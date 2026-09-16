//! System-level integration tests for the DDS ASB via `get_cal()`.
//!
//! Uses unique DDS domain IDs (starting at 10) to isolate parallel tests.
//! Domain 0 is reserved for default/production use.
//!
//! Note: DDS discovery uses loopback multicast. If CI blocks multicast,
//! mark tests with `#[ignore]`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rcal::QName;
use rcal::cal::{Cal, get_cal};
use rcal::uci::CalMessage;
use rcal::uci::base::{MessageListener, TopicQos};

// ── domain-ID allocator ───────────────────────────────────────────────────────

static NEXT_DOMAIN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(10);

fn next_domain() -> u32 {
    NEXT_DOMAIN.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

// ── config / bus builders ─────────────────────────────────────────────────────

fn test_config_domain(domain_id: u32) -> Arc<rcal::calconfig::CalConfig> {
    use rcal::calconfig;
    use rcal::uci::base::UUID;
    let ns = UUID::parse_str("6ef79d81-8a79-4750-9c6a-e5e50a30f81b").unwrap();
    let sys_uuid = UUID::generate_v3(&ns, domain_id.to_string().as_bytes());
    let toml = format!(
        "[system]\nid = \"TestSystem\"\nuuid = \"{sys_uuid}\"\ndefault_transport = \"D\"\n\
         \n[[transport]]\nid = \"D\"\ntype = \"dds\"\nuri = \"{domain_id}\"\n"
    );
    Arc::new(calconfig::parse_config(&toml).unwrap())
}

async fn make_bus(
    service: &str,
    config: Arc<rcal::calconfig::CalConfig>,
    logger: slog::Logger,
) -> Cal {
    get_cal(service, Some("D"), config, logger).await.unwrap()
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

// ── tests ─────────────────────────────────────────────────────────────────────

/// Writer and polling reader on the same DDS domain exchange messages.
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_writer_to_polling_reader() {
    let domain = next_domain();
    let config = test_config_domain(domain);
    // Service name includes domain to get a distinct CalFactory entry per test.
    let mut bus = make_bus(&format!("Sys_{domain}"), config, logger).await;
    // Use domain-scoped topic names: dust_dds does not isolate same-process
    // participants by domain ID, so unique names prevent cross-test contamination.
    let topic = format!("data_{domain}");

    let mut writer = bus
        .create_writer::<IntMsg>(&topic, TopicQos::default())
        .unwrap();
    let mut reader = bus
        .create_reader::<IntMsg>(&topic, TopicQos::default())
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    writer.write(&IntMsg { value: 42 }).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let timeout = Duration::from_millis(500);
    let msg = reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!(msg.value, 42);

    bus.close().unwrap();
}

/// Writer and callback reader on the same DDS domain exchange messages.
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_writer_to_callback_reader() {
    let domain = next_domain();
    let config = test_config_domain(domain);
    let mut bus = make_bus(&format!("Sys_{domain}"), config, logger).await;
    let topic = format!("data_{domain}");

    let mut writer = bus
        .create_writer::<IntMsg>(&topic, TopicQos::default())
        .unwrap();
    let received: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut reader = bus
        .create_reader::<IntMsg>(&topic, TopicQos::default())
        .unwrap();
    reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&received),
        }))
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    writer.write(&IntMsg { value: 1 }).unwrap();
    writer.write(&IntMsg { value: 2 }).unwrap();
    writer.write(&IntMsg { value: 3 }).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(*received.lock().unwrap(), [1, 2, 3]);

    bus.close().unwrap();
}

/// Three logical clients share one `Cal` (one DDS participant).
///
/// Topology:
///   A — writer on "data"
///   B — polling reader on "data" + writer on "status"
///   C — callback reader on "data" + polling reader on "status"
#[rcal_macros::init_test_logger]
#[tokio::test(flavor = "multi_thread")]
async fn test_three_clients_shared_bus() {
    let domain = next_domain();
    let config = test_config_domain(domain);
    let mut bus = make_bus(&format!("Sys_{domain}"), config, logger).await;

    let data_topic = format!("data_{domain}");
    let status_topic = format!("status_{domain}");

    let mut a_writer = bus
        .create_writer::<IntMsg>(&data_topic, TopicQos::default())
        .unwrap();

    let mut b_reader = bus
        .create_reader::<IntMsg>(&data_topic, TopicQos::default())
        .unwrap();
    let mut b_writer = bus
        .create_writer::<IntMsg>(&status_topic, TopicQos::default())
        .unwrap();

    let c_data_log: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut c_data_reader = bus
        .create_reader::<IntMsg>(&data_topic, TopicQos::default())
        .unwrap();
    c_data_reader
        .add_listener(Arc::new(CollectListener {
            received: Arc::clone(&c_data_log),
        }))
        .unwrap();
    let mut c_status_reader = bus
        .create_reader::<IntMsg>(&status_topic, TopicQos::default())
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    a_writer.write(&IntMsg { value: 10 }).unwrap();
    a_writer.write(&IntMsg { value: 20 }).unwrap();
    a_writer.write(&IntMsg { value: 30 }).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let timeout = Duration::from_millis(500);
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
    tokio::time::sleep(Duration::from_millis(200)).await;

    let s1 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    let s2 = c_status_reader.read(Some(timeout)).unwrap().unwrap();
    assert_eq!([s1.value, s2.value], [1, 2], "C status poll order");
    assert!(c_status_reader.read_no_wait().unwrap().is_none());

    bus.close().unwrap();
}
