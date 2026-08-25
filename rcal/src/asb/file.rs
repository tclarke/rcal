//! A file based asb.
//!
//! Can be used to log message traffic to a file or to replay an existing log.

#![allow(dead_code)]

use slog::{Logger, error, trace, debug};
use std::collections::VecDeque;
use std::fs::File;
use std::marker::PhantomData;
use std::io::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};
use crate::cal::{
    AbstractCal, AbstractCalExt, AbstractReader, AbstractWriter,
    MessageHeaderDefaults, MessageListener, TopicQos,
};
use crate::calconfig::{CalConfig, Transport};
use crate::externalizer::{Externalizer, build_externalizer, write_to_bytes};
use crate::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalMessage, CalResult};
use serde::Deserialize as _;
use serde::de::IntoDeserializer;

/// ASB identifier string for the ZeroMQ-compatible transport.
pub const FILE_ASB_ID: &str = "file";

/// Validates that message type `M` matches the topic's registered type in
/// the service config, if one is configured. No-op when the service or topic
/// is not configured (CAL-005208).
fn validate_topic_type<M: CalMessage>(
    config: &crate::calconfig::CalConfig,
    service_id: &str,
    topic: &str,
) -> CalResult<()> {
    let Some(service) = config.get_service(service_id) else {
        return Ok(());
    };
    let Some(topic_cfg) = service.topic.iter().find(|t| t.id == topic) else {
        return Ok(());
    };
    let Some(registered_type) = &topic_cfg.type_ else {
        return Ok(());
    };
    // Config type= must match QName::display: bare local name for the default UCI
    // namespace (e.g. "SystemStatusType"), prefix:local for mapped namespaces.
    if M::message_type_name() != registered_type.as_str() {
        return Err(CalError::new(
            CalErrorKind::TopicUnavailable,
            format!(
                "Topic '{}' is registered for type '{}' but got '{}' (CAL-005208)",
                topic,
                registered_type,
                M::message_type_name()
            ),
        ));
    }
    Ok(())
}

/// Returns the remapped CAL topic name for `topic` if the service config defines
/// a mapping (CAL-005209), otherwise returns `topic` unchanged.
fn resolve_topic<'a>(config: &'a CalConfig, service_id: &str, topic: &'a str) -> &'a str {
    config
        .get_service(service_id)
        .and_then(|s| s.topic.iter().find(|t| t.id == topic))
        .and_then(|t| t.topic.as_deref())
        .unwrap_or(topic)
}

// ════════════════════════════════════════════════════════════════════════════
// FileAsb
// ════════════════════════════════════════════════════════════════════════════

/// Cal instance which reads and write to files.
///
/// Implements [`AbstractServiceBus`] using files
pub struct FileAsb {
    asb_id: String,
    service_name: String,

    /// Sender side of the writer background task's work queue.
    /// Set to `None` by [`close()`].
    write_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,

    /// Current ASB connection status.
    status: AsbStatus,

    logger: Logger,

    /// Shared, read-only CAL configuration.
    config: Arc<CalConfig>,

    /// Externalizer name for messages on this transport (references `CalConfig::externalizer`).
    externalizer_name: String,

    /// Registered connection-status listeners.
    listeners: Vec<Arc<dyn AsbStatusListener>>,

    /// Signals all reader tasks to stop; sent on close() (CAL-016049).
    shutdown_tx: Arc<tokio::sync::watch::Sender<()>>,
}

