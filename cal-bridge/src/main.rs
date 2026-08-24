//! Main program for the cal-bridge service.
//!
//! see [`CalBridgeService`] for more information.

#[rcal_macros::rcal_main]
async fn main() {
    info!(root_logger, "service stopped"; "service" => &service_name);
}
