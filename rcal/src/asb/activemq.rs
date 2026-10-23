//! tokio-stomp-backed Abstract Service Bus implementation (ActiveMQ / Artemis).

use futures::{SinkExt, StreamExt};
use slog::{Logger, trace};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio_stomp::client;
use tokio_stomp::{AckMode, FromServer, Message, ToServer};

use super::{
    AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener, apply_config_qos,
    resolve_topic, validate_topic_direction, validate_topic_type,
};
use crate::cal::{
    AbstractCal, AbstractReader, AbstractWriter, MessageHeaderDefaults, MessageListener,
    Reliability, TopicQos,
};
use crate::calconfig::{CalConfig, Transport};
use crate::externalizer::{Externalizer, build_externalizer, read_from_bytes, write_to_bytes};
use crate::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalMessage, CalResult};
use serde::Deserialize as _;
use serde::de::IntoDeserializer;

/// ASB identifier string for the ActiveMQ/STOMP transport.
pub const AMQ_ASB_ID: &str = "activemq";

type WriteBuf = Arc<Mutex<VecDeque<(String, Vec<u8>)>>>;

fn parse_broker_addr(uri: &str) -> &str {
    uri.strip_prefix("stomp://").unwrap_or(uri)
}

/// Resolves `addr` (e.g. `"localhost:61613"`) and returns the best socket
/// address as a string, preferring IPv4.
///
/// On Windows + Docker Desktop "localhost" resolves to `::1` first; the broker
/// typically listens on IPv4 only, so the IPv6 attempt times out after ~21 s
/// before falling back. Picking the IPv4 address upfront avoids that delay.
async fn resolve_broker_addr(addr: &str) -> String {
    match tokio::net::lookup_host(addr).await {
        Ok(addrs) => {
            let mut fallback = None;
            for a in addrs {
                if a.is_ipv4() {
                    return a.to_string();
                }
                fallback.get_or_insert(a);
            }
            fallback
                .map(|a| a.to_string())
                .unwrap_or_else(|| addr.to_string())
        }
        Err(_) => addr.to_string(),
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ActivemqAsb
// ════════════════════════════════════════════════════════════════════════════

/// tokio-stomp STOMP-backed CAL instance (ActiveMQ Classic / Artemis).
///
/// A dedicated background task owns the write STOMP connection and drains a
/// channel. [`AmqWriter`]s hold a clone of the channel sender. Each
/// [`AmqReader`] opens its own STOMP connection and subscribes to one topic.
///
/// Unlike the ZMQ transport, `Reliability::Reliable` is supported (CAL-005434,
/// CAL-016076): the reader subscribes with `ack:client-individual` and sends
/// `ACK` after each message; the broker redelivers unacknowledged messages on
/// reconnect.
pub struct ActivemqAsb {
    asb_id: String,
    service_name: String,
    /// Sender to the background write task. `None` after `close()`.
    write_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, Vec<u8>)>>,
    /// Signals when the writer task has fully exited and its TcpStream is closed.
    writer_done: Arc<(Mutex<bool>, Condvar)>,
    status: AsbStatus,
    logger: Logger,
    config: Arc<CalConfig>,
    /// Broker address in `host:port` form (scheme stripped from config URI).
    broker_addr: String,
    externalizer_name: String,
    listeners: Vec<Arc<dyn AsbStatusListener>>,
    /// Signals all reader tasks to stop on `close()` (CAL-016049).
    shutdown_tx: Arc<tokio::sync::watch::Sender<()>>,
}

#[rcal_macros::rcal_trace]
impl ActivemqAsb {
    /// Connects to the broker and starts the background writer task.
    pub async fn new(
        service_name: impl Into<String>,
        asb_id: impl Into<String>,
        logger: Logger,
        config: Arc<CalConfig>,
        tconfig: &Transport,
    ) -> CalResult<Self> {
        let broker_addr = parse_broker_addr(&tconfig.uri).to_string();
        let host = broker_addr
            .split(':')
            .next()
            .unwrap_or(&broker_addr)
            .to_string();

        let resolved = resolve_broker_addr(&broker_addr).await;
        let transport = client::connect(&resolved, &host, None, None)
            .await
            .map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("ActiveMQ connect to '{broker_addr}' failed: {e}"),
                )
            })?;

        let (shutdown_tx, _) = tokio::sync::watch::channel(());
        let shutdown_tx = Arc::new(shutdown_tx);

        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Vec<u8>)>();
        let writer_done = Arc::new((Mutex::new(false), Condvar::new()));
        let writer_done_task = Arc::clone(&writer_done);
        tokio::spawn(async move {
            // Guard declared first → drops last, AFTER sink, ensuring the TcpStream
            // is closed before Drop for ActivemqAsb unblocks (prevents IOCP hang).
            let _writer_done_guard = DoneGuard(writer_done_task);
            let mut sink = transport;
            while let Some((topic, payload)) = write_rx.recv().await {
                let msg = Message {
                    content: ToServer::Send {
                        destination: format!("/topic/{topic}"),
                        transaction: None,
                        body: Some(payload),
                    },
                    extra_headers: vec![],
                };
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            // sink drops here (before _writer_done_guard) → TcpStream closed → IOCP completion posted
        });

        let logger = logger.new(slog::o!("subsystem" => "activemq"));
        Ok(Self {
            service_name: service_name.into(),
            asb_id: asb_id.into(),
            write_tx: Some(write_tx),
            writer_done,
            status: AsbStatus::new(AsbConnectionState::Normal, "ActiveMQ ASB connected"),
            logger,
            config,
            broker_addr,
            externalizer_name: tconfig
                .externalizer
                .clone()
                .unwrap_or_else(|| "xml".to_string()),
            listeners: Vec::new(),
            shutdown_tx,
        })
    }

    /// Transitions to `state`, updates the description, then notifies all
    /// registered listeners.
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
// AbstractServiceBus implementation
// ════════════════════════════════════════════════════════════════════════════

