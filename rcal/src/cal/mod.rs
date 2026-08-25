//! CAL layer interfaces — application-facing traits above the pure transport ASB.
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L §5.4–5.8 (topics, messages, writers, readers, QoS)
//! - OMSC-SPC-001 Rev L §5.3 (CAL initialisation, CAL-005201, CAL-005202)

#![allow(dead_code)]
#![warn(missing_docs)]

use crate::asb::AbstractServiceBus;
use crate::calconfig::CalConfig;
use crate::uci::CalMessage;
use crate::uci::base::UUID;
use crate::uci::types::{
    ClassificationEnum, ID_Type as _, MessageModeEnum, OwnerProducerChoiceType_,
};
use crate::uci::{CalError, CalErrorKind, CalResult};
use chrono::Utc;
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "zmq")]
use crate::asb::zmq::{ZMQ_ASB_ID, ZmqAsb};
use crate::asb::file::{FILE_ASB_ID, FileAsb};

// ════════════════════════════════════════════════════════════════════════════
// MessageHeaderDefaults
// ════════════════════════════════════════════════════════════════════════════

/// Default values applied to every message created through a CAL instance.
///
/// Sourced from the CAL configuration file (`[system]` and `[service]`
/// sections) plus the compiled-in schema version string.
#[derive(Debug, Clone)]
pub struct MessageHeaderDefaults {
    /// System UUID for the `MessageHeader.SystemID.UUID` field (required).
    pub system_id: UUID,
    /// Optional service UUID for the `MessageHeader.ServiceID.UUID` field.
    pub service_id: Option<UUID>,
    /// Optional mission UUID for the `MessageHeader.MissionID.UUID` field.
    pub mission_id: Option<UUID>,
    /// Schema version string for `MessageHeader.SchemaVersion`.
    pub schema_version: String,
    /// Message mode for `MessageHeader.Mode`.
    pub mode: MessageModeEnum,
    /// Classification for `SecurityInformation.Classification`.
    pub classification: ClassificationEnum,
    /// OwnerProducer entries for `SecurityInformation.OwnerProducer`.
    pub owner_producer: Vec<OwnerProducerChoiceType_>,
}

// ════════════════════════════════════════════════════════════════════════════
// MessageListener
// ════════════════════════════════════════════════════════════════════════════

/// Callback interface for receiving CAL Messages on a subscribed topic.
///
/// Register with [`AbstractReader::add_listener`]. Remove with
/// [`AbstractReader::remove_listener`] (CERT CAL-005396).
///
/// # CAL-005392 — single shared reference
/// Each registered listener receives the **same** `Arc<M>` exactly once per
/// received message. The inner `M` is immutable for the full duration of the
/// handler invocation (CERT CAL-016046).
///
/// # Thread safety
/// The CAL may dispatch `on_message` from an internal receive thread.
/// Implementations must be `Send + Sync`. If internal mutation is required,
/// use interior mutability (`Mutex`, `AtomicXxx`, etc.).
///
/// # CERT coverage
/// CAL-005379, CAL-005391, CAL-005392, CAL-005396, CAL-016045, CAL-016046
pub trait MessageListener<M: CalMessage>: Send + Sync {
    /// Called once per received message instance (CERT CAL-005392).
    ///
    /// Must not block indefinitely — the CAL removes the message from its
    /// internal buffer only after **all** registered listeners' `on_message`
    /// calls return (CERT CAL-016045).
    fn on_message(&self, message: &Arc<M>);
}

// ════════════════════════════════════════════════════════════════════════════
// RawMessageListener
// ════════════════════════════════════════════════════════════════════════════

