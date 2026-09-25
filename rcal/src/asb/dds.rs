//! dust_dds-backed Abstract Service Bus implementation.

use slog::{Logger, trace};
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dust_dds::dds_async::data_reader_listener::DataReaderListener;
use dust_dds::dds_async::data_writer_listener::DataWriterListener;
use dust_dds::dds_async::domain_participant::DomainParticipantAsync;
use dust_dds::dds_async::domain_participant_factory::DomainParticipantFactoryAsync;
use dust_dds::dds_async::domain_participant_listener::DomainParticipantListener;
use dust_dds::dds_async::publisher::PublisherAsync;
use dust_dds::dds_async::publisher_listener::PublisherListener;
use dust_dds::dds_async::subscriber::SubscriberAsync;
use dust_dds::dds_async::subscriber_listener::SubscriberListener;
use dust_dds::dds_async::topic::TopicAsync;
use dust_dds::dds_async::topic_listener::TopicListener;
use dust_dds::infrastructure::qos::{DataReaderQos, DataWriterQos, QosKind};
use dust_dds::infrastructure::qos_policy::{
    HistoryQosPolicy, HistoryQosPolicyKind, LifespanQosPolicy, ReliabilityQosPolicy,
    ReliabilityQosPolicyKind, TimeBasedFilterQosPolicy,
};
use dust_dds::infrastructure::sample_info::{ANY_VIEW_STATE, InstanceStateKind, SampleStateKind};
use dust_dds::infrastructure::time::DurationKind;
use dust_dds::infrastructure::type_support::DdsType;

use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};
use crate::cal::{
    AbstractCal, AbstractReader, AbstractWriter, MessageHeaderDefaults, MessageListener,
    Reliability, TopicQos,
};
use crate::calconfig::{CalConfig, Transport};
use crate::externalizer::{Externalizer, build_externalizer, read_from_bytes, write_to_bytes};
use crate::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalMessage, CalResult};
use serde::Deserialize as _;
use serde::de::IntoDeserializer;

/// ASB identifier string for the DDS transport.
pub const DDS_ASB_ID: &str = "dds";

// ════════════════════════════════════════════════════════════════════════════
// DdsBytePayload — single opaque-bytes DDS type for all UCI messages
// ════════════════════════════════════════════════════════════════════════════

/// Opaque byte payload carrying serialized UCI messages over DDS topics.
///
/// Topic name acts as the routing discriminator — each CAL topic maps to one
/// DDS topic with this type, regardless of the UCI message type carried.
#[derive(DdsType, Debug, Clone)]
struct DdsBytePayload {
    data: Vec<u8>,
}

// ════════════════════════════════════════════════════════════════════════════
// DdsAsb
// ════════════════════════════════════════════════════════════════════════════

/// dust_dds-backed CAL instance.
///
/// Uses a single `DomainParticipant` per instance (one DDS domain ID).
/// Each `create_writer` / `create_reader` call creates a dedicated
/// `DataWriter` / `DataReader` for the given topic with appropriate QoS.
///
/// Unlike ZMQ RADIO/DISH, DDS supports `Reliability::Reliable` (RTPS retransmission).
pub struct DdsAsb {
    asb_id: String,
    service_name: String,
    status: AsbStatus,
    logger: Logger,
    config: Arc<CalConfig>,
    externalizer_name: String,
    listeners: Vec<Arc<dyn AsbStatusListener>>,
    /// Signals all reader tasks to stop on close() (CAL-016049).
    shutdown_tx: Arc<tokio::sync::watch::Sender<()>>,
    participant: DomainParticipantAsync,
    publisher: PublisherAsync,
    subscriber: SubscriberAsync,
    /// Cache: DDS topics are participant-scoped; re-creating the same name errors.
    topics: Mutex<HashMap<String, TopicAsync>>,
}

