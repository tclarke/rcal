//! Main program for the cal-bridge service.
//!
//! see [`CalBridgeService`] for more information.

use std::sync::Arc;

use clap::Parser;
use cal_bridge::CalBridgeService;
use rcal::service::AbstractService;
use slog::{debug, error, info, trace};

#[derive(Parser, Debug)]
struct Args {
    /// Zero or more services to start. If not specified "CalBridge" will be started.
    service_name: Vec<String>,
}

#[rcal_macros::rcal_main]
async fn main() {
    trace!(root_logger, "main()");
    let mut args = Args::parse();

    if args.service_name.is_empty() {
        args.service_name.push("CalBridge".into());
    }
    let config = Arc::new(rcal_config);

    let mut services: Vec<CalBridgeService> = Vec::new();
    debug!(root_logger, "Found {} service(s)", args.service_name.len());
    for sname in args.service_name.iter() {
        info!(root_logger, "Starting Cal Bridge service: {sname}");
        let mut service = CalBridgeService::new(sname.clone(), config.clone(), root_logger.clone())
            .expect("Can't create service");
        if let Err(e) = service.activate().await {
            error!(root_logger, "Failed to activate {sname}: {e}");
            continue;
        }
        services.push(service);
    }

    if let Err(e) = tokio::signal::ctrl_c().await {
        error!(root_logger, "Signal handler error: {}", e);
    }
    info!(root_logger, "Shutdown signal received, deactivating services");
    for service in services.iter_mut() {
        if let Err(e) = service.deactivate().await {
            error!(root_logger, "Deactivate error: {e}");
        }
    }
}
