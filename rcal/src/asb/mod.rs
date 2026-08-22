//! Abstract Service Bus (ASB) interface.
//!
//! Provides:
//!   - Service identity and UUID retrieval (CERT CAL-005203)
//!   - CAL instance lifecycle management (CERT CAL-005201, CAL-005202)
//!   - ASB connection status – polling **and** callback (§5.9, CERT CAL-016366)
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L §5.3, §5.9, Table 5.9-1/2, Figure 5.9-1/2
//! - OMSC-SPC-008 Rev K §9.9 (`AbstractServiceBusConnection`)

#![allow(dead_code)]
#![warn(missing_docs)]

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
use std::env;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "zmq")]
pub mod zmq;
#[cfg(feature = "zmq")]
use zmq::{ZMQ_ASB_ID, ZmqAsb};

// ════════════════════════════════════════════════════════════════════════════
// Config helpers
// ════════════════════════════════════════════════════════════════════════════

/// Returns the path to the CAL configuration file.
///
/// Resolution order:
/// 1. `path` argument, if `Some`.
/// 2. `RCAL_CONFIG` environment variable.
/// 3. `./CALConfig.toml` default.
///
/// Returns `Err(InitializationFailure)` when the resolved path does not exist.
pub fn get_asb_config_location(path: Option<String>) -> CalResult<String> {
    let config_file = path.unwrap_or_else(|| {
        env::var("RCAL_CONFIG").unwrap_or_else(|_| String::from_str("./CALConfig.toml").unwrap())
    });

    if Path::new(&config_file).exists() {
        Ok(config_file)
    } else {
        Err(CalError::new(
            CalErrorKind::InitializationFailure,
            format!("Config file '{}' does not exist.", config_file),
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AsbConnectionState
// ════════════════════════════════════════════════════════════════════════════

/// Enumeration of Abstract Service Bus (ASB) connection states.
///
/// Defined in Table 5.9-1 of OMSC-SPC-001 Rev L.
///
/// Only `Normal` and `Failed` are **required** states. `Initializing`,
/// `Degraded`, and `Inoperable` are optional (marked `*` in the spec).
///
/// # State machine (Figure 5.9-1)
///
/// ```text
///   ┌──────────────────┐
///   │  INITIALIZING *  │──────────────────────────────┐
///   └──────────────────┘                              │
///          │  ↑                                       ↓
///          │  └──────────────────────────┌───────────────────────┐
///          ↓                             │      Operational      │
///   ┌──────────┐         ┌────────────┐  │  ┌────────┐           │
///   │  FAILED  │◄────────│ INOPERABLE*│──│  │ NORMAL │           │
///   │   (●)    │◄───────◄└────────────┘  │  └────────┘           │
///   └──────────┘                         │      ↕                 │
///                                        │  ┌──────────┐         │
///                                        │  │ DEGRADED*│         │
///                                        │  └──────────┘         │
///                                        └───────────────────────┘
/// ```
///
/// Transition rules (Figure 5.9-2):
/// - `Failed` is terminal — no outgoing transitions.
/// - `Normal` cannot return to `Initializing`.
/// - `Degraded` ↔ `Normal` are mutually reachable.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsbConnectionState {
    /// Optional (*). CAL is starting up; initialization is not complete.
    ///
    /// `write()` → Error, `read_no_wait()` → Error,
    /// `read()` (blocking) → OK, `add_listener()` → OK.
    Initializing,

    /// Required. CAL is fully operational; all QoS settings are satisfied.
    Normal,

    /// Optional (*). CAL is operational but some QoS settings are not met.
    Degraded,

    /// Optional (*). CAL cannot send/receive; attempting recovery.
    ///
    /// `write()` → Error, `read_no_wait()` → Error,
    /// `read()` (blocking) → OK, `add_listener()` → OK.
    Inoperable,

    /// Required. CAL is permanently unusable; recovery is impossible.
    ///
    /// Terminal state — no transitions out are permitted.
    /// Existing blocking reads are released with [`CalErrorKind::AsbFailed`].
    Failed,
}

impl AsbConnectionState {
    /// `true` if the CAL can transmit messages in this state.
    pub fn allows_write(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// `true` if non-blocking reads are permitted.
    pub fn allows_read_no_wait(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// `true` if registering a new status listener is permitted.
    /// Only `Failed` rejects new listeners (Table 5.9-2).
    pub fn allows_add_listener(&self) -> bool {
        !matches!(self, Self::Failed)
    }

    /// `true` for any operational (fully or partially) state.
    pub fn is_operational(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// Validates whether the transition `self → next` is allowed per Figure 5.9-2.
    ///
    /// Returns `Err(InvalidState)` for disallowed transitions.
    pub fn validate_transition(&self, next: AsbConnectionState) -> CalResult<()> {
        let allowed = match (self, next) {
            // ── From INITIALIZING ──────────────────────────────────────────
            (Self::Initializing, Self::Normal) => true,
            (Self::Initializing, Self::Degraded) => true,
            (Self::Initializing, Self::Inoperable) => true,
            (Self::Initializing, Self::Failed) => true,
            // ── From NORMAL ────────────────────────────────────────────────
            (Self::Normal, Self::Degraded) => true,
            (Self::Normal, Self::Inoperable) => true,
            (Self::Normal, Self::Failed) => true,
            // ── From DEGRADED ──────────────────────────────────────────────
            (Self::Degraded, Self::Normal) => true,
            (Self::Degraded, Self::Inoperable) => true,
            (Self::Degraded, Self::Failed) => true,
            // ── From INOPERABLE ────────────────────────────────────────────
            (Self::Inoperable, Self::Initializing) => true,
            (Self::Inoperable, Self::Normal) => true,
            (Self::Inoperable, Self::Degraded) => true,
            (Self::Inoperable, Self::Failed) => true,
            // ── From FAILED — terminal ─────────────────────────────────────
            (Self::Failed, _) => false,
            // ── Self-transitions ───────────────────────────────────────────
            _ => false,
        };

        if allowed {
            Ok(())
        } else {
            Err(CalError::new(
                CalErrorKind::InvalidState { current: *self },
                format!(
                    "ASB state transition {self:?} → {next:?} is not permitted \
                     (Figure 5.9-2, OMSC-SPC-001 Rev L)",
                ),
            ))
        }
    }
}

impl fmt::Display for AsbConnectionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initializing => write!(f, "Initializing"),
            Self::Normal => write!(f, "Normal"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Inoperable => write!(f, "Inoperable"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AsbStatus
// ════════════════════════════════════════════════════════════════════════════

/// Current ASB connection state plus an implementation-defined description.
///
/// Returned by both the polling interface (`connection_status()`) and the
/// callback interface (`AsbStatusListener::on_status_change`).
///
/// Per §5.9: "The CAL status interface poll and callback will provide the
/// enumeration of the current state and a CAL implementer-defined string."
#[derive(Debug, Clone)]
pub struct AsbStatus {
    /// The current ASB connection state (Table 5.9-1).
    pub state: AsbConnectionState,

    /// Human-readable, implementation-defined description.
    /// May include middleware-specific detail (e.g., "ZeroMQ broker unreachable").
    pub description: String,
}

impl AsbStatus {
    /// Constructs a new `AsbStatus`.
    pub fn new(state: AsbConnectionState, description: impl Into<String>) -> Self {
        Self {
            state,
            description: description.into(),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// AsbStatusListener
// ════════════════════════════════════════════════════════════════════════════

/// Callback interface for ASB connection-status change notifications.
///
/// # Immediate invocation on registration (CERT CAL-016366)
///
/// Upon successful `register_status_listener()` the implementation **must**
/// call `on_status_change` with the current state before returning.
///
/// # Thread safety
///
/// The CAL may call `on_status_change` from an internal thread.
/// Implementations must therefore be `Send + Sync`.  Multiple registered
/// listeners may be called sequentially from one thread *or* concurrently
/// from independent threads — implementations must tolerate both.
pub trait AsbStatusListener: Send + Sync {
    /// Called when the ASB connection state changes.
    ///
    /// Also called once immediately after successful registration
    /// (CERT CAL-016366).
    ///
    /// Must not block indefinitely; doing so delays subsequent notifications.
    /// Implementations that require internal mutation must use interior
    /// mutability (e.g. `Mutex`, `AtomicXxx`).
    fn on_status_change(&self, status: &AsbStatus);
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractServiceBus
// ════════════════════════════════════════════════════════════════════════════

/// Central CAL instance interface — Rust equivalent of the C++
/// `AbstractServiceBusConnection` abstract class.
///
/// Each instance is bound to a unique (Service Identifier, ASB Identifier)
/// pair (CERT CAL-005202) and is obtained via [`get_asb`] (CERT CAL-005201).
///
/// # Responsibilities
///
/// 1. **Identity** — expose Service and system UUIDs (CERT CAL-005203).
/// 2. **Connection Status** — polling *and* callback interfaces (§5.9).
/// 3. **Lifecycle** — initialisation and graceful shutdown.
pub trait AbstractServiceBus: Send + Sync {
    /// Returns the [`slog::Logger`] associated with this ASB instance.
    fn get_logger(&self) -> &slog::Logger;

    // ── Identity ──────────────────────────────────────────────────────────

    /// Service Identifier used to initialise this instance (§4.10, §5.3).
    fn service_identifier(&self) -> &str;

    /// ASB Identifier.  Together with `service_identifier()` this uniquely
    /// identifies the CAL instance (CERT CAL-005202).
    fn asb_identifier(&self) -> &str;

    /// UUID of the system this CAL instance belongs to (CERT CXX-011168).
    ///
    /// Sourced from `[system] uuid` in the CAL configuration file.
    /// Returns the nil UUID when not configured.
    fn get_system_uuid(&self) -> UUID;

    /// UUID of the service this CAL instance represents (CERT CXX-011169).
    ///
    /// Sourced from `[service.<id>] uuid` in the CAL configuration file.
    /// Returns `None` when the service UUID is not configured.
    fn get_service_uuid(&self) -> Option<UUID>;

    /// UUID of the subsystem this service belongs to (CERT CXX-011170).
    ///
    /// Sourced from `[service.<id>] subsystem_uuid` in the CAL configuration file.
    /// Returns `None` when not configured.
    fn get_subsystem_uuid(&self) -> Option<UUID>;

    /// UUID of the named component (CERT CXX-011171).
    ///
    /// Looks up `name` in the `[[service.<id>.components]]` list in the CAL
    /// configuration file.  Returns `None` when the component is not found.
    fn get_component_uuid(&self, name: &str) -> Option<UUID>;

    /// UUID of the named capability (CERT CXX-011172).
    ///
    /// Looks up `name` in the `[[service.<id>.capabilities]]` list in the CAL
    /// configuration file.  Returns `None` when the capability is not found.
    fn get_capability_uuid(&self, name: &str) -> Option<UUID>;

    /// Version of the OMS Schema Definition used to generate the CAL.
    fn oms_schema_version(&self) -> &str;

    /// Version of the OMS Schema Compiler used to generate the CAL.
    fn oms_schema_compiler_version(&self) -> &str;

    /// Human-readable label for the system this CAL instance belongs to.
    ///
    /// Sourced from `[system] label` in the CAL configuration file.
    /// Returns `None` when the label is not configured.
    ///
    /// CERT CXX-005424 (`getMySystemLabel()`).
    fn get_system_label(&self) -> Option<&str>;

    /// Version string identifying this CAL implementation.
    ///
    /// Defaults to `<package>/<version>` at build time; override by setting
    /// the `RCAL_ASB_CONNECTION_VERSION` environment variable during the build.
    ///
    /// CERT CXX-011176 (`getAbstractServiceBusConnectionVersion()`).
    fn get_abstract_service_bus_connection_version(&self) -> &str;

    /// Version string identifying the OMS API against which this CAL was built.
    ///
    /// Defaults to `<package>/<version>` at build time; override by setting
    /// the `RCAL_OMS_API_VERSION` environment variable during the build.
    ///
    /// CERT CXX-012694 (`getOMSApiVersion()`).
    fn get_oms_api_version(&self) -> &str;

    // ── Connection Status — Polling ───────────────────────────────────────

    /// Returns the current ASB connection status (polling interface, §5.9).
    ///
    /// Executes within the caller's thread.  Safe to call in any state,
    /// including `Failed`.
    fn connection_status(&self) -> &AsbStatus;

    // ── Connection Status — Callback ──────────────────────────────────────

    /// Registers an ASB connection-status listener.
    ///
    /// `on_status_change` is called **immediately** with the current state
    /// (CERT CAL-016366) and subsequently on every state change.
    ///
    /// Returns `Err(InvalidState)` when called in the `Failed` state
    /// (Table 5.9-2: `addListener()` in `Failed` → Error).
    fn register_status_listener(&mut self, listener: Arc<dyn AsbStatusListener>) -> CalResult<()>;

    /// Unregisters a previously registered ASB status listener.
    ///
    /// Identified by `Arc` pointer equality. No-op if the listener is not
    /// registered.
    fn unregister_status_listener(
        &mut self,
        listener: &Arc<dyn AsbStatusListener>,
    ) -> CalResult<()>;

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Shuts down the CAL instance and releases all associated resources.
    ///
    /// After `close()` returns, all Writers and Readers are invalidated,
    /// registered listeners will not receive further callbacks, and blocked
    /// `read()` calls are released with an error.
    fn close(&mut self) -> CalResult<()>;

    // ── Message header defaults ───────────────────────────────────────────

    /// Returns the defaults used to pre-populate `MessageHeader` and
    /// `SecurityInformation` fields when creating a new message via
    /// [`AbstractServiceBusCreateMessage::create_message`].
    fn message_header_defaults(&self) -> MessageHeaderDefaults;
}

// ════════════════════════════════════════════════════════════════════════════
// MessageHeaderDefaults
// ════════════════════════════════════════════════════════════════════════════

/// Default values applied to every message created through an ASB.
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
// AbstractWriter
// ════════════════════════════════════════════════════════════════════════════

/// A topic-bound CAL message publisher (CERT CAL-005368).
///
/// Obtained via [`AbstractServiceBusExt::create_writer`] (CERT CAL-005364).
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
/// Obtained via [`AbstractServiceBusExt::create_reader`] (CERT CAL-005374).
/// The topic connection and message buffering are established at creation
/// time, before this call returns (CERT CAL-005394, CAL-016044).
///
/// # Connection timing (stream-based transports)
///
/// For transports that use an asynchronous TCP handshake (e.g. ZeroMQ
/// RADIO/DISH over TCP), the underlying socket connects in the background.
/// Messages published in the brief window between `create_reader` returning
/// and the handshake completing may be missed. Callers requiring reliable
/// first-message delivery should insert a short delay after creation.
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
    ///
    /// Zero or more listeners may be registered. Each received message
    /// is dispatched to every listener exactly once (CERT CAL-005392).
    /// Once any listener is registered, `read` and `read_no_wait` return
    /// `Err(OperationNotPermitted)` (CERT CAL-016050).
    fn add_listener(&mut self, listener: Arc<dyn MessageListener<M>>) -> CalResult<()>;

    /// Unregisters a previously registered listener (CERT CAL-005396).
    ///
    /// Identified by `Arc` pointer equality. No-op if not registered.
    fn remove_listener(&mut self, listener: &Arc<dyn MessageListener<M>>) -> CalResult<()>;

    // ── Polling interface ─────────────────────────────────────────────────

    /// Blocking read — waits until a message arrives or the call is released.
    ///
    /// Blocks until:
    /// 1. A message arrives — returns `Ok(Arc<M>)`, message removed from buffer (CAL-016052).
    /// 2. `timeout` elapses — returns `Ok(None)`.
    /// 3. This reader is closed — returns `Err(InvalidState)`.
    /// 4. The ASB enters `Failed` — returns `Err(AsbFailed)`.
    ///
    /// `timeout: None` blocks indefinitely until a message or a close event.
    ///
    /// Returns `Err(OperationNotPermitted)` if any listener is registered
    /// (CERT CAL-016050).
    ///
    /// # CERT coverage
    /// CAL-016049, CAL-016050, CAL-016052
    fn read(&mut self, timeout: Option<Duration>) -> CalResult<Option<Arc<M>>>;

    /// Non-blocking read — returns a buffered message if one is available.
    ///
    /// - `Ok(Some(msg))` — message available; removed from buffer (CAL-016052).
    /// - `Ok(None)` — buffer empty.
    /// - `Err(OperationNotPermitted)` — listeners registered (CAL-016050).
    /// - `Err(InvalidState)` — ASB state prohibits reads (Table 5.9-2).
    /// - `Err(AsbFailed)` — ASB has permanently failed.
    ///
    /// # CERT coverage
    /// CAL-005380, CAL-016050, CAL-016052
    fn read_no_wait(&mut self) -> CalResult<Option<Arc<M>>>;

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Shuts down this reader and releases all associated resources.
    ///
    /// After `close()` returns, registered listeners receive no further
    /// callbacks and any blocked `read` call returns `Err(InvalidState)`.
    ///
    /// `Box<Self>` consuming receiver prevents use-after-close.
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
/// Pass to [`AbstractServiceBusExt::create_writer`] or
/// [`AbstractServiceBusExt::create_reader`] at creation time. The default
/// value selects best-effort reliability with no filtering, no expiration, and
/// unbounded buffers.
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
// AbstractServiceBusExt
// ════════════════════════════════════════════════════════════════════════════

/// Extension trait adding typed factory methods to [`AbstractServiceBus`].
///
/// Separated so that `AbstractServiceBus` remains object-safe —
/// `dyn AbstractServiceBus` can still be stored in `Arc<Mutex<...>>`.
/// Factory methods are called through a concrete or generic reference.
///
/// # CERT coverage
/// CAL-005364 (`create_writer`), CAL-005374 (`create_reader`)
pub trait AbstractServiceBusExt<M: CalMessage>: AbstractServiceBus {
    /// Creates a [`AbstractWriter`] bound to `topic` with the given QoS
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
// AbstractServiceBusCreateMessage
// ════════════════════════════════════════════════════════════════════════════

/// Extension trait that adds typed message creation to any [`AbstractServiceBus`].
///
/// Implemented as a blanket impl over all `AbstractServiceBus` types so that
/// transport implementations do not need to duplicate this logic.
///
/// Use this instead of constructing message types directly — generated structs
/// have no public constructors (CERT CAL-016035).
pub trait AbstractServiceBusCreateMessage {
    /// Creates a default-initialised instance of message type `M`.
    ///
    /// Equivalent to the C++ `<Type>::create(asb)` static factory.
    fn create_message<M: CalMessage>(&self) -> CalResult<M>;
}

impl<T: AbstractServiceBus> AbstractServiceBusCreateMessage for T {
    fn create_message<M: CalMessage>(&self) -> CalResult<M> {
        let mut msg = M::cal_create();
        if let Some(mt) = msg.as_message_type_mut() {
            let defaults = self.message_header_defaults();
            let hdr = mt.message_header_mut();
            *hdr.system_id_mut().uuid_mut() = defaults.system_id;
            *hdr.schema_version_mut() = defaults.schema_version;
            *hdr.mode_mut() = defaults.mode;
            *hdr.timestamp_mut() = Utc::now().into();
            // Optional fields can only be set if already initialized (trait returns Option<&mut>).
            // service_id and mission_id are set here only if already Some in the message.
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
struct AsbKey {
    service_identifier: String,
    asb_identifier: String,
}

type AsbInstance = Arc<Mutex<dyn AbstractServiceBus>>;
type AsbFactoryMap = HashMap<AsbKey, AsbInstance>;

lazy_static! {
    static ref ASB_FACTORY: tokio::sync::Mutex<AsbFactoryMap> =
        tokio::sync::Mutex::new(AsbFactoryMap::new());
}

/// Returns the [`AbstractServiceBus`] instance for `(service_identifier,
/// asb_identifier)`, creating it if it does not yet exist.
///
/// Satisfies CERT CAL-005201 (mechanism to obtain a fully initialised CAL
/// instance) and CERT CAL-005202 (one instance per unique key pair).
///
/// Returns `Err(InitializationFailure)` when no matching (or default)
/// transport is configured, or when the underlying constructor fails.
pub async fn get_asb(
    service_identifier: impl Into<String>,
    asb_identifier: impl Into<String>,
    config: Arc<CalConfig>,
    logger: slog::Logger,
) -> CalResult<AsbInstance> {
    let key = AsbKey {
        service_identifier: service_identifier.into(),
        asb_identifier: asb_identifier.into(),
    };

    // Hold the factory lock for the entire construction to prevent concurrent
    // callers from constructing duplicate instances that bind the same socket.
    let mut map = ASB_FACTORY.lock().await;

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

    let instance: AsbInstance = match transport.type_.as_str() {
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
// Common message macros
// ════════════════════════════════════════════════════════════════════════════

/// Refreshes the `MessageHeader.Timestamp` field to the current UTC time.
///
/// Accepts any mutable reference to a type that implements [`CalMessage`] with
/// an accessible `MessageType` (i.e. any generated top-level message wrapper).
/// No-op for message types that do not expose a `MessageType` interface.
///
/// # Example
/// ```ignore
/// update_message_header!(my_msg);
/// ```
#[macro_export]
macro_rules! update_message_header {
    ($msg:expr) => {
        if let Some(mt) = $crate::uci::CalMessage::as_message_type_mut(&mut $msg) {
            *mt.message_header_mut().timestamp_mut() = chrono::Utc::now().into();
        }
    };
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

/// Shared port counter for all ASB tests — prevents overlap between test
/// modules running in parallel within the same binary.
#[cfg(test)]
pub(crate) static NEXT_TEST_PORT: std::sync::atomic::AtomicU16 =
    std::sync::atomic::AtomicU16::new(2000);

#[cfg(test)]
mod trait_object_safety {
    use super::*;
    use crate::uci::CalMessage;

    struct Ping;
    impl CalMessage for Ping {
        fn message_type_name() -> crate::QName {
            "test.Ping".into()
        }
        fn cal_create() -> Self {
            Self
        }
    }

    // Compile-only assertions — fail at definition site if any trait is not object-safe.
    #[allow(dead_code)]
    type _W = Box<dyn AbstractWriter<Ping>>;
    #[allow(dead_code)]
    type _R = Box<dyn AbstractReader<Ping>>;
    #[allow(dead_code)]
    type _L = Arc<dyn MessageListener<Ping>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calconfig::{get_test_config_path, parse_config_from_file};
    use rcal_macros::init_test_logger;
    use std::sync::atomic::Ordering;

    #[cfg(feature = "zmq")]
    #[init_test_logger]
    #[tokio::test]
    async fn test_asb_factory_same_key_returns_same_instance() {
        let p1 = super::NEXT_TEST_PORT.fetch_add(1, Ordering::SeqCst);
        let p2 = super::NEXT_TEST_PORT.fetch_add(1, Ordering::SeqCst);
        let config = zmq::test_config_on_ports(&[p1, p2]);

        let a = get_asb("test_svc", "TestZmq", Arc::clone(&config), logger.clone())
            .await
            .expect("first get_asb must succeed");
        let b = get_asb("test_svc", "TestZmq", Arc::clone(&config), logger.clone())
            .await
            .expect("second get_asb must succeed");
        let c = get_asb(
            "test_svc_2",
            "TestZmq2",
            Arc::clone(&config),
            logger.clone(),
        )
        .await
        .expect("different service must succeed");

        // Same (service, asb) key → same Arc (CERT CAL-005202)
        assert!(Arc::ptr_eq(&a, &b), "same key should return the same Arc");
        // Different service key → different Arc
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different service key must be a distinct instance"
        );
    }

    #[init_test_logger]
    #[tokio::test]
    async fn test_asb_factory_unknown_transport_returns_err() {
        let no_default_config = Arc::new(
            parse_config_from_file(&get_test_config_path("calconfig_no_default.toml")).unwrap(),
        );
        assert!(
            get_asb("svc", "dummy", no_default_config, logger)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_state_display_does_not_recurse() {
        assert_eq!(AsbConnectionState::Initializing.to_string(), "Initializing");
        assert_eq!(AsbConnectionState::Normal.to_string(), "Normal");
        assert_eq!(AsbConnectionState::Degraded.to_string(), "Degraded");
        assert_eq!(AsbConnectionState::Inoperable.to_string(), "Inoperable");
        assert_eq!(AsbConnectionState::Failed.to_string(), "Failed");
    }

    // ── State-transition validation ───────────────────────────────────────

    #[tokio::test]
    async fn test_valid_transitions() {
        use AsbConnectionState::*;
        let cases = [
            (Initializing, Normal),
            (Initializing, Degraded),
            (Initializing, Inoperable),
            (Initializing, Failed),
            (Normal, Degraded),
            (Normal, Inoperable),
            (Normal, Failed),
            (Degraded, Normal),
            (Degraded, Inoperable),
            (Degraded, Failed),
            (Inoperable, Initializing),
            (Inoperable, Normal),
            (Inoperable, Degraded),
            (Inoperable, Failed),
        ];
        for (from, to) in cases {
            assert!(
                from.validate_transition(to).is_ok(),
                "{from:?} → {to:?} should be allowed"
            );
        }
    }

    #[tokio::test]
    async fn test_invalid_transitions() {
        use AsbConnectionState::*;
        // Failed is terminal.
        for to in [Initializing, Normal, Degraded, Inoperable] {
            assert!(
                Failed.validate_transition(to).is_err(),
                "Failed → {to:?} must be rejected"
            );
        }
        // Normal cannot go back to Initializing.
        assert!(Normal.validate_transition(Initializing).is_err());
    }
}