/// Callback for raw wire-format bytes received on a subscribed topic.
///
/// Used by bridge/proxy services that forward messages without deserializing
/// them. Register with [`AbstractCal::subscribe_raw`].
pub trait RawMessageListener: Send + Sync {
    /// Called once per received message.
    ///
    /// `topic` is the resolved wire-level topic string.
    /// `payload` is in the same wire format that [`AbstractCal::publish_raw`]
    /// accepts — pass it through unchanged for zero-copy bridging.
    fn on_raw_message(&self, topic: &str, payload: &[u8]);
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractWriter
// ════════════════════════════════════════════════════════════════════════════

/// A topic-bound CAL message publisher (CERT CAL-005368).
///
/// Obtained via [`AbstractCalExt::create_writer`] (CERT CAL-005364).
///
/// # Error conditions on `write()`
/// - `CalErrorKind::TopicUnavailable` — topic connection unavailable (CAL-005369)
/// - `CalErrorKind::ResourcesUnavailable` — platform resources exhausted (CAL-016043)
/// - `CalErrorKind::InvalidState` — ASB state prohibits writes (Table 5.9-2)
///
/// # CERT coverage
/// CAL-005364, CAL-005368, CAL-005369, CAL-016043
pub trait AbstractWriter<M: CalMessage>: Send + Sync {
    /// The Client Topic string this writer is bound to (CERT CAL-005368).
    fn topic(&self) -> &str;

    /// Publishes `message` on the bound Client Topic.
    ///
    /// Returns `Err(TopicUnavailable)` if the topic is unreachable
    /// (CERT CAL-005369), `Err(ResourcesUnavailable)` if transport resources
    /// are exhausted (CERT CAL-016043), or `Err(InvalidState)` if the current
    /// ASB state prohibits writes (Table 5.9-2).
    fn write(&mut self, message: &M) -> CalResult<()>;

    /// Shuts down this writer and releases all associated resources.
    ///
    /// `Box<Self>` consuming receiver prevents use-after-close at the type
    /// level, consistent with [`AbstractServiceBus::close`].
    fn close(self: Box<Self>) -> CalResult<()>;
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractReader
// ════════════════════════════════════════════════════════════════════════════

/// A topic-bound CAL message subscriber (CERT CAL-005378).
///
/// Obtained via [`AbstractCalExt::create_reader`] (CERT CAL-005374).
/// The topic connection and message buffering are established at creation
/// time, before this call returns (CERT CAL-005394, CAL-016044).
///
/// # Callback vs polling — mutually exclusive (CAL-016050)
///
/// **Callback mode**: register one or more [`MessageListener`]s via
/// `add_listener`. Each received message is dispatched to all listeners
/// exactly once, then removed from the buffer (CAL-016045).
/// Calling `read` or `read_no_wait` while any listener is registered returns
/// `Err(OperationNotPermitted)` (CAL-016050).
///
/// **Polling mode**: call `read` or `read_no_wait` with no listeners
/// registered. Each call removes the message from the buffer (CAL-016052).
///
/// # CERT coverage
/// CAL-005374, CAL-005378, CAL-005379, CAL-005380, CAL-005391, CAL-005392,
/// CAL-005394, CAL-005396, CAL-016044, CAL-016045, CAL-016046, CAL-016049,
/// CAL-016050, CAL-016052
pub trait AbstractReader<M: CalMessage>: Send + Sync {
    /// The Client Topic string this reader is bound to (CERT CAL-005378).
    fn topic(&self) -> &str;

    // ── Callback interface ────────────────────────────────────────────────

    /// Registers a message listener (CERT CAL-005391).
    fn add_listener(&mut self, listener: Arc<dyn MessageListener<M>>) -> CalResult<()>;

    /// Unregisters a previously registered listener (CERT CAL-005396).
    fn remove_listener(&mut self, listener: &Arc<dyn MessageListener<M>>) -> CalResult<()>;

    // ── Polling interface ─────────────────────────────────────────────────

    /// Blocking read — waits until a message arrives or the call is released.
    ///
    /// `timeout: None` blocks indefinitely until a message or a close event.
    /// Returns `Err(OperationNotPermitted)` if any listener is registered
    /// (CERT CAL-016050).
    fn read(&mut self, timeout: Option<Duration>) -> CalResult<Option<Arc<M>>>;

