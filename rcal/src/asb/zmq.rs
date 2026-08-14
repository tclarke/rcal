//! omq-tokio-backed Abstract Service Bus implementation (RADIO/DISH).

#![allow(dead_code)]

use slog::{Logger, trace};
use std::marker::PhantomData;
use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    format: &SerializationFormat,
) -> CalResult<String> {
    match format {
        SerializationFormat::Xml => quick_xml::se::to_string(msg)
            .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string())),
        SerializationFormat::PrettyXml => {
            let mut buf = String::new();
            let mut ser = quick_xml::se::Serializer::new(&mut buf);
            ser.indent(' ', 4);
            msg.serialize(ser)
                .map_err(|e| CalError::new(CalErrorKind::SerializationError, e.to_string()))?;
            Ok(buf)
        }
    }
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

    /// Transport URI — readers' DISH sockets connect here.
    transport_uri: String,

    /// Serialization format for messages on this transport.
    serialization_format: SerializationFormat,

    /// Registered connection-status listeners.
    listeners: Vec<Arc<dyn AsbStatusListener>>,
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

        let (write_tx, mut write_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
        tokio::spawn(async move {
            while let Some(msg) = write_rx.recv().await {
                let _ = radio.send(msg).await;
            }
            let _ = radio.close().await;
        });

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
            serialization_format: tconfig.format.clone(),
            listeners: Vec::new(),
        })
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
        ZMQ_ASB_ID
    }

    fn service_uuids(&self) -> CalResult<&ServiceUuids> {
        Ok(&self.uuids)
    }

    fn oms_schema_version(&self) -> &str {
        "2.1.0_test_schema"
    }

    fn oms_schema_compiler_version(&self) -> &str {
        "0.1.0"
    }

    fn connection_status(&self) -> &AsbStatus {
        &self.status
    }

    fn register_status_listener(&mut self, listener: Arc<dyn AsbStatusListener>) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");

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
        // Dropping write_tx closes the channel; the background task exits and closes the socket.
        self.write_tx = None;
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ZmqWriter
// ════════════════════════════════════════════════════════════════════════════

/// ZMQ-backed [`AbstractWriter`]: serializes messages and sends via the shared
/// RADIO socket background task.
pub struct ZmqWriter<M: CalMessage> {
    topic: String,
    _qos: TopicQos,
    format: SerializationFormat,
    write_tx: tokio::sync::mpsc::UnboundedSender<Message>,
    _phantom: PhantomData<M>,
}

impl<M: CalMessage + serde::Serialize> AbstractWriter<M> for ZmqWriter<M> {
    fn topic(&self) -> &str {
        &self.topic
    }

    fn write(&mut self, message: &M) -> CalResult<()> {
        let xml = serialize_message(message, &self.format)?;
        // RADIO/DISH: part[0] = group (topic for DISH filtering), part[1] = payload
        let msg = Message::multipart([self.topic.clone(), xml]);
        self.write_tx
            .send(msg)
            .map_err(|_| CalError::new(CalErrorKind::AsbFailed, "ASB write channel closed"))
    }

