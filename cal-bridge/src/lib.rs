//! A service which acts as a bridge betwen multiple cal instantiations.
//!
//! It's basic function just copies all messages between busses
//! Configuring individual topics will only forward messages on those topics.
//!
//! # Configuration
//!
//! The service section should point to 1 cal instance. Specify the other cal instances
//! with `service.bridge = ["a", "b"]`
//!
//! # Running
//!
//! Create a suitable `CALContig.toml` (see example in repo) and run from that
//! directory or set RCAL_CONFIG to point to the toml file.
//!
//! ```text
//! cargo run -- cal-bridge
//! ```
#![allow(dead_code)]


use std::sync::Arc;

use anyhow::{anyhow, Result};
use rcal::cal::AbstractCal;
use rcal::uci::{self, CalError, CalImplementationErrorKind};
use rcal::{calconfig::CalConfig, service::ServiceLifecycleState};
use rcal:: service::AbstractService;
use slog::{Logger, warn};

pub struct CalBridgeService {
    logger: Logger,
    config: Arc<CalConfig>,
    service_name: String,
    state: ServiceLifecycleState,
    cals: Vec<dyn AbstractCal>,
}

impl CalBridgeService {
    pub fn new(service_name: String, config: Arc<CalConfig>, logger: Logger) -> Result<Self> {
        // Check that it's valid here so we can just unwrap() the Service everywhere else
        let _service_config = config.get_service(service_name.as_str())
            .ok_or(anyhow!("Service does not exist in config"))?;
        Ok(CalBridgeService {
            logger,
            config,
            service_name,
            state: ServiceLifecycleState::Inactive,
            cals: Vec::new(),
        })
    }
}

impl AbstractService for CalBridgeService {
    fn system_id(&self) -> &str {
        self.config.system.id.as_str()
    }

    fn service_id(&self) -> &str {
        self.config.get_service(self.service_name.as_str()).unwrap().id.as_str()
    }

    fn subsystem_ids(&self) -> Option<&[rcal::uci::base::UUID]> {
        None
    }

    fn lifecycle_state(&self) -> ServiceLifecycleState {
        self.state
    }

    fn activate(&mut self) -> rcal::uci::CalResult<()> {
        if self.state == ServiceLifecycleState::Active {
            warn!(self.logger, "Service is already active");
        }

        // Create the CALs
        let service = self.config.get_service(self.service_name.as_str()).unwrap();
        let main_transport = self.config.get_transport_for_service(self.service_name.as_str()).ok_or(|| {
                CalError::new_impl(CalImplementationErrorKind::ConfigError, "Service transport is invalid.")
            })?;
        let bridge_names: Vec<String> = service.get_option("bridge").unwrap_or_default();
        if bridge_names.is_empty() {
            return Err(CalError::new_impl(CalImplementationErrorKind::ConfigError, "Must be at least 1 service.bridge entry"));
        }
        self.cals.push(uci::get_cal(self.service_name, main_transport.id, self.config, self.logger.clone())?);

        for bridge in bridge_names.iter() {
            self.cals.push(uci::get_cal(self.service_name, bridge.as_stR(), self.config, self.logger.clone())?);
        }

        self.state = ServiceLifecycleState::Active;
        Ok(())
    }

    fn deactivate(&mut self) -> rcal::uci::CalResult<()> {
        if self.state == ServiceLifecycleState::Inactive {
            warn!(self.logger, "Service is already inactive");
        }
        self.state = ServiceLifecycleState::Inactive;
        Ok(())
    }

    fn reset(&mut self) -> rcal::uci::CalResult<()> {
        todo!()
    }
}