    /// Non-blocking read — returns a buffered message if one is available.
    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>>;

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Shuts down this reader and releases all associated resources.
    fn close(self: Box<Self>) -> CalResult<()>;
}

// ════════════════════════════════════════════════════════════════════════════
// QoS settings
// ════════════════════════════════════════════════════════════════════════════

/// Reliability policy for a Client Topic (CERT CAL-005434, CAL-016076).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reliability {
    /// Best-effort delivery — messages may be dropped (default).
    #[default]
    BestEffort,
    /// Reliable delivery — unacknowledged messages are retransmitted in order.
    Reliable,
}

/// Minimum inter-arrival gap for accepted messages (CERT CAL-005431).
///
/// The reader silently drops any message received within `min_separation` of
/// the previously accepted message.
#[derive(Debug, Clone)]
pub struct TimeBasedFilter {
    /// Minimum time that must elapse between two consecutively accepted messages.
    pub min_separation: Duration,
}

/// Maximum lifetime for a buffered message (CERT CAL-005437).
///
/// Messages older than `max_age` are removed from the receive buffer.
#[derive(Debug, Clone)]
pub struct Expiration {
    /// Age after which a buffered message is discarded.
    pub max_age: Duration,
}

/// Bounded message buffer (CERT CAL-005444, CAL-005445, CAL-015746, CAL-016079).
///
/// Used for both the writer-side send buffer (`TopicQos::writer_buffer`) and
/// the reader-side receive buffer (`TopicQos::reader_buffer`). When the count
/// of buffered messages exceeds `max_messages`, the oldest is dropped.
#[derive(Debug, Clone)]
pub struct MessageBuffer {
    /// Maximum number of messages held in the buffer before the oldest is dropped.
    pub max_messages: usize,
}

/// Aggregate Quality of Service settings for a Client Topic (CERT CAL-005210).
///
/// Pass to [`AbstractCalExt::create_writer`] or [`AbstractCalExt::create_reader`]
/// at creation time.  The default value selects best-effort reliability with no
/// filtering, no expiration, and unbounded buffers.
#[derive(Debug, Clone, Default)]
pub struct TopicQos {
    /// Delivery reliability policy (default: `BestEffort`).
    pub reliability: Reliability,
    /// Time-based filter applied on the reader side; `None` disables filtering.
    pub time_based_filter: Option<TimeBasedFilter>,
    /// Message lifetime on the reader's receive buffer; `None` disables expiry.
    pub expiration: Option<Expiration>,
    /// Writer-side send buffer limit; `None` means unbounded.
    pub writer_buffer: Option<MessageBuffer>,
    /// Reader-side receive buffer limit; `None` means unbounded.
    pub reader_buffer: Option<MessageBuffer>,
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCal
// ════════════════════════════════════════════════════════════════════════════

/// CAL application-layer interface. Supertrait of [`AbstractServiceBus`].
///
/// Every `AbstractCal` IS-A `AbstractServiceBus`: it exposes the full transport
/// identity and status interface, plus the CAL-layer concerns that applications
/// actually use (message header defaults, typed writer/reader factories).
///
/// # Object safety
/// `AbstractCal` is object-safe; store instances as `Arc<Mutex<dyn AbstractCal>>`.
/// Generic factory methods live in the separate non-object-safe [`AbstractCalExt<M>`]
/// trait.
///
/// # CERT coverage
/// CAL-005201, CAL-005202, CAL-005203
pub trait AbstractCal: AbstractServiceBus {
    /// Returns the defaults used to pre-populate `MessageHeader` and
    /// `SecurityInformation` fields when creating a new message.
    ///
    /// Sourced from the `[system]` and `[service]` sections of the CAL
    /// configuration file.
    fn message_header_defaults(&self) -> MessageHeaderDefaults;