#[rcal_macros::rcal_trace]
impl AbstractServiceBus for ActivemqAsb {
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
        trace!(self.logger, "ActivemqAsb::register_status_listener()");

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
        trace!(self.logger, "ActivemqAsb::unregister_status_listener()");

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
        trace!(self.logger, "ActivemqAsb::close()");
        // Wake all reader tasks (CAL-016049).
        let _ = self.shutdown_tx.send(());
        // Closing write_tx causes the background writer task to exit.
        self.write_tx = None;
        Ok(())
    }
}

impl Drop for ActivemqAsb {
    fn drop(&mut self) {
        // Ensure tasks stop even when close() was never called (e.g. panic unwind).
        let _ = self.shutdown_tx.send(());
        self.write_tx = None;
        // Block until the writer task exits and closes its TcpStream. Without this,
        // the tokio runtime drops before the IOCP read on the writer socket completes,
        // causing a ~40s wait for ActiveMQ's inactivity timeout on Windows.
        let (lock, cvar) = &*self.writer_done;
        let _ =
            cvar.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |done| !*done);
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCal implementation
// ════════════════════════════════════════════════════════════════════════════

#[rcal_macros::rcal_trace]
impl AbstractCal for ActivemqAsb {
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
        validate_topic_type::<M>(&self.config, &self.service_name, topic)?;
        validate_topic_direction(&self.config, &self.service_name, topic, true)?;
        let qos = apply_config_qos(&self.config, &self.service_name, topic, qos);
        let cal_topic = resolve_topic(&self.config, &self.service_name, topic);

        let tx = self
            .write_tx
            .as_ref()
            .ok_or_else(|| {
                CalError::new(
                    CalErrorKind::InvalidState {
                        current: self.status.state,
                    },
                    "ASB is closed",
                )
            })?
            .clone();

        let writer_max = qos.writer_buffer.map(|b| b.max_messages);
        let (direct_tx, writer_buf, writer_notify, writer_task) = if let Some(_max) = writer_max {
            let buf: WriteBuf = Arc::new(Mutex::new(VecDeque::new()));
            let notify = Arc::new(tokio::sync::Notify::new());
            let buf_task = Arc::clone(&buf);
            let notify_task = Arc::clone(&notify);
            let task = tokio::spawn(async move {
                loop {
                    notify_task.notified().await;
                    let msgs: Vec<(String, Vec<u8>)> = buf_task.lock().unwrap().drain(..).collect();
                    for item in msgs {
                        if tx.send(item).is_err() {
                            return;
                        }
                    }
                }
            });
            (None, Some(buf), Some(notify), Some(task))
        } else {
            (Some(tx), None, None, None)
        };