#[rcal_macros::rcal_trace]
impl DdsAsb {
    /// Constructs a new `DdsAsb` in the `Normal` state.
    ///
    /// `tconfig.uri` is parsed as an `i32` DDS domain ID (e.g. `"0"`).
    /// Defaults to domain 0 on parse failure.
    pub async fn new(
        service_name: impl Into<String>,
        asb_id: impl Into<String>,
        logger: Logger,
        config: Arc<CalConfig>,
        tconfig: &Transport,
    ) -> CalResult<Self> {
        let domain_id: i32 = tconfig.uri.parse().unwrap_or(0);

        let factory = DomainParticipantFactoryAsync::get_instance();
        let participant = factory
            .create_participant(domain_id, QosKind::Default, None::<NoOpDpListener>, &[])
            .await
            .map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("DDS create_participant(domain={domain_id}) failed: {e:?}"),
                )
            })?;

        let publisher = participant
            .create_publisher(QosKind::Default, None::<NoOpPubListener>, &[])
            .await
            .map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("DDS create_publisher failed: {e:?}"),
                )
            })?;

        let subscriber = participant
            .create_subscriber(QosKind::Default, None::<NoOpSubListener>, &[])
            .await
            .map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("DDS create_subscriber failed: {e:?}"),
                )
            })?;

        let (shutdown_tx, _) = tokio::sync::watch::channel(());
        let logger = logger.new(slog::o!("subsystem" => "dds"));

        Ok(Self {
            service_name: service_name.into(),
            asb_id: asb_id.into(),
            status: AsbStatus::new(AsbConnectionState::Normal, "DDS ASB connected"),
            logger,
            config,
            externalizer_name: tconfig
                .externalizer
                .clone()
                .unwrap_or_else(|| "xml".to_string()),
            listeners: Vec::new(),
            shutdown_tx: Arc::new(shutdown_tx),
            participant,
            publisher,
            subscriber,
            topics: Mutex::new(HashMap::new()),
        })
    }

    /// Transitions to `state`, updates description, notifies listeners.
    pub fn update_status(
        &mut self,
        state: AsbConnectionState,
        description: impl Into<String>,
    ) -> CalResult<()> {
        self.status.state.validate_transition(state)?;
        self.status.state = state;
        self.status.description = description.into();
        for listener in &self.listeners {
            listener.on_status_change(&self.status);
        }
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// No-op listener stubs — all trait methods have default impls
// ════════════════════════════════════════════════════════════════════════════

struct NoOpDpListener;
impl DomainParticipantListener for NoOpDpListener {}

struct NoOpPubListener;
impl PublisherListener for NoOpPubListener {}

struct NoOpSubListener;
impl SubscriberListener for NoOpSubListener {}

// ════════════════════════════════════════════════════════════════════════════
// AbstractServiceBus implementation
// ════════════════════════════════════════════════════════════════════════════

#[rcal_macros::rcal_trace]
impl AbstractServiceBus for DdsAsb {
    fn get_logger(&self) -> &Logger {
        &self.logger
    }

    fn service_identifier(&self) -> &str {
        &self.service_name
    }

    fn asb_identifier(&self) -> &str {
        &self.asb_id
    }

    fn get_system_uuid(&self) -> crate::uci::base::UUID {
        self.config.system.uuid
    }

    fn get_service_uuid(&self) -> Option<crate::uci::base::UUID> {
        self.config
            .get_service(&self.service_name)
            .and_then(|s| s.uuid)
    }

    fn get_subsystem_uuid(&self) -> Option<crate::uci::base::UUID> {
        self.config
            .get_service(&self.service_name)
            .and_then(|s| s.subsystem_uuid)
    }

    fn get_component_uuid(&self, name: &str) -> Option<crate::uci::base::UUID> {
        self.config
            .get_service(&self.service_name)
            .and_then(|s| s.get_component_uuid(name))
    }

    fn get_capability_uuid(&self, name: &str) -> Option<crate::uci::base::UUID> {
        self.config
            .get_service(&self.service_name)
            .and_then(|s| s.get_capability_uuid(name))
    }

    fn oms_schema_version(&self) -> &str {
        env!("RCAL_SCHEMA_VERSION")
    }

    fn oms_schema_compiler_version(&self) -> &str {
        env!("RCAL_OMS_COMPILER_VERSION")
    }

    fn get_system_label(&self) -> Option<&str> {
        Some(self.config.system.id.as_str())
    }

    fn get_asb_connection_version(&self) -> &str {
        env!("RCAL_ASB_CONNECTION_VERSION")
    }

    fn get_oms_api_version(&self) -> &str {
        env!("RCAL_OMS_API_VERSION")
    }

    fn connection_status(&self) -> &AsbStatus {
        &self.status
    }

    fn register_status_listener(&mut self, listener: Arc<dyn AsbStatusListener>) -> CalResult<()> {
        trace!(self.logger, "DdsAsb::register_status_listener()");
        if !self.status.state.allows_add_listener() {
            return Err(CalError::new(
                CalErrorKind::InvalidState {
                    current: self.status.state,
                },
                "Cannot register listener in Failed state (CAL-016366).",
            ));
        }
        if self.listeners.iter().any(|l| Arc::ptr_eq(l, &listener)) {
            return Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError,
                "Status listener is already registered.",
            ));
        }
        listener.on_status_change(&self.status);
        self.listeners.push(listener);
        Ok(())
    }

    fn unregister_status_listener(
        &mut self,
        listener: &Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
        trace!(self.logger, "DdsAsb::unregister_status_listener()");
        if let Some(index) = self.listeners.iter().position(|l| Arc::ptr_eq(l, listener)) {
            self.listeners.swap_remove(index);
            Ok(())
        } else {
            Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError,
                "Status listener is not registered.",
            ))
        }
    }

    fn close(&mut self) -> CalResult<()> {
        trace!(self.logger, "DdsAsb::close()");
        // Signal all spawned reader tasks to stop.
        // DDS entity cleanup happens when the participant is dropped via the factory
        // worker; explicitly calling delete_contained_entities on the shared static
        // factory singleton corrupts its state for subsequent participants.
        let _ = self.shutdown_tx.send(());
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCal implementation
// ════════════════════════════════════════════════════════════════════════════

#[rcal_macros::rcal_trace]
impl AbstractCal for DdsAsb {
    fn message_header_defaults(&self) -> MessageHeaderDefaults {
        use crate::uci::types::{
            ClassificationEnum, MessageModeEnum, OwnerProducerChoiceType_, OwnerProducerEnum,
        };
        let sys = &self.config.system;
        let service_id = self
            .config
            .get_service(&self.service_name)
            .and_then(|svc| svc.uuid);
        let mission_id = sys.mission_id;
        let mode = match sys.mode.as_deref() {
            Some("EXERCISE") => MessageModeEnum::Exercise,
            Some("SIMULATION") => MessageModeEnum::Simulation,
            Some("NONEXERCISE_SIMULATION") => MessageModeEnum::Nonexercise_simulation,
            _ => MessageModeEnum::Live,
        };
        let classification = match sys.classification.as_deref() {
            Some("R") => ClassificationEnum::R,
            Some("C") => ClassificationEnum::C,
            Some("S") => ClassificationEnum::S,
            Some("TS") => ClassificationEnum::Ts,
            _ => ClassificationEnum::U,
        };
        let owner_producer: Vec<_> = sys
            .owner_producer
            .iter()
            .map(|s| {
                let de: serde::de::value::StrDeserializer<serde::de::value::Error> =
                    s.as_str().into_deserializer();
                let inner = OwnerProducerEnum::deserialize(de).unwrap_or(OwnerProducerEnum::Usa);
                OwnerProducerChoiceType_::GovernmentIdentifier { inner }
            })
            .collect();
        let owner_producer = if owner_producer.is_empty() {
            vec![OwnerProducerChoiceType_::GovernmentIdentifier {
                inner: OwnerProducerEnum::Usa,
            }]
        } else {
            owner_producer
        };
        MessageHeaderDefaults {
            system_id: sys.uuid,
            system_name: Some(self.config.system.id.clone()),
            service_id,
            service_name: Some(self.service_name.clone()),
            mission_id,
            schema_version: self.oms_schema_version().to_string(),
            mode,
            classification,
            owner_producer,
        }
    }

    fn create_writer<M: CalMessage>(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractWriter<M>>> {
        use super::{apply_config_qos, resolve_topic, validate_topic_direction, validate_topic_type};

        validate_topic_type::<M>(&self.config, &self.service_name, topic)?;
        validate_topic_direction(&self.config, &self.service_name, topic, true)?;
        let qos = apply_config_qos(&self.config, &self.service_name, topic, qos);
        let cal_topic = resolve_topic(&self.config, &self.service_name, topic);

        let reliability_kind = match qos.reliability {
            Reliability::Reliable => ReliabilityQosPolicyKind::Reliable,
            Reliability::BestEffort => ReliabilityQosPolicyKind::BestEffort,
        };
        let writer_history = match qos.writer_buffer {
            Some(ref b) => HistoryQosPolicyKind::KeepLast(b.max_messages as u32),
            None => HistoryQosPolicyKind::KeepAll,
        };
        let mut writer_qos = DataWriterQos {
            reliability: ReliabilityQosPolicy {
                kind: reliability_kind,
                max_blocking_time: DurationKind::Finite(Duration::from_millis(100).into()),
            },
            history: HistoryQosPolicy {
                kind: writer_history,
            },
            ..Default::default()
        };
        if let Some(exp) = qos.expiration {
            writer_qos.lifespan = LifespanQosPolicy {
                duration: DurationKind::Finite(exp.max_age.into()),
            };
        }

        let topic_name = cal_topic.to_string();
        let participant = self.participant.clone();
        let publisher = self.publisher.clone();
        // Look up existing topic before block_in_place to avoid "already exists" error.
        let existing_topic = self.topics.lock().unwrap().get(&topic_name).cloned();

        let (dds_topic, datawriter) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let dds_topic = match existing_topic {
                    Some(t) => t,
                    None => participant
                        .create_topic::<DdsBytePayload>(
                            &topic_name,
                            "DdsBytePayload",
                            QosKind::Default,
                            None::<NoOpTopicListener>,
                            &[],
                        )
                        .await
                        .map_err(|e| {
                            CalError::new(
                                CalErrorKind::TopicUnavailable,
                                format!("DDS create_topic({topic_name}) failed: {e:?}"),
                            )
                        })?,
                };
                let datawriter = publisher
                    .create_datawriter::<DdsBytePayload>(
                        &dds_topic,
                        QosKind::Specific(writer_qos),
                        None::<NoOpDwListener<DdsBytePayload>>,
                        &[],
                    )
                    .await
                    .map_err(|e| {
                        CalError::new(
                            CalErrorKind::TopicUnavailable,
                            format!("DDS create_datawriter({topic_name}) failed: {e:?}"),
                        )
                    })?;
                Ok::<_, CalError>((dds_topic, datawriter))
            })
        })?;
        self.topics
            .lock()
            .unwrap()
            .entry(topic_name.clone())
            .or_insert(dds_topic);

        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<DdsBytePayload>();
        tokio::spawn(async move {
            while let Some(payload) = write_rx.recv().await {
                let _ = datawriter.write(payload, None).await;
            }
        });

        let externalizer: Arc<dyn Externalizer> =
            Arc::from(build_externalizer(&self.externalizer_name, &self.config)?);
        trace!(self.logger, "DdsAsb::create_writer()"; "topic" => cal_topic);
        Ok(Box::new(DdsWriter {
            topic: cal_topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => cal_topic.to_string())),
            externalizer,
            write_tx,
            _phantom: PhantomData,
        }))
    }

    fn create_reader<M: CalMessage>(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>> {
        use super::{apply_config_qos, resolve_topic, validate_topic_direction, validate_topic_type};

        validate_topic_type::<M>(&self.config, &self.service_name, topic)?;
        validate_topic_direction(&self.config, &self.service_name, topic, false)?;
        let qos = apply_config_qos(&self.config, &self.service_name, topic, qos);
        let cal_topic = resolve_topic(&self.config, &self.service_name, topic);

        let reliability_kind = match qos.reliability {
            Reliability::Reliable => ReliabilityQosPolicyKind::Reliable,
            Reliability::BestEffort => ReliabilityQosPolicyKind::BestEffort,
        };
        let (reader_history, reader_max) = match qos.reader_buffer {
            Some(ref b) => (
                HistoryQosPolicyKind::KeepLast(b.max_messages as u32),
                Some(b.max_messages),
            ),
            None => (HistoryQosPolicyKind::KeepAll, None),
        };
        let mut reader_qos = DataReaderQos {
            reliability: ReliabilityQosPolicy {
                kind: reliability_kind,
                max_blocking_time: DurationKind::Finite(Duration::from_millis(100).into()),
            },
            history: HistoryQosPolicy {
                kind: reader_history,
            },
            ..Default::default()
        };
        if let Some(ref f) = qos.time_based_filter {
            reader_qos.time_based_filter = TimeBasedFilterQosPolicy {
                minimum_separation: DurationKind::Finite(f.min_separation.into()),
            };
        }

        let topic_name = cal_topic.to_string();
        let participant = self.participant.clone();
        let subscriber = self.subscriber.clone();
        let existing_topic = self.topics.lock().unwrap().get(&topic_name).cloned();

        let (dds_topic, datareader) = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let dds_topic = match existing_topic {
                    Some(t) => t,
                    None => participant
                        .create_topic::<DdsBytePayload>(
                            &topic_name,
                            "DdsBytePayload",
                            QosKind::Default,
                            None::<NoOpTopicListener>,
                            &[],
                        )
                        .await
                        .map_err(|e| {
                            CalError::new(
                                CalErrorKind::TopicUnavailable,
                                format!("DDS create_topic({topic_name}) failed: {e:?}"),
                            )
                        })?,
                };
                let datareader = subscriber
                    .create_datareader::<DdsBytePayload>(
                        &dds_topic,
                        QosKind::Specific(reader_qos),
                        None::<NoOpDrListener<DdsBytePayload>>,
                        &[],
                    )
                    .await
                    .map_err(|e| {
                        CalError::new(
                            CalErrorKind::TopicUnavailable,
                            format!("DDS create_datareader({topic_name}) failed: {e:?}"),
                        )
                    })?;
                Ok::<_, CalError>((dds_topic, datareader))
            })
        })?;
        self.topics
            .lock()
            .unwrap()
            .entry(topic_name.clone())
            .or_insert(dds_topic);

        let time_filter = qos.time_based_filter;
        let expiration_dur = qos.expiration.map(|e| e.max_age);
        let poll_state: PollState<M> = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let task_alive = Arc::new(AtomicBool::new(true));
        let listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        let listeners_task = Arc::clone(&listeners);
        let poll_state_task = Arc::clone(&poll_state);
        let task_alive_task = Arc::clone(&task_alive);
        let externalizer: Arc<dyn Externalizer> =
            Arc::from(build_externalizer(&self.externalizer_name, &self.config)?);
        let ext_task = Arc::clone(&externalizer);
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let reader_logger = self.logger.new(slog::o!("topic" => cal_topic.to_string()));

        let task = tokio::spawn(async move {
            let mut last_accepted: Option<Instant> = None;
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => break,
                    // ponytail: 1ms poll loop; upgrade to DDS WaitSet/StatusCondition when latency matters
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {
                        let samples = match datareader
                            .take(
                                i32::MAX,
                                &[SampleStateKind::NotRead],
                                ANY_VIEW_STATE,
                                &[InstanceStateKind::Alive],
                            )
                            .await
                        {
                            Ok(s) => s,
                            // NoData is transient — no samples available yet; keep polling
                            Err(dust_dds::infrastructure::error::DdsError::NoData) => continue,
                            Err(e) => {
                                slog::error!(reader_logger, "DDS take() failed"; "error" => %e);
                                break;
                            }
                        };
                        for sample in samples {
                            let payload = match sample.data {
                                Some(p) => p,
                                None => continue,
                            };
                            let m = match read_from_bytes::<M>(ext_task.as_ref(), &payload.data) {
                                Ok(m) => m,
                                Err(e) => {
                                    slog::warn!(reader_logger, "deserialize failed"; "error" => %e);
                                    continue;
                                }
                            };
                            // TimeBasedFilter: drop messages within min_separation (CAL-005431)
                            if let Some(ref f) = time_filter
                                && let Some(last) = last_accepted
                                && last.elapsed() < f.min_separation
                            {
                                continue;
                            }
                            last_accepted = Some(Instant::now());

                            let arc_m = Arc::new(m);
                            let ls = listeners_task.lock().unwrap();
                            if ls.is_empty() {
                                let (lock, cvar) = &*poll_state_task;
                                let mut queue = lock.lock().unwrap();
                                if let Some(max) = reader_max {
                                    while queue.len() >= max {
                                        queue.pop_front();
                                    }
                                }
                                queue.push_back((Instant::now(), Arc::clone(&arc_m)));
                                cvar.notify_one();
                            } else {
                                for l in ls.iter() {
                                    l.on_message(&arc_m);
                                }
                            }
                        }
                    }
                }
            }
            task_alive_task.store(false, Ordering::Release);
            poll_state_task.1.notify_all();
        });

        trace!(self.logger, "DdsAsb::create_reader()"; "topic" => topic);
        Ok(Box::new(DdsReader {
            topic: topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => topic.to_string())),
            externalizer,
            listeners,
            poll_state,
            task_alive,
            expiration: expiration_dur,
            task,
        }))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Additional no-op listener stubs
