//! ZeroMQ-backed Abstract Service Bus implementation.

#![allow(dead_code)]

use slog::{Logger, trace};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zeromq::{Socket, PubSocket, SubSocket};

use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};
use crate::calconfig::{CalConfig, Transport};
use crate::uci::base::{ServiceUuids, UUID};
use crate::uci::{CalError, CalImplementationErrorKind, CalResult};

/// ASB identifier string for the ZeroMQ transport.
pub const ZMQ_ASB_ID: &str = "zmq";

/// ZeroMQ-backed CAL instance.
///
/// Implements [`AbstractServiceBus`] using ZeroMQ PUB/SUB sockets.
///
/// The struct is `'static` — it owns all data it needs.
pub struct ZmqAsb {
    asb_id: String,
    service_id: String,
    uuids: ServiceUuids,

    /// ZeroMQ publisher sockets keyed by topic.
    out_conns: HashMap<String, PubSocket>,

    /// ZeroMQ subscriber sockets keyed by topic.
    in_conns: HashMap<String, SubSocket>,

    /// Current ASB connection status.
    status: AsbStatus,

    logger: Logger,

    /// Shared, read-only CAL configuration.
    config: Arc<CalConfig>,

    /// Transport URI extracted from the resolved [`Transport`] at construction.
    /// Owned to avoid holding a borrow of `CalConfig` for the lifetime of
    /// this struct
    transport_uri: String,

    /// Registered connection-status listeners.
    listeners: Vec<Arc<Mutex<dyn AsbStatusListener>>>,
}

