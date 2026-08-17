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
use rcal::uci::types::SystemStatus_;

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
    use rcal::asb::{AbstractServiceBus, AbstractServiceBusCreateMessage, AbstractServiceBusExt, TopicQos};
    use slog::info;

    let config = Arc::new(rcal_config);
    let transport_id = config
        .system
        .default_transport
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let tconfig = config
        .get_transport(&transport_id)
        .unwrap_or_else(|| panic!("transport '{transport_id}' not in config"))
        .clone();

    let mut bus =
        ZmqAsb::new("SystemStatusExample", transport_id, root_logger.clone(), config, &tconfig)
            .await
            .expect("ASB init failed");

    let mut writer =
        <ZmqAsb as AbstractServiceBusExt<SystemStatus_>>::create_writer(&mut bus, TOPIC, TopicQos::default())
            .expect("create_writer failed");

    // Create message once; mutate each iteration.
    let mut msg = bus
        .create_message::<SystemStatus_>()
        .expect("create_message failed");

    use rcal::uci::types::{SystemStatusMT, SystemStatusMDT, SystemStateEnum};

    for i in 0..ITERATIONS {
        *msg.message_data_mut().system_state_mut() =
            if i % 2 == 0 { SystemStateEnum::Operational } else { SystemStateEnum::Degraded };

        writer.write(&msg).expect("write failed");
        info!(root_logger, "sent"; "iteration" => i + 1, "of" => ITERATIONS);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    bus.close().ok();
}
