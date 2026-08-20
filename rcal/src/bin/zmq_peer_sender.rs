//! Helper binary for multi-process `add_receive_peer` integration tests.
//!
//! Usage: `zmq_peer_sender <port> <num_messages>`
//!
//! Binds a RADIO socket on `tcp://127.0.0.1:<port>`, waits briefly for
//! receivers to connect, then publishes `<num_messages>` `IntMsg` values
//! (1, 2, …, N) on topic "data" and exits.

use std::sync::Arc;
use std::time::Duration;

use rcal::QName;
use rcal::asb::zmq::ZmqAsb;
use rcal::uci::CalMessage;
use rcal::uci::base::{AbstractServiceBus, AbstractServiceBusExt, TopicQos, UUID};

#[derive(serde::Serialize, serde::Deserialize)]
struct IntMsg {
    value: i32,
}

impl CalMessage for IntMsg {
    fn message_type_name() -> QName {
        QName::new(Some("test"), "IntMsg")
    }
    fn cal_create() -> Self {
        Self { value: 0 }
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args
        .next()
        .expect("usage: zmq_peer_sender <port> <num_messages>")
        .parse()
        .expect("port must be u16");
    let count: i32 = args
        .next()
        .expect("usage: zmq_peer_sender <port> <num_messages>")
        .parse()
        .expect("count must be i32");

    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, port.to_string().as_bytes());
    let toml = format!(
        "[system]\nid = \"SenderProcess\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"T\"\n\
         \n[[transport]]\nid = \"T\"\ntype = \"zmq\"\nuri = \"tcp://127.0.0.1:{port}\"\n"
    );
    let config = Arc::new(rcal::calconfig::parse_config(&toml).unwrap());
    let tconfig = config.get_transport("T").unwrap();

    let logger = slog::Logger::root(slog::Discard, slog::o!());
    let mut bus = ZmqAsb::new("sender", "T", None, logger, config.clone(), tconfig)
        .await
        .unwrap();

    let mut writer = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_writer(
        &mut bus,
        "data",
        TopicQos::default(),
    )
    .unwrap();

    // Allow receiver process to connect before publishing.
    // ponytail: fixed delay, use a ready-signal if startup variance matters
    tokio::time::sleep(Duration::from_millis(1000)).await;

    for i in 1..=count {
        writer.write(&IntMsg { value: i }).unwrap();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    bus.close().unwrap();
}