impl ZmqAsb {
    /// Constructs a new `ZmqAsb` in the `Initializing` state.
    ///
    /// The `transport_uri` is cloned from `tconfig` so the struct carries no
    /// borrowed lifetime.
    pub fn new(
        service_id: impl Into<String>,
        asb_id: impl Into<String>,
        logger: Logger,
        config: Arc<CalConfig>,
        tconfig: &Transport,
    ) -> CalResult<Self> {
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
            out_conns: HashMap::new(),
            in_conns: HashMap::new(),
            status: AsbStatus::new(AsbConnectionState::Initializing, "ZeroMQ ASB initializing"),
            logger,
            config,
            transport_uri: tconfig.uri.clone(),
            listeners: Vec::new(),
        })
    }

    // Connect to the pub/sub bus.
    pub async fn connect(&mut self) -> CalResult<()> {
        let mut sub_socket = SubSocket::new();
        sub_socket.connect(&self.transport_uri).await.map_err(|err| CalError::with_source(crate::uci::CalErrorKind::InitializationFailure, "Can't sub to zmq.", err))?;
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
        // In a production build this would be embedded at compile time via
        // a build.rs / env!() pattern.
        "2.1.0_test_schema"
    }

    fn oms_schema_compiler_version(&self) -> &str {
        "0.1.0"
    }

    fn connection_status(&self) -> &AsbStatus {
        &self.status
    }

    /// Registers a status listener.
    ///
    /// The listener's `on_status_change` is called **immediately** with the
    /// current state before the method returns, as required by §5.9.
    ///
    /// Returns `Err(ImplementationError { ListenerError })` if the same
    /// `Arc` is already registered (pointer equality check).
    fn register_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");

        // Reject duplicate registrations (same Arc pointer).
        if self.listeners.iter().any(|l| Arc::ptr_eq(l, &listener)) {
            return Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError,
                "Status listener is already registered.",
            ));
        }

        // CERT CAL-016366: invoke immediately with the current status *before*
        // adding to the list, so the call happens outside the listeners vec
        // iteration path (prevents potential borrow issues).
        listener.lock().unwrap().on_status_change(&self.status);

        self.listeners.push(listener);
        Ok(())
    }

    fn unregister_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::unregister_status_listener()");

        if let Some(index) = self
            .listeners
            .iter()
            .position(|l| Arc::ptr_eq(l, &listener))
        {
            // swap_remove is O(1); listener ordering is not guaranteed.
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
        // TODO: close open PUB/SUB sockets, drain in-flight messages.
        Ok(())
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calconfig;
    use rcal_macros::init_test_logger;
    use std::sync::atomic::{AtomicI32, Ordering};

    fn get_test_conf() -> Arc<CalConfig> {
        Arc::new(
            calconfig::parse_config_from_file(&calconfig::get_test_config_path(
                "calconfig_sample.toml",
            ))
            .unwrap(),
        )
    }

    fn make_bus(logger: Logger) -> ZmqAsb {
        let config = get_test_conf();
        let tconfig = config
            .get_transport(&String::from("TestZmq"))
            .expect("TestZmq transport must exist in test config");
        ZmqAsb::new("Test Service", "TestZmq", logger, config.clone(), tconfig).unwrap()
    }

    // ── Basic construction ────────────────────────────────────────────────

    #[test]
    fn test_check_creation() {
        let logger = init_test_logger!();
        let a = make_bus(logger);
        assert_eq!(a.oms_schema_version(), "2.1.0_test_schema");
        assert_eq!(a.oms_schema_compiler_version(), "0.1.0");
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), ZMQ_ASB_ID);
        assert_eq!(
            a.connection_status().state,
            AsbConnectionState::Initializing
        );
    }

    // ── Listener helper ───────────────────────────────────────────────────

    /// A status listener that records how many times it has been called and
    /// the most recent state observed.
    ///
    /// Uses `AtomicI32` for the count so the type is `Sync` even without a
    /// `Mutex` around the whole struct.  The state is wrapped in a `Mutex`
    /// because `AsbConnectionState` is `Copy` but needs interior mutability.
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

    /// Full lifecycle test for `register_status_listener`,
    /// `update_status`, and `unregister_status_listener`.
    ///
    /// The immediate-invocation rule means each `register_status_listener`
    /// call adds **+1** to the listener's count before the first
    /// `update_status` is invoked.
    ///
    /// ```
    /// Step                       l1.count  l2.count
    /// ─────────────────────────  ────────  ────────
    /// initial                         0         0
    /// update_status(Normal)           0         0   (not yet registered)
    /// register l1  → immediate +1     1         0   (state = Normal)
    /// update_status(Degraded)         2         0
    /// register l2  → immediate +1     2         1   (state = Degraded)
    /// update_status(Normal)           3         2
    /// unregister l1                   3         2
    /// update_status(Failed)           3         3
    /// ```
    #[test]
    fn test_status_listeners() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger);

        let l1 = Arc::new(Mutex::new(TestStatusListener::new()));
        let l2 = Arc::new(Mutex::new(TestStatusListener::new()));

        // No listeners yet — update has no effect on counts.
        assert_eq!(a.listeners.len(), 0);
        a.update_status(AsbConnectionState::Normal, "").unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 0);
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        // Register l1 → immediate callback fires (CERT CAL-016366).
        a.register_status_listener(l1.clone()).unwrap();
        assert_eq!(a.listeners.len(), 1);
        assert_eq!(
            l1.lock().unwrap().call_count(),
            1,
            "immediate callback on register"
        );
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        // update_status fires all registered listeners.
        a.update_status(AsbConnectionState::Degraded, "degraded")
            .unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 2);
        assert_eq!(
            l1.lock().unwrap().last_state(),
            AsbConnectionState::Degraded
        );
        assert_eq!(l2.lock().unwrap().call_count(), 0);

        // Register l2 → immediate callback with *current* state (Degraded).
        a.register_status_listener(l2.clone()).unwrap();
        assert_eq!(a.listeners.len(), 2);
        assert_eq!(l1.lock().unwrap().call_count(), 2);
        assert_eq!(
            l2.lock().unwrap().call_count(),
            1,
            "immediate callback on register"
        );
        assert_eq!(
            l2.lock().unwrap().last_state(),
            AsbConnectionState::Degraded
        );

        // Both listeners receive the next update.
        a.update_status(AsbConnectionState::Normal, "recovered")
            .unwrap();
        assert_eq!(l1.lock().unwrap().call_count(), 3);
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 2);
        assert_eq!(l2.lock().unwrap().last_state(), AsbConnectionState::Normal);

        // Unregister l1.
        a.unregister_status_listener(l1.clone()).unwrap();
        assert_eq!(a.listeners.len(), 1);

        // Only l2 receives the Failed update.
        a.update_status(AsbConnectionState::Failed, "terminal")
            .unwrap();
        assert_eq!(
            l1.lock().unwrap().call_count(),
            3,
            "l1 must not be called after unregister"
        );
        assert_eq!(l1.lock().unwrap().last_state(), AsbConnectionState::Normal);
        assert_eq!(l2.lock().unwrap().call_count(), 3);
        assert_eq!(l2.lock().unwrap().last_state(), AsbConnectionState::Failed);
    }

    #[test]
    fn test_duplicate_register_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger);

        let l = Arc::new(Mutex::new(TestStatusListener::new()));
        a.register_status_listener(l.clone()).unwrap();
        let result = a.register_status_listener(l.clone());
        assert!(
            result.is_err(),
            "registering the same Arc twice must return Err"
        );
    }

    #[test]
    fn test_unregister_unknown_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger);

        let l = Arc::new(Mutex::new(TestStatusListener::new()));
        let result = a.unregister_status_listener(l);
        assert!(
            result.is_err(),
            "unregistering an unknown listener must return Err"
        );
    }

    #[test]
    fn test_invalid_state_transition_returns_err() {
        let logger = init_test_logger!();
        let mut a = make_bus(logger);

        // Drive to Failed.
        a.update_status(AsbConnectionState::Normal, "").unwrap();
        a.update_status(AsbConnectionState::Failed, "").unwrap();

        // All transitions out of Failed must be rejected.
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