impl FileAsb {
    /// Constructs a new `FileAsb` in the `Initializing` state.
    ///
    pub async fn new(
        service_name: impl Into<String>,
        asb_id: impl Into<String>,
        logger: Logger,
        config: Arc<CalConfig>,
        tconfig: &Transport,
    ) -> CalResult<Self> {
        let logger = logger.new(slog::o!("subsystem" => "fileasb"));
        let transport_uri = tconfig.uri.clone();

        let (shutdown_tx, _) = tokio::sync::watch::channel(()); // initial receiver dropped; readers call subscribe()
        let shutdown_tx = Arc::new(shutdown_tx);

        debug!(logger, "Opening {} for write", transport_uri);
        let fpath = Path::new(&transport_uri);
        let mut output_file = File::create(fpath)
            .map_err(|e| {
                CalError::with_source(
                    CalErrorKind::InitializationFailure,
                    "Unable to open output file",
                    e
                )
            })?;

        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let logger2 = logger.clone();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                // write to file
                if let Err(err) = output_file.write_all(msg.as_ref()) {
                    error!(logger2, "Unable to write message to file: {}", err);
                };
            }
            // close file
            drop(output_file);
        });


        Ok(Self {
            service_name: service_name.into(),
            asb_id: asb_id.into(),
            write_tx: Some(write_tx),
            status: AsbStatus::new(AsbConnectionState::Normal, "File ASB connected"),
            logger,
            config,
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

impl AbstractServiceBus for FileAsb {
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
        self.config.system.label.as_deref()
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

    #[rcal_macros::rcal_trace]
    fn register_status_listener(&mut self, listener: Arc<dyn AsbStatusListener>) -> CalResult<()> {
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

    #[rcal_macros::rcal_trace]
    fn unregister_status_listener(
        &mut self,
        listener: &Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
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

    #[rcal_macros::rcal_trace]
    fn close(&mut self) -> CalResult<()> {
        // Signal all reader tasks to unblock any pending dish.recv() (CAL-016049).
        let _ = self.shutdown_tx.send(());
        // Dropping write_tx closes the channel; the background writer task exits.
        self.write_tx = None;
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCal implementation
// ════════════════════════════════════════════════════════════════════════════

impl AbstractCal for FileAsb {
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
            service_id,
            mission_id,
            schema_version: self.oms_schema_version().to_string(),
            mode,
            classification,
            owner_producer,
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// FileWriter
// ════════════════════════════════════════════════════════════════════════════

/// Poll-mode queue shared between the receive task and [`FileReader`].
type PollState<M> = Arc<(Mutex<VecDeque<(Instant, Arc<M>)>>, Condvar)>;

/// file-backed [`AbstractWriter`]: serializes messages to a file
pub struct FileWriter<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    direct_tx: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>,
    writer_buf: Option<Arc<Mutex<VecDeque<Vec<u8>>>>>,
    writer_notify: Option<Arc<tokio::sync::Notify>>,
    writer_max: Option<usize>,
    writer_task: Option<tokio::task::JoinHandle<()>>,
    _phantom: PhantomData<M>,
}

impl<M: CalMessage + serde::Serialize> AbstractWriter<M> for FileWriter<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    #[rcal_macros::rcal_trace]
    fn write(&mut self, message: &M) -> CalResult<()> {
        trace!(self.logger, ""; "topic" => &self.topic);
        message.is_valid().map_err(|e| {
            crate::uci::CalError::new(
                crate::uci::CalErrorKind::ValidationError(e),
                "message failed schema validation",
            )
        })?;
        let msg = write_to_bytes(self.externalizer.as_ref(), message, &self.topic)?;
        match &self.writer_buf {
            Some(buf) => {
                let max = self.writer_max.unwrap();
                let mut q = buf.lock().unwrap();
                while q.len() >= max {
                    q.pop_front(); // drop oldest (CAL-005445)
                }
                q.push_back(msg);
                drop(q);
                self.writer_notify.as_ref().unwrap().notify_one();
                Ok(())
            }
            None => self
                .direct_tx
                .as_ref()
                .unwrap()
                .send(msg)
                .map_err(|_| CalError::new(CalErrorKind::AsbFailed, "ASB write channel closed")),
        }
    }

    #[rcal_macros::rcal_trace]
    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, ""; "topic" => &self.topic);
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
// FileReader
// ════════════════════════════════════════════════════════════════════════════

/// file-backed [`AbstractReader`]: reads a message per line in a background
/// task, dispatching to listeners or buffering for polling.
///
/// Callback and polling modes are mutually exclusive (CAL-016050).
pub struct FileReader<M: CalMessage> {
    topic: String,
    logger: Logger,
    externalizer: Arc<dyn Externalizer>,
    listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>>,
    /// Shared queue + condvar for poll-mode delivery with expiration support.
    poll_state: PollState<M>,
    /// Set false by the receive task on exit; wakes any blocked read().
    task_alive: Arc<AtomicBool>,
    expiration: Option<Duration>,
    task: tokio::task::JoinHandle<()>,
}

impl<M: CalMessage + serde::de::DeserializeOwned> AbstractReader<M> for FileReader<M> {
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

    #[rcal_macros::rcal_trace]
    fn read(&mut self, timeout: Option<Duration>) -> CalResult<Option<Arc<M>>> {
        trace!(self.logger, ""; "topic" => &self.topic, "timeout_ms" => timeout.map(|d| d.as_millis()));
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
            // Expire buffered messages older than max_age (CAL-005437)
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

    #[rcal_macros::rcal_trace]
    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>> {
        trace!(self.logger, ""; "topic" => &self.topic);
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

    #[rcal_macros::rcal_trace]
    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, ""; "topic" => &self.topic);
        self.task.abort();
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCalExt implementation
// ════════════════════════════════════════════════════════════════════════════

impl<M> AbstractCalExt<M> for FileAsb
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    #[rcal_macros::rcal_trace]
    fn create_writer(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractWriter<M>>> {
        validate_topic_type::<M>(&self.config, &self.service_name, topic)?;
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
            let buf: Arc<Mutex<VecDeque<Vec<u8>>>> = Arc::new(Mutex::new(VecDeque::new()));
            let notify = Arc::new(tokio::sync::Notify::new());
            let buf_task = Arc::clone(&buf);
            let notify_task = Arc::clone(&notify);
            let task = tokio::spawn(async move {
                loop {
                    notify_task.notified().await;
                    let msgs: Vec<Vec<u8>> = buf_task.lock().unwrap().drain(..).collect();
                    for m in msgs {
                        if tx.send(m).is_err() {
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
        trace!(self.logger, ""; "topic" => cal_topic);
        Ok(Box::new(FileWriter {
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

    fn create_reader(
        &mut self,
        _topic: &str,
        _qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>> {
        Err(CalError::new(CalErrorKind::OperationNotPermitted, "FileAsb only supports writers"))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Test helpers
// ════════════════════════════════════════════════════════════════════════════

/// Builds a [`CalConfig`] with an output file.
#[cfg(test)]
pub(super) fn test_config(filename: &str) -> Arc<CalConfig> {
    use crate::calconfig;
    use crate::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, filename.as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nlabel = \"OMS Test System\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestFile\"\n\n[[transport]]\nid = \"TestFile\"\ntype = \"file\"\nuri = \"{filename}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asb::AbstractServiceBus;
    use rcal_macros::init_test_logger;
    use tempfile::{NamedTempFile, TempPath};
    use std::sync::{
        Mutex,
        atomic::{AtomicI32, Ordering},
    };
    use std::time::Duration;

    async fn make_bus(logger: Logger) -> (FileAsb, TempPath) {
        let tempfile = NamedTempFile::new().expect("Unable to create a temporary output file.").into_temp_path();
        let config = test_config(tempfile.to_str().expect("Invalid filename"));
        let tconfig = config
            .get_transport(&String::from("TestFile"))
            .expect("TestFile transport must exist in test config");
        (FileAsb::new("Test Service", "TestFile", logger, config.clone(), tconfig)
            .await
            .unwrap(), tempfile)
    }

    // ── Test message type ─────────────────────────────────────────────────

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct TestMsg {
        value: String,
    }

    impl CalMessage for TestMsg {
        fn message_type_name() -> crate::QName {
            "test.TestMsg".into()
        }
        fn cal_create() -> Self {
            Self {
                value: String::new(),
            }
        }
    }

    // ── Basic construction ────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_uuid_identity_methods() {
        let (a, _f) = make_bus(logger).await;
        // System UUID is derived from the port in test_config_on_ports.
        assert!(!a.get_system_uuid().is_nil());
        // "Test Service" has no [[service]] entry in the test config → all None.
        assert_eq!(a.get_service_uuid(), None);
        assert_eq!(a.get_subsystem_uuid(), None);
        assert_eq!(a.get_component_uuid("anything"), None);
        assert_eq!(a.get_capability_uuid("anything"), None);
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_check_creation() {
        let (a, _f) = make_bus(logger).await;
        assert_eq!(a.oms_schema_version(), env!("RCAL_SCHEMA_VERSION"));
        assert_eq!(
            a.oms_schema_compiler_version(),
            env!("RCAL_OMS_COMPILER_VERSION")
        );
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), "TestFile");
        assert_eq!(a.connection_status().state, AsbConnectionState::Normal);
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_version_and_label_methods() {
        let (a, _f) = make_bus(logger).await;
        // Version strings are non-empty (defaults to package/version).
        assert!(!a.get_asb_connection_version().is_empty());
        assert!(!a.get_oms_api_version().is_empty());
        // Label comes from calconfig_sample.toml.
        assert_eq!(a.get_system_label(), Some("OMS Test System"));
    }

    #[tokio::test]
    async fn test_file_write() {
    }

    // ── Listener helper ───────────────────────────────────────────────────

    struct TestStatusListener {
        count: AtomicI32,
        last_state: Mutex<AsbConnectionState>,
    }

    impl TestStatusListener {
        fn new() -> Self {
            Self {
                count: AtomicI32::new(0),
                last_state: Mutex::new(AsbConnectionState::Inoperable),
            }
        }

        fn call_count(&self) -> i32 {
            self.count.load(Ordering::SeqCst)
        }

        fn last_state(&self) -> AsbConnectionState {
            *self.last_state.lock().unwrap()
        }
    }

    impl AsbStatusListener for TestStatusListener {
        fn on_status_change(&self, status: &AsbStatus) {
            self.count.fetch_add(1, Ordering::SeqCst);
            *self.last_state.lock().unwrap() = status.state;
        }
    }

    // ── Status-listener tests ─────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_status_listeners() {
        let (mut a, _f) = make_bus(logger).await;

        let l1 = Arc::new(TestStatusListener::new());
        let l2 = Arc::new(TestStatusListener::new());
        let l1d: Arc<dyn AsbStatusListener> = l1.clone();
        let l2d: Arc<dyn AsbStatusListener> = l2.clone();

        assert_eq!(a.listeners.len(), 0);
        assert_eq!(l1.call_count(), 0);
        assert_eq!(l2.call_count(), 0);

        a.register_status_listener(l1d.clone()).unwrap();
        assert_eq!(a.listeners.len(), 1);
        assert_eq!(l1.call_count(), 1, "immediate callback on register");
        assert_eq!(l1.last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.call_count(), 0);

        a.update_status(AsbConnectionState::Degraded, "degraded")
            .unwrap();
        assert_eq!(l1.call_count(), 2);
        assert_eq!(l1.last_state(), AsbConnectionState::Degraded);
        assert_eq!(l2.call_count(), 0);

        a.register_status_listener(l2d.clone()).unwrap();
        assert_eq!(a.listeners.len(), 2);
        assert_eq!(l1.call_count(), 2);
        assert_eq!(l2.call_count(), 1, "immediate callback on register");
        assert_eq!(l2.last_state(), AsbConnectionState::Degraded);

        a.update_status(AsbConnectionState::Normal, "recovered")
            .unwrap();
        assert_eq!(l1.call_count(), 3);
        assert_eq!(l1.last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.call_count(), 2);
        assert_eq!(l2.last_state(), AsbConnectionState::Normal);

        a.unregister_status_listener(&l1d).unwrap();
        assert_eq!(a.listeners.len(), 1);

        a.update_status(AsbConnectionState::Failed, "terminal")
            .unwrap();
        assert_eq!(l1.call_count(), 3, "l1 must not be called after unregister");
        assert_eq!(l1.last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.call_count(), 3);
        assert_eq!(l2.last_state(), AsbConnectionState::Failed);
        drop(a);
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_duplicate_register_returns_err() {
        let (mut a, _f) = make_bus(logger).await;

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        a.register_status_listener(ld.clone()).unwrap();
        let result = a.register_status_listener(ld.clone());
        assert!(
            result.is_err(),
            "registering the same Arc twice must return Err"
        );
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_unregister_unknown_returns_err() {
        let (mut a, _f) = make_bus(logger).await;

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        let result = a.unregister_status_listener(&ld);
        assert!(
            result.is_err(),
            "unregistering an unknown listener must return Err"
        );
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_register_listener_in_failed_state_returns_err() {
        let (mut a, _f) = make_bus(logger).await;
        a.update_status(AsbConnectionState::Failed, "terminal")
            .unwrap();

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        assert!(
            a.register_status_listener(ld).is_err(),
            "register_status_listener in Failed state must return Err (CAL-016366)"
        );
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_invalid_state_transition_returns_err() {
        let (mut a, _f) = make_bus(logger).await;

        a.update_status(AsbConnectionState::Failed, "").unwrap();

        for next in [
            AsbConnectionState::Initializing,
            AsbConnectionState::Normal,
            AsbConnectionState::Degraded,
            AsbConnectionState::Inoperable,
        ] {
            assert!(
                a.update_status(next, "").is_err(),
                "transition Failed → {next:?} must be rejected"
            );
        }
    }

    // ── Writer tests ──────────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_writer_sends_message() {
        let mut tempfile = NamedTempFile::new().expect("Unable to create a temporary output file.");
        let config = test_config(tempfile.path().to_str().expect("Invalid filename"));
        let tconfig = config.get_transport(&String::from("TestFile")).unwrap();
        let mut asb = FileAsb::new("TestSvc", "TestFile", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut writer = <FileAsb as AbstractCalExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        let msg = TestMsg {
            value: "hello".to_string(),
        };
        writer.write(&msg).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        tempfile.rewind().expect("Unable to rewind file");
        let mut buf = Vec::new();
        tempfile.read_to_end(&mut buf).unwrap();
        let xml = std::str::from_utf8(&buf).unwrap();
        assert!(
            xml.contains("hello"),
            "expected XML to contain 'hello', got: {xml}"
        );

        asb.close().unwrap();
    }
}
