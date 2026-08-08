// =============================================================================
//  OMS Open Mission Systems — Rust Critical Abstraction Layer (CAL)
//  ZeroMQ based ASB
//
// =============================================================================
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use zeromq::{PubSocket, SubSocket};
use slog::{Logger, trace, error};

use crate::calconfig::{Transport};
use crate::uci::{CalResult, CalError, CalImplementationErrorKind};
use crate::uci::base::{ServiceUuids, UUID};
use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};

pub const ZMQ_ASB_ID: &str = "zmq";

pub struct ZmqAsb {
    asb_id: String,
    service_id: String,
    uuids: ServiceUuids,

    out_conns: HashMap<String, PubSocket>,
    in_conns: HashMap<String, SubSocket>,

    listeners: Vec<Arc<dyn AsbStatusListener>>,

    logger: slog::Logger,
}

impl ZmqAsb {
    pub fn new<S: Into<String>>(service_id: S, logger: Logger) -> Self {
        Self {
            service_id: service_id.into(),
            uuids: ServiceUuids{
                system: UUID::nil(),
                service: UUID::nil(),
                subsystem: None,
                components: Vec::new(),
                capabilities: Vec::new(),
            },
            out_conns: HashMap::new(),
            in_conns: HashMap::new(),

            listeners: Vec::new(),

            logger,
        }
    }

    pub fn update_status<S: Into<String>>(&self, state: AsbConnectionState, description: S) {
        self.status.state = state;
        self.status.description = description.into();
        self.listeners.iter().for_each(|mut l| {l.on_status_change(&self.status);});
    }
}

impl AbstractServiceBus for ZmqAsb {
    fn get_logger(&self) -> &slog::Logger {
        &self.logger
    }
    fn service_identifier(&self) -> &str {
        self.service_id.as_str()
    }

    fn asb_identifier(&self)  -> &str {
        ZMQ_ASB_ID
    }

    fn service_uuids(&self) -> CalResult<&ServiceUuids> {
        Ok(&self.uuids)
    }

    fn oms_schema_version(&self) -> &str {
        let version: &'static str = "2.1.0_test_schema";
        version
    }

    fn oms_schema_compiler_version(&self) -> &str {
        let version: &'static str = "0.1.0";
        version
    }

    fn register_status_listener(
        &mut self,
        _listener: Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");
        if let Some(index) = self.listeners.iter().position(|l| *l == listener) {
            Err(CalError::new_impl(
                CalImplementationErrorKind::ListenerError, "Status listener not registered."))
        } else {
            self.listeners.push(listener);
            Ok(())
        }
    }

    fn unregister_status_listener(
        &mut self,
        _listener: &Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::unregister_status_listener()");
        if let Some(index) = self.listeners.iter().position(|l| l == listener) {
            self.listeners.swap_remove(index);
            Ok(())
        } else {
            Err(CalError*::new_impl(
                CalImplementationErrorKind::ListenerErr.&mut self.stateor, "Status listener not registered."))
        }
    }

    fn close(&mut self) -> CalResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_creation() {
        let logger = procedural_macros::init_test_logger!();
        // let ns = UUID::generate_v4();
        let a = ZmqAsb::new("Test Service", logger);
            /* UUID::generate_v3(&ns, b"service"),
            UUID::generate_v3(&ns, b"system"),
            Some(UUID::generate_v3(&ns, b"subsystem"))); */
        assert_eq!(a.oms_schema_version(), "2.1.0_test_schema");
        assert_eq!(a.oms_schema_compiler_version(), "0.1.0");
        assert_eq!(a.service_identifier(), "Test Service");
        assert_eq!(a.asb_identifier(), ZMQ_ASB_ID)
    }

    struct TestStatusListener {
        count: i8,
        state: AsbConnectionState,
    }

    impl AsbStatusListener for TestStatusListener {
        fn on_status_change(&mut self, status: &AsbStatus) {
            self.count += 1;
            &mut self.state = status.state;
        }
    }

    #[test]
    fn test_status_listeners() {
        let mut a = ZmqAsb::new("Test Service");
        let mut l1 = Arc::new(TestStatusListener{count: 0, state: AsbConnectionState::Inoperable});
        let mut l2 = Arc::new(TestStatusListener{count: 0, state: AsbConnectionState::Inoperable});

        assert_eq!(a.listeners.len(), 0);
        a.update_status(AsbConnectionState::Normal, "");
        assert_eq!(l1.count, 0);
        assert_eq!(l2.count, 0);

        a.register_status_listener(&l1.into()).unwrap();
        assert_eq!(a.listeners.len(), 1);
        a.update_status(AsbConnectionState::Degraded, "");
        assert_eq!(l1.count, 1);
        assert_eq!(l1.state, AsbConnectionState::Degraded);
        assert_eq!(l2.count, 0);
        assert_eq!(l2.state, AsbConnectionState::Inoperable);

        a.register_status_listener(&l2.into()).unwrap();
        assert_eq!(a.listeners.len(), 2);
        a.update_status(AsbConnectionState::Normal, "");
        assert_eq!(l1.count, 2);
        assert_eq!(l1.state, AsbConnectionState::Normal);
        assert_eq!(l2.count, 1);
        assert_eq!(l2.state, AsbConnectionState::Normal);

        a.unregister_status_listener(&l1.into()).unwrap();
        assert_eq!(a.listeners.len(), 1);
        a.update_status(AsbConnectionState::Failed, "");
        assert_eq!(l1.count, 2);
        assert_eq!(l1.state, AsbConnectionState::Normal);
        assert_eq!(l2.count, 2);
        assert_eq!(l2.state, AsbConnectionState::Failed);
    }
}