    fn close(self: Box<Self>) -> CalResult<()> {
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
    _qos: TopicQos,
    listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>>,
    // ponytail: Mutex<Receiver> for Send + Sync; Receiver alone is !Sync
    poll_rx: Mutex<std::sync::mpsc::Receiver<Arc<M>>>,
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
        if !self.listeners.lock().unwrap().is_empty() {
            return Err(CalError::new(
                CalErrorKind::OperationNotPermitted,
                "polling is not permitted while listeners are registered (CAL-016050)",
            ));
        }
        let rx = self.poll_rx.lock().unwrap();
        match timeout {
            Some(d) => match rx.recv_timeout(d) {
                Ok(m) => Ok(Some(m)),
                Err(RecvTimeoutError::Timeout) => Ok(None),
                Err(RecvTimeoutError::Disconnected) => Err(CalError::new(
                    CalErrorKind::AsbFailed,
                    "reader task has stopped",
                )),
            },
            None => rx
                .recv()
                .map(Some)
                .map_err(|_| CalError::new(CalErrorKind::AsbFailed, "reader task has stopped")),
        }
    }

    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>> {
        if !self.listeners.lock().unwrap().is_empty() {
            return Err(CalError::new(
                CalErrorKind::OperationNotPermitted,
                "polling is not permitted while listeners are registered (CAL-016050)",
            ));
        }
        match self.poll_rx.lock().unwrap().try_recv() {
            Ok(m) => Ok(Some(m)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(CalError::new(
                CalErrorKind::AsbFailed,
                "reader task has stopped",
            )),
        }
    }

    fn close(self: Box<Self>) -> CalResult<()> {
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

        Ok(Box::new(ZmqWriter {
            topic: topic.to_string(),
            _qos: qos,
            format: self.serialization_format.clone(),
            write_tx: tx,
            _phantom: PhantomData,
        }))
    }

    fn create_reader(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>> {
        // Parse URI now for fast-fail on invalid config (no network I/O).
        let ep: Endpoint = self.transport_uri.parse().map_err(|e| {
            CalError::new(
                CalErrorKind::InitializationFailure,
                format!("invalid transport URI: {e}"),
            )
        })?;

        let (poll_tx, poll_rx) = std::sync::mpsc::channel::<Arc<M>>();
        let listeners: Arc<Mutex<Vec<Arc<dyn MessageListener<M>>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let listeners_task = Arc::clone(&listeners);
        let topic_str = topic.to_string();
        let _format = self.serialization_format.clone(); // ponytail: deserialization is format-agnostic (XML only); extend if binary formats are added

        let task = tokio::spawn(async move {
            let dish = Socket::new(SocketType::Dish, Options::default());
            // Connection errors here are silent; polling will block/timeout.
            let _ = dish.connect(ep).await;
            let _ = dish.join(topic_str).await;

            while let Ok(raw) = dish.recv().await {
                let payload = raw.part_bytes(1).unwrap_or_default();
                if let Ok(m) = deserialize_message::<M>(&payload) {
                    let arc_m = Arc::new(m);
                    let ls = listeners_task.lock().unwrap();
                    if ls.is_empty() {
                        // Polling mode: buffer in channel (CAL-016052)
                        let _ = poll_tx.send(Arc::clone(&arc_m));
                    } else {
                        // Callback mode: dispatch to all listeners (CAL-005392)
                        for l in ls.iter() {
                            l.on_message(&arc_m);
                        }
                        // CAL-016045: message not placed in poll buffer after dispatch
                    }
                }
            }
        });

        Ok(Box::new(ZmqReader {
            topic: topic.to_string(),
            _qos: qos,
            listeners,
            poll_rx: Mutex::new(poll_rx),
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
        fn message_type_name() -> &'static str {
            "test.TestMsg"
        }
    }

    // ── Basic construction ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_check_creation() {
        let logger = init_test_logger!();
        let a = make_bus(logger).await;
        assert_eq!(a.oms_schema_version(), "2.1.0_test_schema");
        assert_eq!(a.oms_schema_compiler_version(), "0.1.0");
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), ZMQ_ASB_ID);
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

    #[tokio::test]
    async fn test_status_listeners() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_duplicate_register_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger).await;

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        a.register_status_listener(ld.clone()).unwrap();
        let result = a.register_status_listener(ld.clone());
        assert!(
            result.is_err(),
            "registering the same Arc twice must return Err"
        );
    }

    #[tokio::test]
    async fn test_unregister_unknown_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger).await;

        let ld: Arc<dyn AsbStatusListener> = Arc::new(TestStatusListener::new());
        let result = a.unregister_status_listener(&ld);
        assert!(
            result.is_err(),
            "unregistering an unknown listener must return Err"
        );
    }

    #[tokio::test]
    async fn test_invalid_state_transition_returns_err() {
        let logger = init_test_logger!();
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

    // ── Writer tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_writer_sends_message() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_writer_pretty_xml() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_reader_poll_mode() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_reader_read_no_wait_empty() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_reader_timeout_returns_none() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_reader_poll_fails_with_listener() {
        let logger = init_test_logger!();
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

    #[tokio::test]
    async fn test_reader_callback_mode() {
        let logger = init_test_logger!();
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
}
