use std::sync::Arc;

use slog::error;

use rcal::asb::zmq::ZmqAsb;
use rcal::service::AbstractService;

use adsb_sim::AdsbSimService;

#[rcal_macros::rcal_main]
async fn main() {
    let config = Arc::new(rcal_config);

    let transport_id = "adsb_sim";
    let tconfig = match config.get_transport(transport_id) {
        Some(t) => t.clone(),
        None => {
            error!(
                root_logger,
                "transport '{}' not found in config", transport_id
            );
            std::process::exit(1);
        }
    };

    let asb = match ZmqAsb::new(
        "adsb_sim",
        transport_id,
        root_logger.clone(),
        Arc::clone(&config),
        &tconfig,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            error!(root_logger, "ZmqAsb init failed"; "error" => %e);
            std::process::exit(1);
        }
    };

    let mut svc = match AdsbSimService::new(asb, Arc::clone(&config), root_logger.clone()) {
        Ok(s) => s,
        Err(e) => {
            error!(root_logger, "AdsbSimService init failed"; "error" => %e);
            std::process::exit(1);
        }
    };

    if let Err(e) = svc.activate() {
        error!(root_logger, "AdsbSimService activate failed"; "error" => %e);
        std::process::exit(1);
    }

    tokio::signal::ctrl_c().await.ok();

    if let Err(e) = svc.deactivate() {
        error!(root_logger, "AdsbSimService deactivate failed"; "error" => %e);
    }
}
