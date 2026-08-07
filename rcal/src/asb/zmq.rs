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

use crate::uci::{CalResult, CalError, CalErrorKind};
use crate::uci::base::{ServiceUuids, UUID};
use super::{AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener};

pub const ZMQ_ASB_ID: &str = "zmq";

pub struct ZmqAsb {
    service_id: String,
    uuids: ServiceUuids,

    out_conns: HashMap<String, PubSocket>,
    in_conns: HashMap<String, SubSocket>,

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

            logger,
        }
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

    fn connection_status(&self) -> AsbStatus {
        AsbStatus::new(AsbConnectionState::Inoperable, String::from("Incomplete"))
    }

    fn register_status_listener(
        &mut self,
        _listener: Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::register_status_listener()");
        error!(self.logger, "Not implemented!");
        Err(CalError::new(CalErrorKind::ImplementationError{kind: None}, String::from("Not implemented")))
    }

    fn unregister_status_listener(
        &mut self,
        _listener: &Arc<dyn AsbStatusListener>,
    ) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::unregister_status_listener()");
        error!(self.logger, "Not implemented!");
        Err(CalError::new(CalErrorKind::ImplementationError{kind: None}, String::from("Not implemented")))
    }

    fn close(&mut self) -> CalResult<()> {
        trace!(self.logger, "ZmqAsb::close()");
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
}