// ════════════════════════════════════════════════════════════════════════════

struct NoOpTopicListener;
impl TopicListener for NoOpTopicListener {}

struct NoOpDwListener<T>(PhantomData<T>);
impl<T: Send + 'static> DataWriterListener<T> for NoOpDwListener<T> {}

struct NoOpDrListener<T>(PhantomData<T>);
impl<T: Send + 'static> DataReaderListener<T> for NoOpDrListener<T> {}

// ════════════════════════════════════════════════════════════════════════════
// DdsWriter
// ════════════════════════════════════════════════════════════════════════════

type PollState<M> = Arc<(Mutex<VecDeque<(Instant, Arc<M>)>>, Condvar)>;

/// DDS-backed [`AbstractWriter`]: serializes messages and sends via background DataWriter task.
pub struct DdsWriter<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    write_tx: tokio::sync::mpsc::UnboundedSender<DdsBytePayload>,
    _phantom: PhantomData<M>,
}

#[rcal_macros::rcal_trace]
impl<M: CalMessage + serde::Serialize> AbstractWriter<M> for DdsWriter<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn write(&mut self, message: &M) -> CalResult<()> {
        trace!(self.logger, "DdsWriter::write()"; "topic" => &self.topic);
        message.is_valid().map_err(|e| {
            CalError::new(
                CalErrorKind::ValidationError(e),
                "message failed schema validation",
            )
        })?;
        let bytes = write_to_bytes(self.externalizer.as_ref(), message, &self.topic)?;
        let payload = DdsBytePayload { data: bytes };