        let externalizer: Arc<dyn Externalizer> =
            Arc::from(build_externalizer(&self.externalizer_name, &self.config)?);
        trace!(self.logger, "ActivemqAsb::create_writer()"; "topic" => cal_topic);
        Ok(Box::new(AmqWriter {
            topic: cal_topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => cal_topic.to_string())),
            externalizer,
            direct_tx,
            writer_buf,
            writer_notify,
            writer_max,
            writer_task,
            _phantom: PhantomData,
        }))
    }

    fn create_reader<M: CalMessage>(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>> {
        validate_topic_type::<M>(&self.config, &self.service_name, topic)?;
        validate_topic_direction(&self.config, &self.service_name, topic, false)?;
        let qos = apply_config_qos(&self.config, &self.service_name, topic, qos);
        let cal_topic = resolve_topic(&self.config, &self.service_name, topic);
        let reliable = qos.reliability == Reliability::Reliable;

        let time_filter = qos.time_based_filter;
        let expiration_dur = qos.expiration.map(|e| e.max_age);
        let reader_max = qos.reader_buffer.map(|b| b.max_messages);

        let poll_state: PollState<M> = Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let task_alive = Arc::new(AtomicBool::new(true));
        let task_done = Arc::new((Mutex::new(false), Condvar::new()));
        let listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>> =
            Arc::new(Mutex::new(Vec::new()));

        let listeners_task = Arc::clone(&listeners);
        let poll_state_task = Arc::clone(&poll_state);
        let task_alive_task = Arc::clone(&task_alive);
        let task_done_task = Arc::clone(&task_done);
        let topic_str = cal_topic.to_string();
        let externalizer: Arc<dyn Externalizer> =
            Arc::from(build_externalizer(&self.externalizer_name, &self.config)?);
        let ext_task = Arc::clone(&externalizer);
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let reader_logger = self.logger.new(slog::o!("topic" => cal_topic.to_string()));
        let broker_addr = self.broker_addr.clone();

        let ack_mode = if reliable {
            Some(AckMode::ClientIndividual)
        } else {
            None // broker uses auto-ack
        };
        let sub = Message {
            content: ToServer::Subscribe {
                destination: format!("/topic/{topic_str}"),
                id: "sub-0".into(),
                ack: ack_mode,
            },
            extra_headers: vec![],
        };

        // Connect and subscribe eagerly before spawning. Inside tokio::spawn the STOMP
        // CONNECT/CONNECTED handshake can hang indefinitely on Windows + Docker Desktop
        // (second concurrent connection stalls). block_in_place parks this worker so the
        // runtime stays live while the handshake completes; requires a multi-thread runtime
        // (always the case for Cal in both tests and production).
        // Subscribing here (not in the task) guarantees the broker routes messages before
        // create_reader returns, eliminating the need for a post-create sleep in tests.
        let host = broker_addr
            .split(':')
            .next()
            .unwrap_or(&broker_addr)
            .to_string();
        let transport = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let resolved = resolve_broker_addr(&broker_addr).await;
                let mut t = client::connect(&resolved, &host, None, None)
                    .await
                    .map_err(|e| {
                        CalError::new(
                            CalErrorKind::InitializationFailure,
                            format!("ActiveMQ reader connect to '{broker_addr}' failed: {e}"),
                        )
                    })?;
                t.send(sub).await.map_err(|_| {
                    CalError::new(
                        CalErrorKind::InitializationFailure,
                        format!("ActiveMQ reader subscribe on '{broker_addr}' failed"),
                    )
                })?;
                Ok::<_, CalError>(t)
            })
        })?;

        let task = tokio::spawn(async move {
            // Drop order (reverse declaration): _cancel_guard first, then transport,
            // then _done_guard last. This ensures CancelIoEx fires before closesocket
            // and the condvar fires after the socket is closed.
            let _done_guard = DoneGuard(task_done_task);
            let mut transport = transport;
            // CancelGuard is declared AFTER transport → drops BEFORE transport.
            // On drop it calls CancelIoEx(socket, NULL), cancelling any pending overlapped
            // IOCP read so closesocket() returns immediately instead of waiting ~60 s for
            // the broker's inactivity timeout. Handles both clean exit and task abort.
            #[cfg(windows)]
            let _cancel_guard = {
                use std::os::windows::io::AsRawSocket;
                CancelGuard(transport.get_ref().as_raw_socket())
            };
            let mut last_accepted: Option<Instant> = None;

            loop {
                let frame = tokio::select! {
                    result = transport.next() => match result {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => { slog::error!(reader_logger, "STOMP frame error"; "error" => %e); break; }
                        None => break,
                    },
                    _ = shutdown_rx.changed() => break,
                };

                let (message_id, payload) = match frame.content {
                    FromServer::Message {
                        message_id, body, ..
                    } => (message_id, body.unwrap_or_default()),
                    _ => continue,
                };

                let m = match read_from_bytes::<M>(ext_task.as_ref(), &payload) {
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

                // ACK for reliable delivery — broker redelivers on unacked disconnect (CAL-005434)
                if reliable {
                    let ack = Message {
                        content: ToServer::Ack {
                            id: message_id,
                            transaction: None,
                        },
                        extra_headers: vec![],
                    };
                    let _ = transport.send(ack).await;
                }

                let arc_m = Arc::new(m);
                let ls = listeners_task.lock().unwrap();
                if ls.is_empty() {
                    // Poll mode: buffer (CAL-016052)
                    let (lock, cvar) = &*poll_state_task;
                    let mut queue = lock.lock().unwrap();
                    if let Some(max) = reader_max {
                        while queue.len() >= max {
                            queue.pop_front(); // drop oldest (CAL-015746)
                        }
                    }
                    queue.push_back((Instant::now(), Arc::clone(&arc_m)));
                    cvar.notify_one();
                } else {
                    // Callback mode (CAL-005392)
                    for l in ls.iter() {
                        l.on_message(&arc_m);
                    }
                }
            }

            task_alive_task.store(false, Ordering::Release);
            poll_state_task.1.notify_all();
            // Drop order on exit (or abort): _cancel_guard fires first (CancelIoEx),
            // then transport drops (closesocket), then _done_guard fires (condvar).
        });

        trace!(self.logger, "ActivemqAsb::create_reader()"; "topic" => topic);
        Ok(Box::new(AmqReader {
            topic: topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => topic.to_string())),
            externalizer,
            listeners,
            poll_state,
            task_alive,
            expiration: expiration_dur,
            task,
            task_done,
        }))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AmqWriter
