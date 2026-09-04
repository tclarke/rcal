//! System-level integration tests for the ZMQ ASB via `get_cal()`.
//!
//! Each test obtains a `Cal` handle and exercises the full
//! writer → RADIO → DISH → reader path with XML serialization.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcal::QName;
use rcal::cal::{Cal, get_cal};
use rcal::uci::CalMessage;
use rcal::uci::base::{MessageListener, TopicQos};

// ── port allocator ────────────────────────────────────────────────────────────

static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(3000);

fn next_port() -> u16 {
    NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

// ── config / bus builders ─────────────────────────────────────────────────────

fn test_config_inproc(names: &[&str]) -> Arc<rcal::calconfig::CalConfig> {
    use rcal::calconfig;
    use rcal::uci::base::UUID;
    const BASE_UUID: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(BASE_UUID).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, names[0].as_bytes());
    let mut transports = String::new();
    for name in names {
        transports.push_str(&format!(
            "\n[[transport]]\nid = \"{name}\"\ntype = \"zmq\"\nuri = \"inproc://{name}\"\n"
        ));
    }
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"{}\"\n{transports}",
        names[0]
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

fn test_config_tcp(port: u16) -> Arc<rcal::calconfig::CalConfig> {
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

async fn make_bus(
    service: &str,
    transport_id: &str,
    config: Arc<rcal::calconfig::CalConfig>,
    logger: slog::Logger,
) -> Cal {
    get_cal(service, Some(transport_id), config, logger)
        .await
        .unwrap()
}

// ── message type ──────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
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

/// Three logical clients share one `Cal` (one RADIO socket).
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
    let name = "shared-bus-v2";
    let config = test_config_inproc(&[name]);
    let mut bus = make_bus("Sys", name, config, logger).await;

    let mut a_writer = bus
        .create_writer::<IntMsg>("data", TopicQos::default())
        .unwrap();

    let mut b_reader = bus
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();
    let mut b_writer = bus
        .create_writer::<IntMsg>("status", TopicQos::default())
        .unwrap();

    let c_data_log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut c_data_reader = bus
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();
    c_data_reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&c_data_log),
        }))
        .unwrap();
    let mut c_status_reader = bus
        .create_reader::<IntMsg>("status", TopicQos::default())
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

/// Three `Cal` instances, each with its own RADIO, simulating separate processes.
///
/// Topology (each letter = its own inproc URI):
///   A — writer on "data"
///   B — polling reader on "data" from A + writer on "status"
///   C — callback reader on "data" from A + polling reader on "status" from B
///
/// Cross-connections via `add_receive_peer`.
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_three_clients_separate_radios() {
    let names = ["sep-client-a2", "sep-client-b2", "sep-client-c2"];
    let config = test_config_inproc(&names);

    let mut bus_a = make_bus("ClientA", names[0], config.clone(), logger.clone()).await;
    let mut bus_b = make_bus("ClientB", names[1], config.clone(), logger.clone()).await;
    let mut bus_c = make_bus("ClientC", names[2], config.clone(), logger.clone()).await;

    bus_b.add_receive_peer(format!("inproc://{}", names[0]));
    bus_c.add_receive_peer(format!("inproc://{}", names[0]));
    bus_c.add_receive_peer(format!("inproc://{}", names[1]));

    let mut a_writer = bus_a
        .create_writer::<IntMsg>("data", TopicQos::default())
        .unwrap();

    let mut b_reader = bus_b
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();
    let mut b_writer = bus_b
        .create_writer::<IntMsg>("status", TopicQos::default())
        .unwrap();

    let c_data_log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut c_data_reader = bus_c
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();
    c_data_reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&c_data_log),
        }))
        .unwrap();
    let mut c_status_reader = bus_c
        .create_reader::<IntMsg>("status", TopicQos::default())
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

// ── system test: real multi-process ──────────────────────────────────────────

/// Verifies `add_receive_peer` across two real OS processes.
///
/// Topology:
///   Sender process (zmq_peer_sender) — RADIO on port_s, publishes 3 messages
///   This process                     — DISH connecting to port_s via `add_receive_peer`
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_multiprocess_receive_peer() {
    let port_s = next_port();
    let port_r = next_port();

    let sender_bin = env!("CARGO_BIN_EXE_zmq_peer_sender");
    let mut child = std::process::Command::new(sender_bin)
        .args([port_s.to_string(), "3".to_string()])
        .spawn()
        .expect("failed to spawn zmq_peer_sender");

    let config = test_config_tcp(port_r);
    let mut bus = make_bus("Receiver", "T", config, logger).await;
    bus.add_receive_peer(format!("tcp://127.0.0.1:{port_s}"));

    let mut reader = bus
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();

    // read() uses cvar.wait_timeout which blocks the tokio executor thread, preventing
    // the reader task from running. Sleep long enough for the sender process to start,
    // bind, publish, and for the reader task to buffer all messages before we call read().
    tokio::time::sleep(Duration::from_millis(2000)).await;

    let timeout = Duration::from_millis(500);
    let m1 = reader.read(Some(timeout)).unwrap().unwrap();
    let m2 = reader.read(Some(timeout)).unwrap().unwrap();
    let m3 = reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!(
        [m1.value, m2.value, m3.value],
        [1, 2, 3],
        "multiprocess delivery order"
    );
    assert!(reader.read_no_wait().unwrap().is_none());

    child.wait().unwrap();
    bus.close().unwrap();
}
