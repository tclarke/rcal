//! omq-tokio-backed Abstract Service Bus implementation (RADIO/DISH).

#![allow(dead_code)]

use slog::{Logger, trace};
use std::sync::{Arc, Mutex};

use omq_tokio::{Endpoint, Options, Socket, SocketType};

use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};
use crate::calconfig::{CalConfig, Transport};
use crate::uci::base::{ServiceUuids, UUID};
use crate::uci::{CalError, CalErrorKind, CalImplementationErrorKind, CalResult};

/// ASB identifier string for the ZeroMQ-compatible transport.
pub const ZMQ_ASB_ID: &str = "zmq";

/// omq-tokio RADIO/DISH-backed CAL instance.
///
/// Implements [`AbstractServiceBus`] using RADIO for broadcast (no broker
/// required for up to ~10 peers). TCP and inproc: RADIO binds, DISH connects.
/// UDP: DISH binds, RADIO connects — callers handle that polarity in tests.
pub struct ZmqAsb {
    asb_id: String,
    service_id: String,
    uuids: ServiceUuids,

    /// RADIO socket — the send/publish side.
    radio: Option<Socket>,

    /// Current ASB connection status.
    status: AsbStatus,

    logger: Logger,

    /// Shared, read-only CAL configuration.
    config: Arc<CalConfig>,

    /// Transport URI extracted from the resolved [`Transport`] at construction.
    transport_uri: String,

    /// Registered connection-status listeners.
    listeners: Vec<Arc<Mutex<dyn AsbStatusListener>>>,
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
            radio: Some(radio),
            status: AsbStatus::new(AsbConnectionState::Initializing, "ZeroMQ ASB initializing"),
            logger,
            config,
            transport_uri,
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
            listener.lock().unwrap().on_status_change(&self.status);
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

    fn register_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");

        if self.listeners.iter().any(|l| Arc::ptr_eq(l, &listener)) {
            return Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError,
                "Status listener is already registered.",
            ));
        }

        listener.lock().unwrap().on_status_change(&self.status);
        self.listeners.push(listener);
        Ok(())
    }

    fn unregister_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::unregister_status_listener()");

        if let Some(index) = self.listeners.iter().position(|l| Arc::ptr_eq(l, &listener)) {
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
        // ponytail: fire-and-forget close; add graceful drain if message loss on shutdown matters
        if let Some(sock) = self.radio.take() {
            tokio::spawn(async move { let _ = sock.close().await; });
        }
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
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

#[cfg(test)]
mod tests {
    use super::*;
    use omq_tokio::{MonitorEvent, Socket, SocketType, Options, Message, Endpoint};
    use omq_tokio::endpoint::Host;
    use rcal_macros::init_test_logger;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicI32, AtomicU16, Ordering};
    use std::time::Duration;

    static NEXT_PORT: AtomicU16 = AtomicU16::new(55600);

    fn next_port() -> u16 {
        NEXT_PORT.fetch_add(1, Ordering::SeqCst)
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

    // ── Basic construction ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_check_creation() {
        let logger = init_test_logger!();
        let a = make_bus(logger).await;
        assert_eq!(a.oms_schema_version(), "2.1.0_test_schema");
        assert_eq!(a.oms_schema_compiler_version(), "0.1.0");
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), ZMQ_ASB_ID);
        assert_eq!(a.connection_status().state, AsbConnectionState::Initializing);
    }

    // ── Transport: inproc ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_radio_dish_inproc() {
        let radio = Socket::new(SocketType::Radio, Options::default());
        radio.bind("inproc://test-asb-inproc".parse::<Endpoint>().unwrap()).await.unwrap();

        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.connect("inproc://test-asb-inproc".parse::<Endpoint>().unwrap()).await.unwrap();
        dish.join("telemetry").await.unwrap();

        radio.send(Message::multipart(["telemetry", "42.0"])).await.unwrap();
        // inproc is synchronous within the runtime — no sleep needed
        radio.send(Message::multipart(["ignored-group", "dropped"])).await.unwrap();

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
        dish.connect(format!("tcp://127.0.0.1:{port}").parse::<Endpoint>().unwrap())
            .await
            .unwrap();

        // Wait for ZMTP handshake to complete.
        tokio::time::sleep(Duration::from_millis(50)).await;

        radio.send(Message::multipart(["status", "online"])).await.unwrap();
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

        radio.send(Message::multipart(["sensor", "hot"])).await.unwrap();
        radio.send(Message::multipart(["other", "dropped"])).await.unwrap();

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
        fn on_status_change(&mut self, status: &AsbStatus) {
            self.count.fetch_add(1, Ordering::SeqCst);
            *self.last_state.lock().unwrap() = status.state;
        }
    }

    // ── Status-listener tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_status_listeners() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger).await;

        let l1 = Arc::new(Mutex::new(TestStatusListener::new()));
        let l2 = Arc::new(Mutex::new(TestStatusListener::new()));

        assert_eq!(a.listeners.len(), 0);
        a.update_status(AsbConnectionState::Normal, "").unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 0);
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        a.register_status_listener(l1.clone()).unwrap();
        assert_eq!(a.listeners.len(), 1);
        assert_eq!(l1.lock().unwrap().call_count(), 1, "immediate callback on register");
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        a.update_status(AsbConnectionState::Degraded, "degraded").unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 2);
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Degraded);
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        a.register_status_listener(l2.clone()).unwrap();
        assert_eq!(a.listeners.len(), 2);
        assert_eq!(l1.lock().unwrap().call_count(), 2);
        assert_eq!(l2.lock().unwrap().call_count(), 1, "immediate callback on register");
        assert_eq!(l2.lock().unwrap().last_state(), AsbConnectionState::Degraded);

        a.update_status(AsbConnectionState::Normal, "recovered").unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 3);
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 2);
        assert_eq!(l2.lock().unwrap().last_state(), AsbConnectionState::Normal);

        a.unregister_status_listener(l1.clone()).unwrap();
        assert_eq!(a.listeners.len(), 1);

        a.update_status(AsbConnectionState::Failed, "terminal").unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 3, "l1 must not be called after unregister");
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 3);
        assert_eq!(l2.lock().unwrap().last_state(), AsbConnectionState::Failed);
        drop(a);
    }

    #[tokio::test]
    async fn test_duplicate_register_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger).await;

        let l = Arc::new(Mutex::new(TestStatusListener::new()));
        a.register_status_listener(l.clone()).unwrap();
        let result = a.register_status_listener(l.clone());
        assert!(result.is_err(), "registering the same Arc twice must return Err");
    }

    #[tokio::test]
    async fn test_unregister_unknown_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger).await;

        let l = Arc::new(Mutex::new(TestStatusListener::new()));
        let result = a.unregister_status_listener(l);
        assert!(result.is_err(), "unregistering an unknown listener must return Err");
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
}
