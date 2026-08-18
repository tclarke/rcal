//! omq-tokio-backed Abstract Service Bus implementation (RADIO/DISH).

#![allow(dead_code)]

use slog::{Logger, trace};
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omq_tokio::{Endpoint, Message, Options, Socket, SocketType};

use super::{
    AbstractReader, AbstractServiceBus, AbstractServiceBusExt, AbstractWriter, AsbConnectionState,
    AsbStatus, AsbStatusListener, MessageListener, TopicQos,
};
use crate::calconfig::{CalConfig, SerializationFormat, Transport};
use crate::uci::base::{ServiceUuids, UUID};
use crate::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalMessage, CalResult};

/// ASB identifier string for the ZeroMQ-compatible transport.
pub const ZMQ_ASB_ID: &str = "zmq";

// ════════════════════════════════════════════════════════════════════════════
// Serialization helpers
// ════════════════════════════════════════════════════════════════════════════

fn serialize_message<M: serde::Serialize>(
    msg: &M,
    root: &str,
    format: &SerializationFormat,
) -> CalResult<String> {
    match format {
        SerializationFormat::Xml => quick_xml::se::to_string_with_root(root, msg)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string())),
        SerializationFormat::PrettyXml => {
            let mut buf = String::new();
            let mut ser = quick_xml::se::Serializer::with_root(&mut buf, Some(root))
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            ser.indent(' ', 4);
            msg.serialize(ser)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            Ok(buf)
        }
    }
}

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

fn deserialize_message<M: serde::de::DeserializeOwned>(xml: &[u8]) -> CalResult<M> {
    let xml_str = std::str::from_utf8(xml)
        .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
    quick_xml::de::from_str(xml_str)
        .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))
}

// ════════════════════════════════════════════════════════════════════════════
// ZmqAsb
// ════════════════════════════════════════════════════════════════════════════

/// omq-tokio RADIO/DISH-backed CAL instance.
///
/// Implements [`AbstractServiceBus`] using RADIO for broadcast (no broker
/// required for up to ~10 peers). TCP and inproc: RADIO binds, DISH connects.
/// UDP: DISH binds, RADIO connects — callers handle that polarity in tests.
///
/// The RADIO socket is owned by a background tokio task that drains a channel.
/// [`ZmqWriter`]s hold a clone of the channel sender.
pub struct ZmqAsb {
    asb_id: String,
    service_id: String,
    uuids: ServiceUuids,

    /// Sender side of the RADIO background task's work queue.
    /// Set to `None` by [`close()`].
    write_tx: Option<tokio::sync::mpsc::UnboundedSender<Message>>,

    /// Current ASB connection status.
    status: AsbStatus,

    logger: Logger,

    /// Shared, read-only CAL configuration.
    config: Arc<CalConfig>,

    /// Transport URI — this ASB's RADIO binds here.
    transport_uri: String,

    /// Remote RADIO URIs that readers' DISH sockets should connect to.
    /// Empty means readers connect to `transport_uri` (single-process use).
    /// Populate with [`add_receive_peer`] for multi-process topologies.
    peer_uris: Vec<String>,

    /// Serialization format for messages on this transport.
    serialization_format: SerializationFormat,

    /// Registered connection-status listeners.
    listeners: Vec<Arc<dyn AsbStatusListener>>,

    /// Signals all reader tasks to stop; sent on close() (CAL-016049).
    shutdown_tx: Arc<tokio::sync::watch::Sender<()>>,

    /// Test-only gate: forwarding task acquires+drops this before each drain.
    /// Hold externally to freeze the task while flooding writes.
    #[cfg(test)]
    write_gate: Arc<tokio::sync::Mutex<()>>,
}