// ════════════════════════════════════════════════════════════════════════════

type PollState<M> = Arc<(Mutex<VecDeque<(Instant, Arc<M>)>>, Condvar)>;

/// ActiveMQ-backed [`AbstractWriter`]: serializes and forwards via the shared
/// STOMP writer background task.
pub struct AmqWriter<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    direct_tx: Option<tokio::sync::mpsc::UnboundedSender<(String, Vec<u8>)>>,
    writer_buf: Option<WriteBuf>,
    writer_notify: Option<Arc<tokio::sync::Notify>>,
    writer_max: Option<usize>,
    writer_task: Option<tokio::task::JoinHandle<()>>,
    _phantom: PhantomData<M>,
}

#[rcal_macros::rcal_trace]
impl<M: CalMessage + serde::Serialize> AbstractWriter<M> for AmqWriter<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn write(&mut self, message: &M) -> CalResult<()> {
        trace!(self.logger, "AmqWriter::write()"; "topic" => &self.topic);
        message.is_valid().map_err(|e| {
            CalError::new(
                CalErrorKind::ValidationError(e),
                "message failed schema validation",
            )
        })?;
        let payload = write_to_bytes(self.externalizer.as_ref(), message, &self.topic)?;
        let item = (self.topic.clone(), payload);
        match &self.writer_buf {
            Some(buf) => {
                let max = self.writer_max.unwrap();
                let mut q = buf.lock().unwrap();
                while q.len() >= max {
                    q.pop_front(); // drop oldest (CAL-005445)
                }
                q.push_back(item);
                drop(q);
                self.writer_notify.as_ref().unwrap().notify_one();
                Ok(())
            }
            None => self
                .direct_tx
                .as_ref()
                .unwrap()
                .send(item)
                .map_err(|_| CalError::new(CalErrorKind::AsbFailed, "ASB write channel closed")),
        }
    }

    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, "AmqWriter::close()"; "topic" => &self.topic);
        let result = if let Some(buf) = &self.writer_buf {
            let remaining = buf.lock().unwrap().len();
            if remaining > 0 {
                Err(CalError::new(
                    CalErrorKind::AsbFailed,
                    format!("close: {remaining} buffered messages dropped"),
                ))
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };
        if let Some(task) = self.writer_task {
            task.abort();
        }
        result
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AmqReader
// ════════════════════════════════════════════════════════════════════════════

/// RAII guard set inside a background task; fires on both normal return and abort.
/// Ensures the caller can wait for the task's TcpStream to close before the tokio
/// runtime drops.
struct DoneGuard(Arc<(Mutex<bool>, Condvar)>);
impl Drop for DoneGuard {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

/// On Windows, calls `shutdown(SD_RECEIVE)` on drop so any pending overlapped
/// IOCP read completes immediately (MSDN: "terminated immediately … forcing a
/// zero byte read"). Must be declared AFTER `transport` in the task so it drops
/// BEFORE transport (reverse-declaration order), guaranteeing the completion is
/// posted before `closesocket()` runs.
#[cfg(windows)]
struct CancelGuard(std::os::windows::io::RawSocket);

#[cfg(windows)]
impl Drop for CancelGuard {
    fn drop(&mut self) {
        use std::mem::ManuallyDrop;
        use std::os::windows::io::FromRawSocket;
        // SAFETY: we borrow the socket without closing it; ManuallyDrop ensures
        // the std::net::TcpStream wrapper does not call closesocket on drop.
        let sock =
            ManuallyDrop::new(unsafe { std::net::TcpStream::from_raw_socket(self.0) });
        // shutdown(SD_RECEIVE) causes any pending WSARecv overlapped operation
        // to complete immediately with 0 bytes, flushing the IOCP queue entry
        // before closesocket() is called. Errors are harmless (e.g. if the
        // remote already sent FIN or the task exited cleanly without a pending
        // read).
        let _ = sock.shutdown(std::net::Shutdown::Read);
    }
}

/// ActiveMQ-backed [`AbstractReader`]: subscribes on a dedicated STOMP
/// connection in a background task.
///
/// Callback and polling modes are mutually exclusive (CAL-016050).
pub struct AmqReader<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>>,
    poll_state: PollState<M>,
    /// Set `false` by the receive task on exit; wakes any blocked `read()`.
    task_alive: Arc<AtomicBool>,
    expiration: Option<Duration>,
    task: tokio::task::JoinHandle<()>,
    /// Notified when the reader task's future is fully dropped (on exit or abort).
    task_done: Arc<(Mutex<bool>, Condvar)>,
}

impl<M: CalMessage> Drop for AmqReader<M> {
    fn drop(&mut self) {
        self.task.abort();
        // Wait (up to 2s) for the reader task's future to be fully dropped so the
        // STOMP transport is closed before the tokio runtime can shut down.
        let (lock, cvar) = &*self.task_done;
        let _ =
            cvar.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(2), |done| !*done);
    }
}

