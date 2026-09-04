use std::sync::Arc;

use slog::error;

use rcal::cal::get_cal;
use rcal::service::AbstractService;

use adsb_sim::AdsbSimService;

#[rcal_macros::rcal_main]
async fn main() {
    let service_name = "adsb_sim";
    let rcal_config = Arc::new(rcal_config);

    let cal = get_cal(
        service_name,
        None::<String>,
        rcal_config.clone(),
        root_logger.clone(),
    )
    .await
    .unwrap_or_else(|err| {
        error!(root_logger, "Unable to obtain CAL instance"; "error" => %err);
        std::process::exit(1);
    });

    let mut svc = AdsbSimService::new(cal, rcal_config.clone(), root_logger.clone())
        .unwrap_or_else(|err| {
            error!(root_logger, "AdsbSimService init failed"; "error" => %err);
            std::process::exit(2);
        });

    if let Err(e) = svc.activate() {
        error!(root_logger, "AdsbSimService activate failed"; "error" => %e);
        std::process::exit(3);
    }

    tokio::signal::ctrl_c().await.ok();

    if let Err(e) = svc.deactivate() {
        error!(root_logger, "AdsbSimService deactivate failed"; "error" => %e);
    }
}