    /// Subscribes to raw wire-format bytes on `topic`.
    ///
    /// The listener is called from a background task with the unmodified
    /// transport payload each time a message arrives. Implementations that
    /// do not support raw forwarding return `Err(OperationNotPermitted)`.
    fn subscribe_raw(
        &mut self,
        _topic: &str,
        _listener: Arc<dyn RawMessageListener>,
    ) -> CalResult<()> {
        Err(CalError::new(
            CalErrorKind::OperationNotPermitted,
            "subscribe_raw not supported by this transport",
        ))
    }

    /// Publishes raw wire-format bytes to `topic`.
    ///
    /// `payload` must be in the same format that [`subscribe_raw`](Self::subscribe_raw)
    /// delivers to listeners. Implementations that do not support raw
    /// forwarding return `Err(OperationNotPermitted)`.
    fn publish_raw(&mut self, _topic: &str, _payload: &[u8]) -> CalResult<()> {
        Err(CalError::new(
            CalErrorKind::OperationNotPermitted,
            "publish_raw not supported by this transport",
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCalExt
// ════════════════════════════════════════════════════════════════════════════

/// Extension trait adding typed factory methods to [`AbstractCal`].
///
/// Separated so that `AbstractCal` remains object-safe —
/// `dyn AbstractCal` can still be stored in `Arc<Mutex<...>>`.
/// Factory methods are called through a concrete or generic reference.
///
/// # CERT coverage
/// CAL-005364 (`create_writer`), CAL-005374 (`create_reader`)
pub trait AbstractCalExt<M: CalMessage>: AbstractCal {
    /// Creates an [`AbstractWriter`] bound to `topic` with the given QoS
    /// settings (CERT CAL-005364, CAL-005210).
    ///
    /// Returns `Err(TopicUnavailable)` if `topic` is not a valid Client Topic
    /// for this service (CERT CAL-005368, CAL-005369).
    fn create_writer(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractWriter<M>>>;

    /// Creates an [`AbstractReader`] bound to `topic` with the given QoS
    /// settings (CERT CAL-005374, CAL-005210).
    ///
    /// The topic connection and message buffering are established before this
    /// returns (CERT CAL-005394, CAL-016044).
    ///
    /// Returns `Err(TopicUnavailable)` if `topic` is not a valid Client Topic
    /// (CERT CAL-005378).
    fn create_reader(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractReader<M>>>;
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractCalCreateMessage
// ════════════════════════════════════════════════════════════════════════════

/// Extension trait that adds typed message creation to any [`AbstractCal`].
///
/// Implemented as a blanket impl over all `AbstractCal` types so that
/// transport implementations do not need to duplicate this logic.
///
/// Use this instead of constructing message types directly — generated structs
/// have no public constructors (CERT CAL-016035).
pub trait AbstractCalCreateMessage {
    /// Creates a default-initialised instance of message type `M`.
    ///
    /// Equivalent to the C++ `<Type>::create(asb)` static factory.
    fn create_message<M: CalMessage>(&self) -> CalResult<M>;
}

impl<T: AbstractCal> AbstractCalCreateMessage for T {
    fn create_message<M: CalMessage>(&self) -> CalResult<M> {
        let mut msg = M::cal_create();
        if let Some(mt) = msg.as_message_type_mut() {
            let defaults = self.message_header_defaults();
            let hdr = mt.message_header_mut();
            *hdr.system_id_mut().uuid_mut() = defaults.system_id;
            *hdr.schema_version_mut() = defaults.schema_version;
            *hdr.mode_mut() = defaults.mode;
            *hdr.timestamp_mut() = Utc::now().into();
            if let (Some(sid), Some(sfield)) = (defaults.service_id, hdr.service_id_mut()) {
                *sfield.uuid_mut() = sid;
            }
            if let (Some(mid), Some(mfield)) = (defaults.mission_id, hdr.mission_id_mut()) {
                *mfield.uuid_mut() = mid;
            }
            let sec = mt.security_information_mut();
            *sec.classification_mut() = defaults.classification;
            let op = sec.owner_producer_mut();
            op.clear();
            op.extend(defaults.owner_producer);
        }
        Ok(msg)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Factory
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct AsbKey {
    pub(crate) service_identifier: String,
    pub(crate) asb_identifier: String,
}

type CalInstance = Arc<Mutex<dyn AbstractCal>>;
type CalFactoryMap = HashMap<AsbKey, CalInstance>;

lazy_static! {
    static ref CAL_FACTORY: tokio::sync::Mutex<CalFactoryMap> =
        tokio::sync::Mutex::new(CalFactoryMap::new());
}

/// Returns the [`AbstractCal`] instance for `(service_identifier, asb_identifier)`,
/// creating it if it does not yet exist.
///
/// Satisfies CERT CAL-005201 (mechanism to obtain a fully initialised CAL
/// instance) and CERT CAL-005202 (one instance per unique key pair).
///
/// Returns `Err(InitializationFailure)` when no matching (or default)
/// transport is configured, or when the underlying constructor fails.
pub async fn get_cal(
    service_identifier: impl Into<String>,
    asb_identifier: impl Into<String>,
    config: Arc<CalConfig>,
    logger: slog::Logger,
) -> CalResult<CalInstance> {
    let key = AsbKey {
        service_identifier: service_identifier.into(),
        asb_identifier: asb_identifier.into(),
    };

    // Hold the factory lock for the entire construction to prevent concurrent
    // callers from constructing duplicate instances that bind the same socket.
    let mut map = CAL_FACTORY.lock().await;

    if let Some(existing) = map.get(&key) {
        return Ok(Arc::clone(existing));
    }

    let transport = config
        .get_transport(&key.asb_identifier)
        .or_else(|| {
            config
                .system
                .default_transport
                .as_ref()
                .and_then(|def| config.get_transport(def))
        })
        .ok_or_else(|| {
            CalError::new(
                CalErrorKind::InitializationFailure,
                format!(
                    "No transport configured for '{}' and no default_transport available.",
                    key.asb_identifier
                ),
            )
        })?;

    let instance: CalInstance = match transport.type_.as_str() {
        #[cfg(feature = "zmq")]
        ZMQ_ASB_ID => Arc::new(Mutex::new(
            ZmqAsb::new(
                key.service_identifier.clone(),
                key.asb_identifier.clone(),
                logger,
                Arc::clone(&config),
                transport,
            )
            .await?,
        )),
        FILE_ASB_ID => Arc::new(Mutex::new(
            FileAsb::new(
                key.service_identifier.clone(),
                key.asb_identifier.clone(),
                logger,
                Arc::clone(&config),
                transport,
            )
            .await?,
        )),
        other => {
            return Err(CalError::new(
                CalErrorKind::InitializationFailure,
                format!("Unknown ASB transport type: '{other}'."),
            ));
        }
    };

    map.insert(key, Arc::clone(&instance));
    Ok(instance)
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asb::NEXT_TEST_PORT;
    use crate::asb::zmq::test_config_on_ports;
    use rcal_macros::init_test_logger;
    use std::sync::atomic::Ordering;

    #[cfg(feature = "zmq")]
    #[init_test_logger]
    #[tokio::test]
    async fn test_cal_factory_same_key_returns_same_instance() {
        let p1 = NEXT_TEST_PORT.fetch_add(1, Ordering::SeqCst);
        let p2 = NEXT_TEST_PORT.fetch_add(1, Ordering::SeqCst);
        let config = test_config_on_ports(&[p1, p2]);

        let a = get_cal("test_svc", "TestZmq", Arc::clone(&config), logger.clone())
            .await
            .expect("first get_cal must succeed");
        let b = get_cal("test_svc", "TestZmq", Arc::clone(&config), logger.clone())
            .await
            .expect("second get_cal must succeed");
        let c = get_cal(
            "test_svc_2",
            "TestZmq2",
            Arc::clone(&config),
            logger.clone(),
        )
        .await
        .expect("different service must succeed");

        assert!(Arc::ptr_eq(&a, &b), "same key should return the same Arc");
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different service key must be a distinct instance"
        );
    }
}