impl ZmqAsb {
    /// Constructs a new `ZmqAsb` in the `Initializing` state.
    ///
    /// For tcp:// and inproc:// URIs the RADIO socket binds immediately.
    /// For udp:// URIs the RADIO connects to an existing DISH listener.
    pub async fn new(
        service_id: impl Into<String>,
        asb_id: impl Into<String>,
        logger: Logger,
        config: Arc<CalConfig>,
        tconfig: &Transport,
    ) -> CalResult<Self> {
        let transport_uri = tconfig.uri.clone();
        let ep: Endpoint = transport_uri.parse().map_err(|e| {
            CalError::new(
                CalErrorKind::InitializationFailure,
                format!("Invalid transport URI '{}': {}", transport_uri, e),
            )
        })?;

        let radio = Socket::new(SocketType::Radio, Options::default());

        // UDP polarity: DISH binds, RADIO connects.
        // TCP/inproc polarity: RADIO binds, DISH connects.
        if matches!(ep, Endpoint::Udp { .. }) {
            radio.connect(ep).await.map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("RADIO udp connect to '{}' failed: {}", transport_uri, e),
                )
            })?;
        } else {
            radio.bind(ep).await.map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("RADIO bind to '{}' failed: {}", transport_uri, e),
                )
            })?;
        }

        let (shutdown_tx, _) = tokio::sync::watch::channel(());
        let shutdown_tx = Arc::new(shutdown_tx);

        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                let _ = radio.send(msg).await;
            }
            let _ = radio.close().await;
        });

        let logger = logger.new(slog::o!("subsystem" => "zmq"));
        Ok(Self {
            service_id: service_id.into(),
            asb_id: asb_id.into(),
            uuids: ServiceUuids {
                system: UUID::nil(),
                service: UUID::nil(),
                subsystem: None,
                components: Vec::new(),
                capabilities: Vec::new(),
            },
            write_tx: Some(write_tx),
            status: AsbStatus::new(AsbConnectionState::Initializing, "ZeroMQ ASB initializing"),
            logger,
            config,
            transport_uri,
            peer_uris: Vec::new(),
            serialization_format: tconfig.format.clone(),
            listeners: Vec::new(),
            shutdown_tx,
            #[cfg(test)]
            write_gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    /// Registers a remote RADIO URI whose messages this ASB should receive.
    ///
    /// Readers created via [`AbstractServiceBusExt::create_reader`] will
    /// connect their DISH sockets to every registered peer URI.  If no peers
    /// are registered the DISH connects to this ASB's own `transport_uri`,
    /// which is the correct behaviour for single-process tests.
    ///
    /// Call before the first `create_reader`; peers added after reader creation
    /// are not picked up by existing readers.
    pub fn add_receive_peer(&mut self, uri: impl Into<String>) {
        self.peer_uris.push(uri.into());
    }

    /// Returns a clone of the write gate for test-controlled backpressure.
    #[cfg(test)]
    pub fn write_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.write_gate)
    }

    /// Connect to the bus (future use: establish DISH subscriptions).
    pub async fn connect(&mut self) -> CalResult<()> {
        Ok(())
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

impl AbstractServiceBus for ZmqAsb {
    fn get_logger(&self) -> &Logger {
        &self.logger
    }

    fn service_identifier(&self) -> &str {
        &self.service_id
    }

    fn asb_identifier(&self) -> &str {
        &self.asb_id
    }

    fn service_uuids(&self) -> CalResult<&ServiceUuids> {
        Ok(&self.uuids)
    }

    fn oms_schema_version(&self) -> &str {
        env!("RCAL_SCHEMA_VERSION")
    }

    fn oms_schema_compiler_version(&self) -> &str {
        env!("RCAL_OMS_COMPILER_VERSION")
    }

    fn connection_status(&self) -> &AsbStatus {
        &self.status
    }

    fn register_status_listener(&mut self, listener: Arc<dyn AsbStatusListener>) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");

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
        trace!(self.logger, "ZmqAsb::unregister_status_listener()");

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
        trace!(self.logger, "ZmqAsb::close()");
        // Signal all reader tasks to unblock any pending dish.recv() (CAL-016049).
        let _ = self.shutdown_tx.send(());
        // Dropping write_tx closes the channel; the background writer task exits.
        self.write_tx = None;
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ZmqWriter
// ════════════════════════════════════════════════════════════════════════════

/// Poll-mode queue shared between the receive task and [`ZmqReader`].
type PollState<M> = Arc<(Mutex<VecDeque<(Instant, Arc<M>)>>, Condvar)>;

/// ZMQ-backed [`AbstractWriter`]: serializes messages and sends via the shared
/// RADIO socket background task.
pub struct ZmqWriter<M: CalMessage> {
    topic: String,
    logger: Logger,
    format: SerializationFormat,
    direct_tx: Option<tokio::sync::mpsc::UnboundedSender<Message>>,
    writer_buf: Option<Arc<Mutex<VecDeque<Message>>>>,
    writer_notify: Option<Arc<tokio::sync::Notify>>,
    writer_max: Option<usize>,
    writer_task: Option<tokio::task::JoinHandle<()>>,
    _phantom: PhantomData<M>,
}

impl<M: CalMessage + serde::Serialize> AbstractWriter<M> for ZmqWriter<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn write(&mut self, message: &M) -> CalResult<()> {
        trace!(self.logger, "ZmqWriter::write()"; "topic" => &self.topic);
        let xml = serialize_message(message, &self.topic, &self.format)?;
        // RADIO/DISH: part[0] = group (topic for DISH filtering), part[1] = payload
        let msg = Message::multipart([self.topic.clone(), xml]);
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

    fn close(self: Box<Self>) -> CalResult<()> {
        trace!(self.logger, "ZmqWriter::close()"; "topic" => &self.topic);
        if let Some(buf) = &self.writer_buf {
            let remaining = buf.lock().unwrap().len();
            if remaining > 0 {
                if let Some(task) = self.writer_task {
                    task.abort();
                }
                return Err(CalError::new(
                    CalErrorKind::AsbFailed,
                    format!("close: {remaining} buffered messages dropped"),
                ));
            }
        }
        if let Some(task) = self.writer_task {
            task.abort();
        }
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ZmqReader
// ════════════════════════════════════════════════════════════════════════════

/// ZMQ-backed [`AbstractReader`]: receives on a DISH socket in a background
/// task, dispatching to listeners or buffering for polling.
///
/// Callback and polling modes are mutually exclusive (CAL-016050).
///
/// # Connection timing
/// The DISH socket connects asynchronously. Messages may be missed in the
/// brief window between `create_reader` returning and the DISH completing its
/// TCP handshake; callers that require reliable first-message delivery should
/// insert a short delay after creation (consistent with the ZMQ ZMTP
/// handshake model).
///
/// # UDP transport
/// UDP-polarity transports (DISH binds, RADIO connects) are not supported.
pub struct ZmqReader<M: CalMessage> {
    topic: String,
    logger: Logger,
    listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>>,
    /// Shared queue + condvar for poll-mode delivery with expiration support.
    poll_state: PollState<M>,
    /// Set false by the receive task on exit; wakes any blocked read().
    task_alive: Arc<AtomicBool>,
    expiration: Option<Duration>,
    task: tokio::task::JoinHandle<()>,
}

impl<M: CalMessage + serde::de::DeserializeOwned> AbstractReader<M> for ZmqReader<M> {
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
        trace!(self.logger, "ZmqReader::read()"; "topic" => &self.topic, "timeout_ms" => timeout.map(|d| d.as_millis()));
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

    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>> {
        trace!(self.logger, "ZmqReader::read_no_wait()"; "topic" => &self.topic);
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
        trace!(self.logger, "ZmqReader::close()"; "topic" => &self.topic);
        self.task.abort();
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractServiceBusExt implementation
// ════════════════════════════════════════════════════════════════════════════

impl<M> AbstractServiceBusExt<M> for ZmqAsb
where
    M: CalMessage + serde::Serialize + serde::de::DeserializeOwned,
{
    fn create_writer(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractWriter<M>>> {
        validate_topic_type::<M>(&self.config, &self.service_id, topic)?;
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
        #[cfg(test)]
        let write_gate_task = Arc::clone(&self.write_gate);
        let (direct_tx, writer_buf, writer_notify, writer_task) = if let Some(_max) = writer_max {
            let buf: Arc<Mutex<VecDeque<Message>>> = Arc::new(Mutex::new(VecDeque::new()));
            let notify = Arc::new(tokio::sync::Notify::new());
            let buf_task = Arc::clone(&buf);
            let notify_task = Arc::clone(&notify);
            let task = tokio::spawn(async move {
                loop {
                    notify_task.notified().await;
                    // ponytail: test-only gate; freezes drain until caller releases lock,
                    // ensuring overflow logic in write() runs before drain. No-op in production.
                    #[cfg(test)]
                    drop(write_gate_task.lock().await);
                    let msgs: Vec<Message> = buf_task.lock().unwrap().drain(..).collect();
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

        trace!(self.logger, "ZmqAsb::create_writer()"; "topic" => topic);
        Ok(Box::new(ZmqWriter {
            topic: topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => topic.to_string())),
            format: self.serialization_format.clone(),
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
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>> {
        validate_topic_type::<M>(&self.config, &self.service_id, topic)?;
        // Build the list of RADIO URIs for this reader's DISH to connect to.
        // If no peers are registered, connect to our own RADIO (single-process use).
        let connect_uris: Vec<String> = if self.peer_uris.is_empty() {
            vec![self.transport_uri.clone()]
        } else {
            self.peer_uris.clone()
        };
        // Fast-fail on invalid URIs before spawning anything.
        for uri in &connect_uris {
            uri.parse::<Endpoint>().map_err(|e| {
                CalError::new(
                    CalErrorKind::InitializationFailure,
                    format!("invalid transport URI '{uri}': {e}"),
                )
            })?;
        }

        let time_filter = qos.time_based_filter;
        let expiration_dur = qos.expiration.map(|e| e.max_age);
        let reader_max = qos.reader_buffer.map(|b| b.max_messages);

        let poll_state: PollState<M> =
            Arc::new((Mutex::new(VecDeque::new()), Condvar::new()));
        let task_alive = Arc::new(AtomicBool::new(true));

        let listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let listeners_task = Arc::clone(&listeners);
        let poll_state_task = Arc::clone(&poll_state);
        let task_alive_task = Arc::clone(&task_alive);
        let topic_str = topic.to_string();
        let _format = self.serialization_format.clone(); // ponytail: deserialization is format-agnostic (XML only); extend if binary formats are added
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let reader_logger = self.logger.new(slog::o!("topic" => topic.to_string()));

        let task = tokio::spawn(async move {
            let dish = Socket::new(SocketType::Dish, Options::default());
            // Connect to every registered peer RADIO. Connection errors are
            // silent; polling will block/timeout if none succeed.
            for uri in &connect_uris {
                if let Ok(ep) = uri.parse::<Endpoint>() {
                    let _ = dish.connect(ep).await;
                }
            }
            let _ = dish.join(topic_str).await;

            let mut last_accepted: Option<Instant> = None;

            loop {
                let raw = tokio::select! {
                    result = dish.recv() => match result {
                        Ok(r) => r,
                        Err(_) => break,
                    },
                    _ = shutdown_rx.changed() => break,
                };
                let payload = raw.part_bytes(1).unwrap_or_default();
                let m = match deserialize_message::<M>(&payload) {
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
                    // Polling mode: buffer in queue (CAL-016052)
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
                    // Callback mode: dispatch to all listeners (CAL-005392)
                    for l in ls.iter() {
                        l.on_message(&arc_m);
                    }
                    // CAL-016045: message not placed in poll buffer after dispatch
                }
            }

            // Signal any blocked read() callers that the task has exited
            task_alive_task.store(false, Ordering::Release);
            poll_state_task.1.notify_all();
        });

        trace!(self.logger, "ZmqAsb::create_reader()"; "topic" => topic);
        Ok(Box::new(ZmqReader {
            topic: topic.to_string(),
            logger: self.logger.new(slog::o!("topic" => topic.to_string())),
            listeners,
            poll_state,
            task_alive,
            expiration: expiration_dur,
            task,
        }))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Test helpers
// ════════════════════════════════════════════════════════════════════════════

/// Builds a [`CalConfig`] with one TCP transport entry per port in `ports`.
#[cfg(test)]
pub(super) fn test_config_on_ports(ports: &[u16]) -> Arc<CalConfig> {
    use crate::calconfig;
    use crate::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, ports[0].to_string().as_bytes());
    let mut transports = String::new();
    for (i, &port) in ports.iter().enumerate() {
        let id = if i == 0 {
            "TestZmq".to_string()
        } else {
            format!("TestZmq{}", i + 1)
        };
        transports.push_str(&format!(
            "\n[[transport]]\nid = \"{id}\"\ntype = \"zmq\"\nuri = \"tcp://127.0.0.1:{port}\"\n"
        ));
    }
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nlabel = \"OMS Test System\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestZmq\"\n{transports}"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

/// Builds a [`CalConfig`] with an inproc transport.
#[cfg(test)]
pub(super) fn test_config_inproc(name: &str) -> Arc<CalConfig> {
    use crate::calconfig;
    use crate::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, name.as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nlabel = \"OMS Test System\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestZmq\"\n\n[[transport]]\nid = \"TestZmq\"\ntype = \"zmq\"\nuri = \"inproc://{name}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use omq_tokio::endpoint::Host;
    use omq_tokio::{Endpoint, Message, MonitorEvent, Options, Socket, SocketType};
    use rcal_macros::init_test_logger;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{
        Mutex,
        atomic::{AtomicI32, AtomicU32, Ordering},
    };
    use std::time::Duration;

    static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(55600);

    fn next_port() -> u16 {
        NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    async fn make_bus(logger: Logger) -> ZmqAsb {
        let config = test_config_on_ports(&[next_port()]);
        let tconfig = config
            .get_transport(&String::from("TestZmq"))
            .expect("TestZmq transport must exist in test config");
        ZmqAsb::new("Test Service", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap()
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
            Self { value: String::new() }
        }
    }

    // ── Basic construction ────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_check_creation() {
        let a = make_bus(logger).await;
        assert_eq!(a.oms_schema_version(), env!("RCAL_SCHEMA_VERSION"));
        assert_eq!(a.oms_schema_compiler_version(), env!("RCAL_OMS_COMPILER_VERSION"));
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), "TestZmq");
        assert_eq!(
            a.connection_status().state,
            AsbConnectionState::Initializing
        );
    }

    // ── Transport: inproc ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_radio_dish_inproc() {
        let radio = Socket::new(SocketType::Radio, Options::default());
        radio
            .bind("inproc://test-asb-inproc".parse::<Endpoint>().unwrap())
            .await
            .unwrap();

        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.connect("inproc://test-asb-inproc".parse::<Endpoint>().unwrap())
            .await
            .unwrap();
        dish.join("telemetry").await.unwrap();

        radio
            .send(Message::multipart(["telemetry", "42.0"]))
            .await
            .unwrap();
        // inproc is synchronous within the runtime — no sleep needed
        radio
            .send(Message::multipart(["ignored-group", "dropped"]))
            .await
            .unwrap();

        let msg = dish.recv().await.unwrap();
        assert_eq!(msg.part_bytes(0).unwrap(), "telemetry");
        assert_eq!(msg.part_bytes(1).unwrap(), "42.0");

        radio.close().await.unwrap();
        dish.close().await.unwrap();
    }

    // ── Transport: tcp ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_radio_dish_tcp() {
        let radio = Socket::new(SocketType::Radio, Options::default());
        let bound = radio
            .bind("tcp://127.0.0.1:0".parse::<Endpoint>().unwrap())
            .await
            .unwrap();
        let port = match bound {
            Endpoint::Tcp { port, .. } => port,
            _ => panic!("expected TCP endpoint"),
        };

        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.join("status").await.unwrap();
        dish.connect(
            format!("tcp://127.0.0.1:{port}")
                .parse::<Endpoint>()
                .unwrap(),
        )
        .await
        .unwrap();

        // Wait for ZMTP handshake to complete.
        tokio::time::sleep(Duration::from_millis(50)).await;

        radio
            .send(Message::multipart(["status", "online"]))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let msg = dish.recv().await.unwrap();
        assert_eq!(msg.part_bytes(0).unwrap(), "status");
        assert_eq!(msg.part_bytes(1).unwrap(), "online");

        radio.close().await.unwrap();
        dish.close().await.unwrap();
    }

    // ── Transport: udp ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_radio_dish_udp() {
        // UDP polarity: DISH binds, RADIO connects.
        let dish = Socket::new(SocketType::Dish, Options::default());
        let mut mon = dish.monitor();
        dish.bind(Endpoint::Udp {
            group: None,
            host: Host::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: 0,
        })
        .await
        .unwrap();

        // Read the OS-assigned port from the monitor event.
        let port = loop {
            match mon.recv().await.unwrap() {
                MonitorEvent::Listening {
                    endpoint: Endpoint::Udp { port, .. },
                } => break port,
                _ => continue,
            }
        };

        dish.join("sensor").await.unwrap();

        let radio = Socket::new(SocketType::Radio, Options::default());
        radio
            .connect(Endpoint::Udp {
                group: None,
                host: Host::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                port,
            })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(20)).await;

        radio
            .send(Message::multipart(["sensor", "hot"]))
            .await
            .unwrap();
        radio
            .send(Message::multipart(["other", "dropped"]))
            .await
            .unwrap();

        let msg = dish.recv().await.unwrap();
        assert_eq!(msg.part_bytes(0).unwrap(), "sensor");
        assert_eq!(msg.part_bytes(1).unwrap(), "hot");

        radio.close().await.unwrap();
        dish.close().await.unwrap();
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
        let mut a = make_bus(logger).await;

        let l1 = Arc::new(TestStatusListener::new());
        let l2 = Arc::new(TestStatusListener::new());
        let l1d: Arc<dyn AsbStatusListener> = l1.clone();
        let l2d: Arc<dyn AsbStatusListener> = l2.clone();

        assert_eq!(a.listeners.len(), 0);
        a.update_status(AsbConnectionState::Normal, "").unwrap();
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
        let mut a = make_bus(logger).await;

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
        let mut a = make_bus(logger).await;

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
        let mut a = make_bus(logger).await;
        a.update_status(AsbConnectionState::Normal, "").unwrap();
        a.update_status(AsbConnectionState::Failed, "terminal").unwrap();

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        assert!(
            a.register_status_listener(ld).is_err(),
            "register_status_listener in Failed state must return Err (CAL-016366)"
        );
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_invalid_state_transition_returns_err() {
        let mut a = make_bus(logger).await;

        a.update_status(AsbConnectionState::Normal, "").unwrap();
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

    // ── Topic-type enforcement (CAL-005208) ──────────────────────────────

    fn test_config_with_topic_type(port: u16, topic: &str, type_name: &str) -> Arc<CalConfig> {
        use crate::calconfig;
        let toml = format!(
            "[system]\nid = \"Test Service\"\ndefault_transport = \"TestZmq\"\n\
             [[transport]]\nid = \"TestZmq\"\ntype = \"zmq\"\nuri = \"tcp://127.0.0.1:{port}\"\n\
             [[service]]\nid = \"Test Service\"\n\
             [[service.topic]]\nid = \"{topic}\"\ntype = \"{type_name}\"\n"
        );
        Arc::new(calconfig::parse_config(&toml).unwrap())
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_create_writer_wrong_type_returns_err() {
        let port = next_port();
        let config = test_config_with_topic_type(port, "test.topic", "some.OtherMsg");
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("Test Service", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let result = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        );
        assert!(result.is_err(), "create_writer must fail on type mismatch (CAL-005208)");
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_create_reader_wrong_type_returns_err() {
        let port = next_port();
        let config = test_config_with_topic_type(port, "test.topic", "some.OtherMsg");
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("Test Service", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let result = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        );
        assert!(result.is_err(), "create_reader must fail on type mismatch (CAL-005208)");
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_create_writer_correct_type_succeeds() {
        let port = next_port();
        let config = test_config_with_topic_type(port, "test.topic", "test.TestMsg");
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("Test Service", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        assert!(
            <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
                &mut asb,
                "test.topic",
                TopicQos::default(),
            )
            .is_ok(),
            "create_writer must succeed when type matches"
        );
    }

    // ── Writer tests ──────────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_writer_sends_message() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.join("test.topic").await.unwrap();
        dish.connect(
            format!("tcp://127.0.0.1:{port}")
                .parse::<Endpoint>()
                .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
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

        let received = dish.recv().await.unwrap();
        assert_eq!(received.part_bytes(0).unwrap(), "test.topic");
        let payload = received.part_bytes(1).unwrap();
        let xml = std::str::from_utf8(&payload).unwrap();
        assert!(
            xml.contains("hello"),
            "expected XML to contain 'hello', got: {xml}"
        );

        dish.close().await.unwrap();
        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_writer_pretty_xml() {
        let port = next_port();
        // Build config with pretty_xml format
        use crate::calconfig;
        use crate::uci::base::UUID;
        const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
        let ns = UUID::parse_str(BASE_UUID).unwrap();
        let sys_uuid = UUID::generate_v3(&ns, port.to_string().as_bytes());
        let toml = format!(
            "[system]\nid = \"TestSystem\"\nlabel = \"OMS Test System\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestZmq\"\n\n[[transport]]\nid = \"TestZmq\"\ntype = \"zmq\"\nuri = \"tcp://127.0.0.1:{port}\"\nformat = \"pretty_xml\"\n"
        );
        let config = Arc::new(calconfig::parse_config(&toml).unwrap());
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.join("test.topic").await.unwrap();
        dish.connect(
            format!("tcp://127.0.0.1:{port}")
                .parse::<Endpoint>()
                .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        writer
            .write(&TestMsg {
                value: "pretty".to_string(),
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let received = dish.recv().await.unwrap();
        let payload = received.part_bytes(1).unwrap();
        let xml = std::str::from_utf8(&payload).unwrap();
        assert!(
            xml.contains('\n'),
            "pretty XML must contain newlines, got: {xml}"
        );
        assert!(xml.contains("pretty"), "expected 'pretty' in XML payload");

        dish.close().await.unwrap();
        asb.close().unwrap();
    }

    // ── Reader tests ──────────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_reader_poll_mode() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        // Allow DISH to connect and ZMTP handshake to complete
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send a message via the RADIO (through ZmqAsb's write task)
        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();
        writer
            .write(&TestMsg {
                value: "poll_test".to_string(),
            })
            .unwrap();

        // Allow message to propagate
        tokio::time::sleep(Duration::from_millis(50)).await;

        let received = reader.read(Some(Duration::from_millis(100))).unwrap();
        assert!(received.is_some(), "expected a message");
        assert_eq!(received.unwrap().value, "poll_test");

        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_reader_read_no_wait_empty() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        let result = reader.read_no_wait().unwrap();
        assert!(result.is_none(), "expected None for empty buffer");

        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_reader_timeout_returns_none() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        // No messages sent — read should time out and return Ok(None)
        let result = reader.read(Some(Duration::from_millis(30))).unwrap();
        assert!(result.is_none(), "expected Ok(None) on timeout");

        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_reader_poll_fails_with_listener() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        struct NoopListener;
        impl MessageListener<TestMsg> for NoopListener {
            fn on_message(&self, _: &Arc<TestMsg>) {}
        }

        let ld: Arc<dyn MessageListener<TestMsg>> = Arc::new(NoopListener);
        reader.add_listener(ld).unwrap();

        assert!(
            reader.read(Some(Duration::from_millis(1))).is_err(),
            "read() must fail with OperationNotPermitted when listeners are registered"
        );
        assert!(
            reader.read_no_wait().is_err(),
            "read_no_wait() must fail with OperationNotPermitted when listeners are registered"
        );

        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_reader_callback_mode() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        struct CountingListener {
            count: AtomicU32,
        }
        impl MessageListener<TestMsg> for CountingListener {
            fn on_message(&self, _: &Arc<TestMsg>) {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
        }

        let listener = Arc::new(CountingListener {
            count: AtomicU32::new(0),
        });
        let listener_check = Arc::clone(&listener);
        let ld: Arc<dyn MessageListener<TestMsg>> = listener;
        reader.add_listener(ld).unwrap();

        // Allow DISH to connect
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();
        writer
            .write(&TestMsg {
                value: "cb_test".to_string(),
            })
            .unwrap();
        writer
            .write(&TestMsg {
                value: "cb_test2".to_string(),
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(
            listener_check.count.load(Ordering::SeqCst),
            2,
            "listener must be called once per message"
        );

        asb.close().unwrap();
    }

    // ── close() unblocks read() (CAL-016049) ─────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_close_unblocks_blocked_read() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        tokio::time::sleep(Duration::from_millis(20)).await;

        // Spawn a task that blocks indefinitely waiting for a message.
        let read_task = tokio::task::spawn_blocking(move || reader.read(None));

        // Close the ASB — must wake the blocked read().
        asb.close().unwrap();

        let result = tokio::time::timeout(Duration::from_millis(500), read_task)
            .await
            .expect("read() did not unblock within 500 ms after close()")
            .expect("task did not panic");

        assert!(result.is_err(), "read() must return Err after close() (CAL-016049)");
    }

    // ── QoS: TimeBasedFilter ──────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_qos_time_based_filter() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let qos = TopicQos {
            time_based_filter: Some(super::super::TimeBasedFilter {
                min_separation: Duration::from_millis(200),
            }),
            ..TopicQos::default()
        };
        let mut reader =
            <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(&mut asb, "test.topic", qos)
                .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        // Send two messages back-to-back; second should be filtered
        writer
            .write(&TestMsg {
                value: "first".into(),
            })
            .unwrap();
        writer
            .write(&TestMsg {
                value: "second".into(),
            })
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m1 = reader.read_no_wait().unwrap();
        let m2 = reader.read_no_wait().unwrap();
        assert!(m1.is_some(), "first message must pass filter");
        assert!(
            m2.is_none(),
            "second message must be dropped by time filter"
        );

        asb.close().unwrap();
    }

    // ── QoS: Expiration ───────────────────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_qos_expiration() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let qos = TopicQos {
            expiration: Some(super::super::Expiration {
                max_age: Duration::from_millis(50),
            }),
            ..TopicQos::default()
        };
        let mut reader =
            <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(&mut asb, "test.topic", qos)
                .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();
        writer
            .write(&TestMsg {
                value: "expire_me".into(),
            })
            .unwrap();
        // Let the message arrive in the buffer, then age past max_age
        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = reader.read_no_wait().unwrap();
        assert!(result.is_none(), "expired message must be discarded");

        asb.close().unwrap();
    }

    // ── QoS: MessageBuffer (reader) ───────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_qos_reader_buffer() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let qos = TopicQos {
            reader_buffer: Some(super::super::MessageBuffer { max_messages: 2 }),
            ..TopicQos::default()
        };
        let mut reader =
            <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(&mut asb, "test.topic", qos)
                .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();

        // Send 3 messages; buffer holds 2 — oldest must be dropped
        writer.write(&TestMsg { value: "a".into() }).unwrap();
        writer.write(&TestMsg { value: "b".into() }).unwrap();
        writer.write(&TestMsg { value: "c".into() }).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m1 = reader.read_no_wait().unwrap();
        let m2 = reader.read_no_wait().unwrap();
        let m3 = reader.read_no_wait().unwrap();
        // "a" is dropped (oldest); "b" and "c" survive
        assert_eq!(m1.as_deref().map(|m| m.value.as_str()), Some("b"));
        assert_eq!(m2.as_deref().map(|m| m.value.as_str()), Some("c"));
        assert!(m3.is_none(), "buffer must be empty after 2 messages");

        asb.close().unwrap();
    }

    // ── QoS: MessageBuffer (writer) ───────────────────────────────────────

    #[init_test_logger]
    #[tokio::test]
    async fn test_qos_writer_buffer() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        // Hold the gate before creating the writer so the forwarding task
        // cannot drain writer_buf between our three write() calls.
        let gate = asb.write_gate();
        let _hold = gate.lock().await;

        let writer_qos = TopicQos {
            writer_buffer: Some(super::super::MessageBuffer { max_messages: 2 }),
            ..TopicQos::default()
        };
        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            writer_qos,
        )
        .unwrap();

        let mut reader = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_reader(
            &mut asb,
            "test.topic",
            TopicQos::default(),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Gate held: forwarding task blocks before each drain.
        // write("c") overflows cap-2 buffer, dropping "a" (oldest). buf=[b,c].
        writer.write(&TestMsg { value: "a".into() }).unwrap();
        writer.write(&TestMsg { value: "b".into() }).unwrap();
        writer.write(&TestMsg { value: "c".into() }).unwrap();

        drop(_hold); // forwarding task unblocks, drains [b, c] to RADIO
        tokio::time::sleep(Duration::from_millis(50)).await;

        let m1 = reader.read_no_wait().unwrap();
        let m2 = reader.read_no_wait().unwrap();
        let m3 = reader.read_no_wait().unwrap();
        assert_eq!(
            m1.as_deref().map(|m| m.value.as_str()),
            Some("b"),
            "oldest message must be dropped by writer buffer"
        );
        assert_eq!(m2.as_deref().map(|m| m.value.as_str()), Some("c"));
        assert!(m3.is_none(), "only max_messages forwarded");

        asb.close().unwrap();
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_writer_close_errors_on_unflushed() {
        let port = next_port();
        let config = test_config_on_ports(&[port]);
        let tconfig = config.get_transport(&String::from("TestZmq")).unwrap();
        let mut asb = ZmqAsb::new("TestSvc", "TestZmq", logger, config.clone(), tconfig)
            .await
            .unwrap();

        let gate = asb.write_gate();
        let _hold = gate.lock().await; // freeze the forwarding task

        let writer_qos = TopicQos {
            writer_buffer: Some(super::super::MessageBuffer { max_messages: 10 }),
            ..TopicQos::default()
        };
        let mut writer = <ZmqAsb as AbstractServiceBusExt<TestMsg>>::create_writer(
            &mut asb,
            "test.topic",
            writer_qos,
        )
        .unwrap();

        writer.write(&TestMsg { value: "pending".into() }).unwrap();
        // Gate still held: forwarding task has not drained; close must error.
        let result = writer.close();
        assert!(result.is_err(), "close must error when buffer is non-empty");

        asb.close().unwrap();
    }
}
