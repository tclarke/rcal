//! Integration tests for QoS settings: TimeBasedFilter, Expiration, MessageBuffer (reader).
//!
//! These tests exercise QoS through the public API on real ZMQ sockets.
//! The writer-buffer test lives in `asb/zmq.rs` unit tests because the
//! `#[cfg(test)]` write gate is not visible to integration test binaries.

use std::sync::Arc;
use std::time::Duration;

use rcal::QName;
use rcal::asb::zmq::ZmqAsb;
use rcal::uci::CalMessage;
use rcal::uci::base::{AbstractServiceBus, AbstractServiceBusExt, TopicQos};

// ── shared port allocator ─────────────────────────────────────────────────────

static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(1000);

fn next_port() -> u16 {
    NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

// ── config + bus builder ──────────────────────────────────────────────────────

fn test_config(port: u16) -> Arc<rcal::calconfig::CalConfig> {
    use rcal::calconfig;
    use rcal::uci::base::UUID;
    let ns = UUID::parse_str("6ef79d81-8a79-4750-9c6a-e5e50a30f81b").unwrap();
    let sys_uuid = UUID::generate_v3(&ns, format!("qos{port}").as_bytes());
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
struct QosMsg {
    value: String,
}

impl CalMessage for QosMsg {
    fn message_type_name() -> rcal::QName {
        QName::new(Some("test"), "QosMsg")
    }
    fn cal_create() -> Self {
        Self {
            value: String::new(),
        }
    }
}

// ── TimeBasedFilter ───────────────────────────────────────────────────────────

/// Sends a burst, verifies only the first passes, then verifies the filter resets
/// after `min_separation` has elapsed.
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_qos_time_based_filter() {
    let mut bus = make_bus("TBF", next_port(), logger).await;

    let reader_qos = TopicQos {
        time_based_filter: Some(rcal::uci::base::TimeBasedFilter {
            min_separation: Duration::from_millis(100),
        }),
        ..TopicQos::default()
    };
    let mut reader =
        <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_reader(&mut bus, "t", reader_qos)
            .unwrap();
    let mut writer = <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_writer(
        &mut bus,
        "t",
        TopicQos::default(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Burst: only the first message should pass the filter
    for i in 0..5u32 {
        writer
            .write(&QosMsg {
                value: format!("burst{i}"),
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let first = reader.read_no_wait().unwrap();
    assert_eq!(
        first.as_deref().map(|m| m.value.as_str()),
        Some("burst0"),
        "first message must pass filter"
    );
    assert!(
        reader.read_no_wait().unwrap().is_none(),
        "burst messages within min_separation must be dropped"
    );

    // After min_separation, the filter should reset
    tokio::time::sleep(Duration::from_millis(120)).await;
    writer
        .write(&QosMsg {
            value: "after_reset".into(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let reset = reader.read_no_wait().unwrap();
    assert_eq!(
        reset.as_deref().map(|m| m.value.as_str()),
        Some("after_reset"),
        "message after min_separation must pass filter"
    );

    bus.close().unwrap();
}

// ── Expiration ────────────────────────────────────────────────────────────────

/// Writes messages, sleeps past max_age, verifies they expired.
/// Then writes fresh messages and verifies they survive.
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_qos_expiration() {
    let mut bus = make_bus("Exp", next_port(), logger).await;

    let reader_qos = TopicQos {
        expiration: Some(rcal::uci::base::Expiration {
            max_age: Duration::from_millis(50),
        }),
        ..TopicQos::default()
    };
    let mut reader =
        <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_reader(&mut bus, "t", reader_qos)
            .unwrap();
    let mut writer = <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_writer(
        &mut bus,
        "t",
        TopicQos::default(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Write 3 messages, wait for them to expire
    for i in 0..3u32 {
        writer
            .write(&QosMsg {
                value: format!("old{i}"),
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(80)).await; // > max_age

    assert!(
        reader.read_no_wait().unwrap().is_none(),
        "expired messages must be discarded"
    );

    // Write fresh messages, read before they expire
    writer
        .write(&QosMsg {
            value: "fresh0".into(),
        })
        .unwrap();
    writer
        .write(&QosMsg {
            value: "fresh1".into(),
        })
        .unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await; // < max_age

    let r0 = reader.read_no_wait().unwrap();
    let r1 = reader.read_no_wait().unwrap();
    assert_eq!(
        r0.as_deref().map(|m| m.value.as_str()),
        Some("fresh0"),
        "fresh message must survive"
    );
    assert_eq!(r1.as_deref().map(|m| m.value.as_str()), Some("fresh1"));
    assert!(reader.read_no_wait().unwrap().is_none());

    bus.close().unwrap();
}

// ── MessageBuffer (reader) ────────────────────────────────────────────────────

/// Floods 5 messages into a cap-3 reader buffer; verifies oldest 2 dropped.
#[rcal_macros::init_test_logger]
#[tokio::test]
async fn test_qos_reader_buffer() {
    let mut bus = make_bus("RBuf", next_port(), logger).await;

    let reader_qos = TopicQos {
        reader_buffer: Some(rcal::uci::base::MessageBuffer { max_messages: 3 }),
        ..TopicQos::default()
    };
    let mut reader =
        <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_reader(&mut bus, "t", reader_qos)
            .unwrap();
    let mut writer = <ZmqAsb as AbstractServiceBusExt<QosMsg>>::create_writer(
        &mut bus,
        "t",
        TopicQos::default(),
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    for i in 0..5u32 {
        writer
            .write(&QosMsg {
                value: format!("msg{i}"),
            })
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cap-3 buffer: msg0 and msg1 dropped (oldest); msg2, msg3, msg4 survive
    let r0 = reader.read_no_wait().unwrap();
    let r1 = reader.read_no_wait().unwrap();
    let r2 = reader.read_no_wait().unwrap();
    let r3 = reader.read_no_wait().unwrap();
    assert_eq!(r0.as_deref().map(|m| m.value.as_str()), Some("msg2"));
    assert_eq!(r1.as_deref().map(|m| m.value.as_str()), Some("msg3"));
    assert_eq!(r2.as_deref().map(|m| m.value.as_str()), Some("msg4"));
    assert!(
        r3.is_none(),
        "buffer must be empty after max_messages reads"
    );

    bus.close().unwrap();
}
