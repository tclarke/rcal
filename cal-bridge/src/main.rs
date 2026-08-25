//! Main program for the cal-bridge service.
//!
//! see [`CalBridgeService`] for more information.

use std::sync::Arc;

use clap::{Parser};
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

    let mut tasks = tokio::task::JoinSet::new();
    debug!(root_logger, "Found {} service(s)", args.service_name.len());
    for sname in args.service_name.iter() {
        info!(root_logger, "Starting Cal Bridge service: {sname}");
        let mut service = CalBridgeService::new(sname.clone(), config.clone(), root_logger.clone()).expect("Can't create service");
        tasks.spawn(async move { service.activate().await });
    }
    while let Some(res) = tasks.join_next().await {
        if let Err(e) = res {
            error!(root_logger, "{}", e);
        }
    }
}
