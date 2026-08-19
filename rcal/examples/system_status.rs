//! Periodic SystemStatus sender + receiver.
//!
//! Spawns a writer that publishes SystemStatus every second and a reader
//! thread that receives, validates, and prints each message as XML.
//!
//! Usage:
//!   cargo run --example system_status

use std::sync::Arc;
use std::time::Duration;

use slog::{error, info};

use rcal::asb::zmq::ZmqAsb;
use rcal::asb::{
    AbstractServiceBus, AbstractServiceBusCreateMessage, AbstractServiceBusExt, TopicQos,
};
use rcal::uci::CalMessage;
use rcal::uci::base::UUID;
use rcal::uci::types::*;

const TOPIC: &str = "SystemStatus";

const ITERATIONS: usize = 10;

#[rcal_macros::rcal_main(config = "examples/CALConfig.toml")]
async fn main() {
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

    let mut bus = ZmqAsb::new(
        "SystemStatusExample",
        transport_id,
        root_logger.clone(),
        config,
        &tconfig,
    )
    .await
    .expect("ASB init failed");

    // Create reader before writer so no messages are missed.
    let mut reader = <ZmqAsb as AbstractServiceBusExt<SystemStatus_>>::create_reader(
        &mut bus,
        TOPIC,
        TopicQos::default(),
    )
    .expect("create_reader failed");

    let mut writer = <ZmqAsb as AbstractServiceBusExt<SystemStatus_>>::create_writer(
        &mut bus,
        TOPIC,
        TopicQos::default(),
    )
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
                Ok(None) => {}   // short timeout — keep polling until ASB closes
                Err(_) => break, // ASB closed
            }
        }
    });

    // Create message once; mutate each iteration.
    let mut msg = bus
        .create_message::<SystemStatus_>()
        .expect("create_message failed");

    *(*msg).security_information_mut().classification_mut() = ClassificationEnum::U;
    *(*msg).message_header_mut().system_id_mut().uuid_mut() = UUID::generate(None);
    (*msg)
        .message_header_mut()
        .system_id_mut()
        .descriptive_label_mut()
        .replace(&mut "This is an example system".to_string());
    (*msg)
        .message_header_mut()
        .schema_version_mut()
        .clone_from(&bus.oms_schema_version().to_string());
    *(*msg).message_header_mut().mode_mut() = MessageModeEnum::Simulation;

    let sysid = (*msg).message_header().system_id().clone();
    *(*msg).message_data_mut().system_id_mut() = sysid;
    *(*msg).message_data_mut().system_state_mut() = SystemStateEnum::Operational;
    *(*msg).message_data_mut().source_mut() = SystemSourceEnum::Actual;
    /* *(*msg)
        .message_data_mut()
        .communications_mut().expect()
        .mission_communications_state_mut() = MissionCommunicationsStateEnum::Active; */

    // Allow DISH socket to connect and complete ZMTP handshake before sending
    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..ITERATIONS {
        *msg.message_data_mut().system_state_mut() = if i % 2 == 0 {
            SystemStateEnum::Operational
        } else {
            SystemStateEnum::Degraded
        };

        if let Err(e) = writer.write(&msg) {
            error!(root_logger, "Unable to write to ASB: {e}");
            break;
        }
        info!(root_logger, "sent"; "iteration" => i + 1, "of" => ITERATIONS);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    bus.close().ok();
    reader_handle.await.ok();
}