        self.write_tx
            .send(payload)
            .map_err(|_| CalError::new(CalErrorKind::AsbFailed, "DDS write channel closed"))
    }

    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, "DdsWriter::close()"; "topic" => &self.topic);
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// DdsReader
// ════════════════════════════════════════════════════════════════════════════

/// DDS-backed [`AbstractReader`]: receives via background DataReader poll task,
/// dispatching to listeners or buffering for polling.
///
/// Callback and polling modes are mutually exclusive (CAL-016050).
pub struct DdsReader<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>>,
    poll_state: PollState<M>,
    task_alive: Arc<AtomicBool>,
    expiration: Option<Duration>,
    task: tokio::task::JoinHandle<()>,
}

#[rcal_macros::rcal_trace]
impl<M: CalMessage + serde::de::DeserializeOwned> AbstractReader<M> for DdsReader<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn add_listener(&mut self, listener: Arc<dyn MessageListener<M>>) -> CalResult<()> {
        self.listeners.lock().unwrap().push(listener);
        Ok(())
    }

    fn remove_listener(&mut self, listener: &Arc<dyn MessageListener<M>>) -> CalResult<()> {
        let mut ls = self.listeners.lock().unwrap();
        if let Some(i) = ls.iter().position(|l| Arc::ptr_eq(l, listener)) {
            ls.swap_remove(i);
            Ok(())
        } else {
            Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError,
                "listener not registered",
            ))
        }
    }

    fn read(&mut self, timeout: Option<Duration>) -> CalResult<Option<Arc<M>>> {
        trace!(self.logger, "DdsReader::read()"; "topic" => &self.topic, "timeout_ms" => timeout.map(|d| d.as_millis()));
        if !self.listeners.lock().unwrap().is_empty() {
            return Err(CalError::new(
                CalErrorKind::OperationNotPermitted,
                "polling is not permitted while listeners are registered (CAL-016050)",
            ));
        }
        let (lock, cvar) = &*self.poll_state;
        let deadline = timeout.map(|d| Instant::now() + d);
        let mut queue = lock.lock().unwrap();
        loop {
            if let Some(max_age) = self.expiration {
                while queue.front().is_some_and(|(t, _)| t.elapsed() > max_age) {
                    queue.pop_front();
                }
            }
            if let Some((_, msg)) = queue.pop_front() {
                return Ok(Some(msg));
            }
            if !self.task_alive.load(Ordering::Acquire) {
                return Err(CalError::new(
                    CalErrorKind::AsbFailed,
                    "reader task has stopped",
                ));
            }
            let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
            match remaining {
                Some(r) if r.is_zero() => return Ok(None),
                Some(r) => {
                    let (q, result) = cvar.wait_timeout(queue, r).unwrap();
                    queue = q;
                    if result.timed_out() {
                        return Ok(None);
                    }
                }
                None => {
                    queue = cvar.wait(queue).unwrap();
                }
            }
        }
    }

    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>> {
        trace!(self.logger, "DdsReader::read_no_wait()"; "topic" => &self.topic);
        if !self.listeners.lock().unwrap().is_empty() {
            return Err(CalError::new(
                CalErrorKind::OperationNotPermitted,
                "polling is not permitted while listeners are registered (CAL-016050)",
            ));
        }
        let (lock, _) = &*self.poll_state;
        let mut queue = lock.lock().unwrap();
        if let Some(max_age) = self.expiration {
            while queue.front().is_some_and(|(t, _)| t.elapsed() > max_age) {
                queue.pop_front();
            }
        }
        Ok(queue.pop_front().map(|(_, m)| m))
    }

    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, "DdsReader::close()"; "topic" => &self.topic);
        self.task.abort();
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Test helpers
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
pub(crate) fn test_config_domain(domain_id: u32) -> Arc<CalConfig> {
    use crate::calconfig;
    use crate::uci::base::UUID;
    let ns = UUID::parse_str("6ef79d81-8a79-4750-9c6a-e5e50a30f81b").unwrap();
    let sys_uuid = UUID::generate_v3(&ns, domain_id.to_string().as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"D\"\n\
         \n[[transport]]\nid = \"D\"\ntype = \"dds\"\nuri = \"{domain_id}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}
