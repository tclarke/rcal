//! System-level integration tests for the ActiveMQ ASB via `get_cal()`.
//!
//! Requires a running ActiveMQ Classic or Artemis broker on localhost:61613.
//! Run with:
//!   cargo test --features activemq --test activemq_asb_system

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcal::QName;
use rcal::cal::{Cal, create_cal};
use rcal::uci::CalMessage;
use rcal::uci::base::{MessageListener, TopicQos};

const BROKER_HOST: &str = "localhost";
const BROKER_PORT: u16 = 61613;

fn require_broker() {
    std::net::TcpStream::connect((BROKER_HOST, BROKER_PORT)).unwrap_or_else(|_| {
        panic!(
            "ActiveMQ broker not reachable at {BROKER_HOST}:{BROKER_PORT}. \
             Start a broker (e.g. `docker run -p 61613:61613 apache/activemq-classic`) \
             before running these tests."
        )
    });
}

fn make_config() -> std::sync::Arc<rcal::calconfig::CalConfig> {
    use rcal::calconfig;
    use rcal::uci::base::UUID;
    const NS: &str = "6ef79d81-8a79-4750-9c6a-e5e50a30f81b";
    let ns = UUID::parse_str(NS).unwrap();
    let sys_uuid = UUID::generate_v3(&ns, format!("{BROKER_HOST}:{BROKER_PORT}").as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"TestAmq\"\n\
         \n[[transport]]\nid = \"TestAmq\"\ntype = \"activemq\"\nuri = \"stomp://{BROKER_HOST}:{BROKER_PORT}\"\n"
    );
    std::sync::Arc::new(calconfig::parse_config(&toml).unwrap())
}

async fn make_bus(service: &str, logger: slog::Logger) -> Cal {
    create_cal(service, None::<&str>, make_config(), logger)
        .await
        .unwrap()
}

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

struct CollectListener {
    received: Arc<Mutex<Vec<i32>>>,
}

impl MessageListener<IntMsg> for CollectListener {
    fn on_message(&self, msg: &Arc<IntMsg>) {
        self.received.lock().unwrap().push(msg.value);
    }
}

/// Basic writer → reader round-trip via the broker.
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_basic_pub_sub() {
    require_broker();
    let mut bus = make_bus("BasicPubSub", logger).await;

    let mut writer = bus
        .create_writer::<IntMsg>("data", TopicQos::default())
        .unwrap();
    let mut reader = bus
        .create_reader::<IntMsg>("data", TopicQos::default())
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    writer.write(&IntMsg { value: 42 }).unwrap();

    let msg = reader.read(Some(Duration::from_secs(2))).unwrap();
    bus.close().unwrap();
    // Sleep briefly so reader task processes shutdown_rx.changed() and exits cleanly
    // before drop(reader) calls task.abort(). Without this, abort fires while
    // transport.next() has a pending IOCP read; the 2s condvar timeout fires and the
    // runtime then hangs ~60s for the broker inactivity timeout on Windows.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(reader);
    drop(writer);
    drop(bus);
    // Yield for IOCP driver to dequeue any ERROR_OPERATION_ABORTED completions.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(msg.expect("no message received within 2s").value, 42);
}

/// Multiple messages, polling mode — order preserved.
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_ordered_poll() {
    require_broker();
    let mut bus = make_bus("OrderedPoll", logger).await;

    let mut writer = bus
        .create_writer::<IntMsg>("ordered", TopicQos::default())
        .unwrap();
    let mut reader = bus
        .create_reader::<IntMsg>("ordered", TopicQos::default())
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    for v in [1, 2, 3] {
        writer.write(&IntMsg { value: v }).unwrap();
    }

    let timeout = Duration::from_secs(2);
    let v1 = reader.read(Some(timeout)).unwrap();
    let v2 = reader.read(Some(timeout)).unwrap();
    let v3 = reader.read(Some(timeout)).unwrap();
    let no_more = reader.read_no_wait().unwrap();
    bus.close().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(reader);
    drop(writer);
    drop(bus);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        [
            v1.expect("msg 1 not received").value,
            v2.expect("msg 2 not received").value,
            v3.expect("msg 3 not received").value,
        ],
        [1, 2, 3]
    );
    assert!(no_more.is_none());
}

/// Callback mode — listener receives messages.
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_callback_mode() {
    require_broker();
    let mut bus = make_bus("CallbackMode", logger).await;

    let mut writer = bus
        .create_writer::<IntMsg>("cbdata", TopicQos::default())
        .unwrap();
    let received: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut reader = bus
        .create_reader::<IntMsg>("cbdata", TopicQos::default())
        .unwrap();
    reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&received),
        }))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    writer.write(&IntMsg { value: 10 }).unwrap();
    writer.write(&IntMsg { value: 20 }).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let msgs = received.lock().unwrap().clone();
    bus.close().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(reader);
    drop(writer);
    drop(bus);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(msgs, [10, 20]);
}

/// Reliable QoS — uses client-individual ACK (CAL-005434).
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_reliable_delivery() {
    require_broker();
    use rcal::cal::{Reliability, TopicQos};
    let mut bus = make_bus("ReliableTest", logger).await;

    let qos = TopicQos {
        reliability: Reliability::Reliable,
        ..Default::default()
    };
    let mut writer = bus
        .create_writer::<IntMsg>("reliable", qos.clone())
        .unwrap();
    let mut reader = bus.create_reader::<IntMsg>("reliable", qos).unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    writer.write(&IntMsg { value: 99 }).unwrap();

    let msg = reader.read(Some(Duration::from_secs(2))).unwrap();
    bus.close().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop(reader);
    drop(writer);
    drop(bus);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(msg.expect("no message received within 2s").value, 99);
}

/// Status listener is called immediately on registration (CAL-016366).
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_status_listener_immediate_call() {
    require_broker();
    use rcal::asb::{AsbStatus, AsbStatusListener};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Counter(Arc<AtomicU32>);
    impl AsbStatusListener for Counter {
        fn on_status_change(&self, _: &AsbStatus) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    let mut bus = make_bus("StatusListener", logger).await;
    let count = Arc::new(AtomicU32::new(0));
    bus.register_status_listener(Arc::new(Counter(Arc::clone(&count))))
        .unwrap();

    assert!(
        count.load(Ordering::SeqCst) >= 1,
        "listener must be called immediately on registration"
    );
    bus.close().unwrap();
}