#[rcal_macros::rcal_trace]
impl<M: CalMessage + serde::de::DeserializeOwned> AbstractReader<M> for AmqReader<M> {
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
        trace!(self.logger, "AmqReader::read()"; "topic" => &self.topic, "timeout_ms" => timeout.map(|d| d.as_millis()));
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
            // Expire messages older than max_age (CAL-005437)
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
        trace!(self.logger, "AmqReader::read_no_wait()"; "topic" => &self.topic);
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
        trace!(self.logger, "AmqReader::close()"; "topic" => &self.topic);
        self.task.abort();
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Test helpers
// ════════════════════════════════════════════════════════════════════════════

/// Builds a [`CalConfig`] pointing at a local ActiveMQ broker on `host:port`.
#[cfg(test)]
pub(crate) fn test_config_activemq(host: &str, port: u16) -> Arc<CalConfig> {
    use crate::calconfig;
    use crate::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, format!("{host}:{port}").as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestAmq\"\n\
         \n[[transport]]\nid = \"TestAmq\"\ntype = \"activemq\"\nuri = \"stomp://{host}:{port}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_broker_addr_strips_scheme() {
        assert_eq!(
            parse_broker_addr("stomp://localhost:61613"),
            "localhost:61613"
        );
        assert_eq!(parse_broker_addr("localhost:61613"), "localhost:61613");
        assert_eq!(
            parse_broker_addr("stomp://192.168.1.1:61616"),
            "192.168.1.1:61616"
        );
    }
}
