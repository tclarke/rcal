//! System-level integration tests for [`ZmqAsb`] / [`AbstractServiceBusExt`].
//!
//! Each test creates real `ZmqAsb` instances and exercises the full
//! writer → RADIO → DISH → reader path with XML serialization.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcal::asb::zmq::ZmqAsb;
use rcal::uci::CalMessage;
use rcal::uci::base::{AbstractServiceBus, AbstractServiceBusExt, MessageListener, TopicQos};

// ── shared port allocator ─────────────────────────────────────────────────────

static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(56200);

fn next_port() -> u16 {
    NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

// ── config builder ────────────────────────────────────────────────────────────

fn test_config(port: u16) -> Arc<rcal::calconfig::CalConfig> {
    use rcal::calconfig;
    use rcal::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, port.to_string().as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"T\"\n\
         \n[[transport]]\nid = \"T\"\ntype = \"zmq\"\nuri = \"tcp://127.0.0.1:{port}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

async fn make_bus(label: &str, port: u16, logger: slog::Logger) -> ZmqAsb {
    let config = test_config(port);
    let tconfig = config.get_transport("T").unwrap();
    ZmqAsb::new(label, "T", logger, config.clone(), tconfig)
        .await
        .unwrap()
}

// ── message type ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct IntMsg {
    value: i32,
}

impl CalMessage for IntMsg {
    fn message_type_name() -> &'static str {
        "test.IntMsg"
    }
    fn cal_create() -> Self {
        Self { value: 0 }
    }
}

// ── callback helper ───────────────────────────────────────────────────────────

struct CollectListener {
    received: Arc<Mutex<Vec<i32>>>,
}

impl MessageListener<IntMsg> for CollectListener {
    fn on_message(&self, msg: &Arc<IntMsg>) {
        self.received.lock().unwrap().push(msg.value);
    }
}

// ── system test: single shared bus ────────────────────────────────────────────

/// Three logical clients share one `ZmqAsb` (one RADIO socket).
///
/// Topology:
///   A — writer on "data"
///   B — polling reader on "data" + writer on "status"
///   C — callback reader on "data" + polling reader on "status"
///
/// Verifies fanout to two DISH sockets and simultaneous polling + callback modes.
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_three_clients_shared_bus() {
    let mut bus = make_bus("Sys", next_port(), logger).await;

    let mut a_writer = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_writer(
        &mut bus,
        "data",
        TopicQos::default(),
    )
    .unwrap();

    let mut b_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus,
        "data",
        TopicQos::default(),
    )
    .unwrap();
    let mut b_writer = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_writer(
        &mut bus,
        "status",
        TopicQos::default(),
    )
    .unwrap();

    let c_data_log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut c_data_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus,
        "data",
        TopicQos::default(),
    )
    .unwrap();
    c_data_reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&c_data_log),
        }))
        .unwrap();
    let mut c_status_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus,
        "status",
        TopicQos::default(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    a_writer.write(&IntMsg { value: 10 }).unwrap();
    a_writer.write(&IntMsg { value: 20 }).unwrap();
    a_writer.write(&IntMsg { value: 30 }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let timeout = Duration::from_millis(100);
    let b1 = b_reader.read(Some(timeout)).unwrap().unwrap();
    let b2 = b_reader.read(Some(timeout)).unwrap().unwrap();
    let b3 = b_reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!([b1.value, b2.value, b3.value], [10, 20, 30], "B poll order");
    assert!(b_reader.read_no_wait().unwrap().is_none());

    assert_eq!(
        *c_data_log.lock().unwrap(),
        [10, 20, 30],
        "C callback values"
    );

    b_writer.write(&IntMsg { value: 1 }).unwrap();
    b_writer.write(&IntMsg { value: 2 }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let s1 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    let s2 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!([s1.value, s2.value], [1, 2], "C status poll order");
    assert!(c_status_reader.read_no_wait().unwrap().is_none());

    bus.close().unwrap();
}

// ── system test: three separate RADIOs ───────────────────────────────────────

/// Three `ZmqAsb` instances, each with its own RADIO, simulating separate processes.
///
/// Topology (each letter = its own TCP port):
///   A (port_a) — writer on "data"
///   B (port_b) — polling reader on "data" from A + writer on "status"
///   C (port_c) — callback reader on "data" from A + polling reader on "status"
///               from B
///
/// Cross-connections via `add_receive_peer`:
///   B.peer_uris = [port_a]
///   C.peer_uris = [port_a, port_b]
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_three_clients_separate_radios() {
    let port_a = next_port();
    let port_b = next_port();
    let port_c = next_port();

    let mut bus_a = make_bus("ClientA", port_a, logger.clone()).await;
    let mut bus_b = make_bus("ClientB", port_b, logger.clone()).await;
    let mut bus_c = make_bus("ClientC", port_c, logger.clone()).await;

    bus_b.add_receive_peer(format!("tcp://127.0.0.1:{port_a}"));
    bus_c.add_receive_peer(format!("tcp://127.0.0.1:{port_a}"));
    bus_c.add_receive_peer(format!("tcp://127.0.0.1:{port_b}"));

    let mut a_writer = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_writer(
        &mut bus_a,
        "data",
        TopicQos::default(),
    )
    .unwrap();

    let mut b_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus_b,
        "data",
        TopicQos::default(),
    )
    .unwrap();
    let mut b_writer = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_writer(
        &mut bus_b,
        "status",
        TopicQos::default(),
    )
    .unwrap();

    let c_data_log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut c_data_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus_c,
        "data",
        TopicQos::default(),
    )
    .unwrap();
    c_data_reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&c_data_log),
        }))
        .unwrap();
    let mut c_status_reader = <ZmqAsb as AbstractServiceBusExt<IntMsg>>::create_reader(
        &mut bus_c,
        "status",
        TopicQos::default(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    a_writer.write(&IntMsg { value: 10 }).unwrap();
    a_writer.write(&IntMsg { value: 20 }).unwrap();
    a_writer.write(&IntMsg { value: 30 }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let timeout = Duration::from_millis(100);
    let b1 = b_reader.read(Some(timeout)).unwrap().unwrap();
    let b2 = b_reader.read(Some(timeout)).unwrap().unwrap();
    let b3 = b_reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!([b1.value, b2.value, b3.value], [10, 20, 30], "B poll order");
    assert!(b_reader.read_no_wait().unwrap().is_none());

    assert_eq!(
        *c_data_log.lock().unwrap(),
        [10, 20, 30],
        "C callback values"
    );

    b_writer.write(&IntMsg { value: 1 }).unwrap();
    b_writer.write(&IntMsg { value: 2 }).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let s1 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    let s2 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!([s1.value, s2.value], [1, 2], "C status poll order");
    assert!(c_status_reader.read_no_wait().unwrap().is_none());

    bus_a.close().unwrap();
    bus_b.close().unwrap();
    bus_c.close().unwrap();
}
