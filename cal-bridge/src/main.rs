//! Main program for the cal-bridge service.
//!
//! see [`CalBridgeService`] for more information.

use std::sync::Arc;

use clap::{Parser};
use cal_bridge::CalBridgeService;
use slog::info;

#[derive(Parser, Debug)]
struct Args {
    /// Zero or more services to start. If not specified "CalBridge" will be started.
    service_name: Vec<String>,
}

#[rcal_macros::rcal_main]
async fn main() {
    let mut args = Args::parse();

    if args.service_name.is_empty() {
        args.service_name.push("CalBridge".into());
    }
    let config = Arc::new(rcal_config);

    for sname in args.service_name.iter() {
        info!(root_logger, "Starting Cal Bridge service: {sname}");
        let _service = CalBridgeService::new(sname.clone(), config.clone(), root_logger.clone());
    }
}
//     let transport_id = config
//         .system
//         .default_transport
//         .clone()
//         .unwrap_or_else(|| "default".to_string());
//     let tconfig = config
//         .get_transport(&transport_id)
//         .unwrap_or_else(|| panic!("transport '{transport_id}' not in config"))
//         .clone();

//     let service_id = config.system.id.clone();

//     rcal::uci::base::AbstractCal
//     let mut bus = ZmqAsb::new(
//         "SystemStatusExample",
//         transport_id,
//         root_logger.clone(),
//         config,
//         &tconfig,
//     )
//     .await
//     .expect("ASB init failed");
