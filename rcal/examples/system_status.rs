//! Periodic SystemStatus sender.
//!
//! Requires `RCAL_XSD_PATH=tests/fixtures/system_status.xsd` at build time
//! and a running ZMQ broker configured via `RCAL_CONFIG` or `CALConfig.toml`.
//!
//! Usage:
//!   RCAL_XSD_PATH=tests/fixtures/system_status.xsd cargo run --example system_status

#[cfg(not(rcal_has_xsd))]
fn main() {
    eprintln!("Set RCAL_XSD_PATH=tests/fixtures/system_status.xsd at build time, then rebuild.");
    std::process::exit(1);
}

#[cfg(rcal_has_xsd)]
use rcal::uci::types::system_status_type::SystemStatusType;

#[cfg(rcal_has_xsd)]
const TOPIC: &str = "SystemStatus";

#[cfg(rcal_has_xsd)]
const ITERATIONS: usize = 10;

#[cfg(rcal_has_xsd)]
#[rcal_macros::rcal_main(config = "examples/CALConfig.toml")]
async fn main() {
    use std::sync::Arc;
    use std::time::Duration;

    use rcal::asb::zmq::ZmqAsb;
    use rcal::asb::{AbstractServiceBusCreateMessage, AbstractServiceBusExt, TopicQos};
    use slog::info;

    let config = Arc::new(rcal_config);
    let transport_id = config
        .system
        .default_transport
        .as_deref()
        .unwrap_or("default");
    let tconfig = config
        .get_transport(transport_id)
        .unwrap_or_else(|| panic!("transport '{transport_id}' not in config"));

    let mut bus =
        ZmqAsb::new("SystemStatusExample", transport_id, root_logger.clone(), config, tconfig)
            .await
            .expect("ASB init failed");

    let mut writer = bus
        .create_writer::<SystemStatusType>(TOPIC, TopicQos::default())
        .expect("create_writer failed");

    // Create message once; mutate each iteration.
    let mut msg = bus
        .create_message::<SystemStatusType>()
        .expect("create_message failed");

    for i in 0..ITERATIONS {
        msg.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        msg.severity = i as u32;

        writer.write(&msg).expect("write failed");
        info!(root_logger, "sent"; "iteration" => i + 1, "of" => ITERATIONS);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    bus.close().ok();
}
