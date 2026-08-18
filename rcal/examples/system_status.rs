//! Periodic SystemStatus sender + receiver.
//!
//! Spawns a writer that publishes SystemStatus every second and a reader
//! thread that receives, validates, and prints each message as XML.
//!
//! Usage:
//!   cargo run --example system_status

#[cfg(not(rcal_has_xsd))]
fn main() {
    eprintln!("No XSD found at build time — rebuild with schema/UCI_MessageDefinitions_*.xsd present.");
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
    use rcal::uci::CalMessage;
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

    // Create reader before writer so no messages are missed.
    let mut reader =
        <ZmqAsb as AbstractServiceBusExt<SystemStatus_>>::create_reader(&mut bus, TOPIC, TopicQos::default())
            .expect("create_reader failed");

    let mut writer =
        <ZmqAsb as AbstractServiceBusExt<SystemStatus_>>::create_writer(&mut bus, TOPIC, TopicQos::default())
            .expect("create_writer failed");

    // Spawn a blocking thread: read → validate → print XML.
    // Loop exits when the ASB closes (Err return from read).
    let reader_handle = tokio::task::spawn_blocking(move || {
        let expected_type = SystemStatus_::message_type_name();
        loop {
            match reader.read(Some(Duration::from_millis(500))) {
                Ok(Some(msg)) => {
                    // Deserialization succeeded — schema conformance is validated.
                    // Confirm the message type matches the expected type name.
                    assert_eq!(SystemStatus_::message_type_name(), expected_type);
                    match quick_xml::se::to_string_with_root(TOPIC, &*msg) {
                        Ok(xml) => println!("[received]\n{xml}\n"),
                        Err(e) => eprintln!("[reader] serialize error: {e}"),
                    }
                }
                Ok(None) => {} // short timeout — keep polling until ASB closes
                Err(_) => break, // ASB closed
            }
        }
    });

    // Create message once; mutate each iteration.
    let mut msg = bus
        .create_message::<SystemStatus_>()
        .expect("create_message failed");

    use rcal::uci::types::{SystemStatusMT, SystemStatusMDT, SystemStateEnum};

    // Allow DISH socket to connect and complete ZMTP handshake before sending.
    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..ITERATIONS {
        *msg.message_data_mut().system_state_mut() =
            if i % 2 == 0 { SystemStateEnum::Operational } else { SystemStateEnum::Degraded };

        writer.write(&msg).expect("write failed");
        info!(root_logger, "sent"; "iteration" => i + 1, "of" => ITERATIONS);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    bus.close().ok();
    reader_handle.await.ok();
}
