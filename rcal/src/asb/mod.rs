// =============================================================================
//  OMS Open Mission Systems — Rust Critical Abstraction Layer (CAL)
//  Abstract Service Bus (ASB) interface
//
//  Document: (Future) Rust CAL Interface Generation Specification
//  References: OMSC-SPC-001 Rev L (Generic CAL Specification)
//              OMSC-SPC-008 Rev K (C++ CAL — inspiration)
//
//  Distribution Statement A. Approved for public release: distribution unlimited.
// =============================================================================
//
// Provides:
//   - Service identity and UUID retrieval (CERT CAL-005203)
//   - CAL instance lifecycle management (CERT CAL-005201, CAL-005202)
//   - ASB connection status (polling + callback) (§5.9, CERT CAL-016366)
//
// References: §4.12, §5.3, §5.9, Table 5.9-1, Table 5.9-2,
//             Figure 5.9-1, Figure 5.9-2
// =============================================================================
#![allow(dead_code)]
use crate::uci::base::ServiceUuids;
use crate::uci::{CalError, CalErrorKind, CalResult};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

mod zmq;
use zmq::{ZMQ_ASB_ID, ZmqAsb};

// ─── ASB Connection State ─────────────────────────────────────────────────

/// Enumeration of Abstract Service Bus (ASB) connection states.
///
/// Defined in Table 5.9-1 of OMSC-SPC-001 Rev L.
///
/// Only `Normal` and `Failed` are **required** states. Implementations
/// may optionally support `Initializing`, `Degraded`, and `Inoperable`
/// (indicated by `*` in the spec).
///
/// # State Machine
///
/// ```text
///   ┌─────────────────┐
///   │  INITIALIZING*  │──────────────────────────────┐
///   └─────────────────┘                              │
///          │  ↑                                      ↓
///          │  └──────────────────────── ┌──────────────────────┐
///          ↓                            │     Operational      │
///   ┌──────────┐                        │  ┌────────┐          │
///   │  FAILED  │◄───────────────────────│  │ NORMAL │          │
///   │   (●)    │◄──────────────────────◄│  └────────┘          │
///   └──────────┘       ┌─────────────┐  │      ↕               │
///        ↑             │ INOPERABLE* │──│  ┌──────────┐        │
///        │             └─────────────┘  │  │ DEGRADED*│        │
///        └─────────────────────────────◄│  └──────────┘        │
///                                       └──────────────────────┘
/// ```
///
/// Transition rules (Figure 5.9-2):
/// - `Failed` is terminal — no transitions out.
/// - `Normal` cannot return to `Initializing`.
/// - `Degraded` ↔ `Normal` transitions are allowed.
///
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsbConnectionState {
    /// Optional. CAL is starting up; initialization is not yet complete.
    ///
    /// Behavior (Table 5.9-2):
    /// - `write()` → Error
    /// - `read_no_wait()` → Error
    /// - `read()` (blocking) → OK
    /// - `add_listener()` → OK
    Initializing,

    /// Required. CAL is fully operational; all QoS settings are satisfied.
    ///
    /// All operations behave normally.
    Normal,

    /// Optional. CAL is operational but some QoS settings are not met.
    ///
    /// Read and write operations behave normally. QoS violations may occur.
    Degraded,

    /// Optional. CAL is unable to send or receive; attempting recovery.
    ///
    /// Behavior (Table 5.9-2):
    /// - `write()` → Error
    /// - `read_no_wait()` → Error
    /// - `read()` (blocking) → OK (waits for recovery or timeout)
    /// - `add_listener()` → OK
    Inoperable,

    /// Required. CAL is permanently unusable; recovery is not possible.
    ///
    /// Terminal state — no transitions out are permitted.
    ///
    /// Behavior (Table 5.9-2):
    /// - All new operations except `add_listener()` → Error
    /// - `add_listener()` → Error (listener will never fire)
    /// - Existing listeners → will never be called again (NO/OP)
    /// - Existing blocking reads → released with `CalError { AsbFailed }`
    Failed,
}

impl AsbConnectionState {
    /// Returns `true` if the CAL can transmit messages in this state.
    pub fn allows_write(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// Returns `true` if non-blocking reads are permitted in this state.
    pub fn allows_read_no_wait(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// Returns `true` if registering a new listener is permitted.
    /// (Fails only in `Failed` state per Table 5.9-2.)
    pub fn allows_add_listener(&self) -> bool {
        !matches!(self, Self::Failed)
    }

    /// Returns `true` if this is an operational (fully or partially) state.
    pub fn is_operational(&self) -> bool {
        matches!(self, Self::Normal | Self::Degraded)
    }

    /// Validates whether transitioning from `self` to `next` is allowed
    /// per Figure 5.9-2. Returns `Err` on disallowed transitions.
    pub fn validate_transition(&self, next: AsbConnectionState) -> CalResult<()> {
        let allowed = match (self, next) {
            // From INITIALIZING
            (Self::Initializing, Self::Normal) => true,
            (Self::Initializing, Self::Degraded) => true,
            (Self::Initializing, Self::Inoperable) => true,
            (Self::Initializing, Self::Failed) => true,
            // From NORMAL
            (Self::Normal, Self::Degraded) => true,
            (Self::Normal, Self::Inoperable) => true,
            (Self::Normal, Self::Failed) => true,
            // From DEGRADED
            (Self::Degraded, Self::Normal) => true,
            (Self::Degraded, Self::Inoperable) => true,
            (Self::Degraded, Self::Failed) => true,
            // From INOPERABLE
            (Self::Inoperable, Self::Initializing) => true,
            (Self::Inoperable, Self::Normal) => true,
            (Self::Inoperable, Self::Degraded) => true,
            (Self::Inoperable, Self::Failed) => true,
            // From FAILED — terminal, no transitions out
            (Self::Failed, _) => false,
            // Self-transitions are no-ops; caller should not call this
            _ => false,
        };
        if allowed {
            Ok(())
        } else {
            Err(CalError::new(
                CalErrorKind::InvalidState { current: *self },
                format!(
                    "ASB state transition {:?} → {:?} is not permitted \
                     (Figure 5.9-2, OMSC-SPC-001 Rev L)",
                    self, next
                ),
            ))
        }
    }
}

impl fmt::Display for AsbConnectionState {
    /// Formats the stae enum as a string
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self, f)
    }
}

// ─── AsbStatus ───────────────────────────────────────────────────────────

/// Carries the current ASB connection state and an implementation-defined
/// descriptive string.
///
/// Per §5.9: "The CAL status interface poll and callback will provide the
/// enumeration of the current state and a CAL implementer-defined string."
///
/// Provided both via the polling interface (`connection_status()`) and
/// the callback interface (`AsbStatusListener::on_status_change`).
#[derive(Debug, Clone)]
pub struct AsbStatus {
    /// The current ASB connection state (Table 5.9-1).
    pub state: AsbConnectionState,

    /// An implementation-defined, human-readable description.
    /// May include middleware-specific detail (e.g., "DDS domain 0 lost").
    pub description: String,
}

impl AsbStatus {
    pub fn new(state: AsbConnectionState, description: impl Into<String>) -> Self {
        Self {
            state,
            description: description.into(),
        }
    }
}

// ─── AsbStatusListener (Callback Interface) ───────────────────────────────

/// Callback interface for ASB connection status change notifications.
///
/// CAL Clients implement this trait to receive ASB status updates.
///
/// # Immediate Invocation on Registration
///
/// Upon successful `register_status_listener()`, the implementation
/// **immediately** invokes `on_status_change` with the current state
/// before returning to the caller (CERT CAL-016366).
///
/// # Thread Safety
///
/// The CAL Implementation may invoke `on_status_change` from an internal
/// thread. Implementations must be `Send + Sync`. Multiple listeners may
/// be called sequentially (single thread) or concurrently (multiple threads)
/// — implementations must handle both cases safely.
pub trait AsbStatusListener: Send + Sync {
    /// Invoked when the ASB connection state changes, and immediately upon
    /// registration with the current state (CERT CAL-016366).
    ///
    /// # Implementation Note
    ///
    /// This method must not block indefinitely; doing so will delay or
    /// prevent subsequent status updates from being delivered.
    fn on_status_change(&mut self, status: &AsbStatus);
}

// ─── AbstractServiceBus ───────────────────────────────────────────────────

/// The central CAL instance interface — the Rust equivalent of the C++
/// `AbstractServiceBus` abstract class.
///
/// Each `AbstractServiceBus` instance represents a single CAL instance
/// (§4.6) bound to a unique (Service Identifier, ASB Identifier) pair
/// (CERT CAL-005202). A CAL Client obtains a fully initialized instance
/// via the [`AsbFActory`] function (CERT CAL-005201).
///
/// # Responsibilities
///
/// 1. **Identity** — Expose Service and system UUIDs (CERT CAL-005203).
/// 2. **Connection Status** — Provide both polling and callback interfaces
///    for ASB connection state (§5.9).
/// 3. **Factory** — Create type-erased `CalWriter` and `CalReader` instances
///    (§5.6, §5.7). *(Writer/Reader traits defined in separate modules.)*
/// 4. **Lifecycle** — Manage initialization and graceful shutdown.
///
pub trait AbstractServiceBus: Send + Sync {
    /// Get the stored logger
    fn get_logger(&self) -> &slog::Logger;

    // ─── Identity ─────────────────────────────────────────────────────────

    /// Returns the Service Identifier string used to initialize this CAL
    /// instance (§4.10, §5.3).
    fn service_identifier(&self) -> &str;

    /// Returns the ASB Identifier associated with this CAL instance.
    /// Together with `service_identifier()`, uniquely identifies this
    /// CAL instance (CERT CAL-005202).
    fn asb_identifier(&self) -> &str;

    /// Returns the set of UUIDs identifying the System, Service, Subsystem,
    /// Components, and Capabilities associated with this Service.
    ///
    /// Satisfies CERT CAL-005203. Returns `Err` if the CAL instance has
    /// not been successfully initialized.
    fn service_uuids(&self) -> CalResult<&ServiceUuids>;

    /// Returns the OMS schema version from which the AbstractServiceBus was generated.
    fn oms_schema_version(&self) -> &str;

    /// Returns the version of the schema compiler used to generate the code.
    fn oms_schema_compiler_version(&self) -> &str;

    // ─── Connection Status — Polling Interface ────────────────────────────

    /// Returns the current ASB connection status synchronously.
    ///
    /// This is the **polling** interface defined in §5.9. Executes
    /// within the calling thread's context.
    ///
    /// May be called in any connection state, including `Failed`.
    fn connection_status(&self) -> &AsbStatus;

    // ─── Connection Status — Callback Interface ───────────────────────────

    /// Registers an ASB connection status listener.
    ///
    /// Upon successful registration, `listener.on_status_change()` is
    /// **immediately invoked** with the current connection state
    /// (CERT CAL-016366).
    ///
    /// Multiple listeners may be registered. The implementation may invoke
    /// them sequentially or concurrently; clients must handle both.
    ///
    /// Returns `Err(InvalidState)` when called in the `Failed` state
    /// (Table 5.9-2: `addListener()` in `Failed` → E).
    fn register_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()>;

    /// Unregisters a previously registered ASB status listener.
    ///
    /// After this call returns, `listener.on_status_change()` will no
    /// longer be invoked for future state changes.
    ///
    /// Has no effect if the listener was not registered.
    fn unregister_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()>;

    // ─── Lifecycle ────────────────────────────────────────────────────────

    /// Shuts down this CAL instance, releasing all associated resources.
    ///
    /// After `close()` returns:
    /// - All `CalWriter` and `CalReader` instances obtained from this bus
    ///   are invalidated.
    /// - Registered listeners will not receive further callbacks.
    /// - Blocked `read()` calls will be released with an error.
    ///
    /// Calling any other method after `close()` is a programming error
    /// and may return `Err(InvalidState)`.
    fn close(&mut self) -> CalResult<()>;
}

/// The actual factory.
/// New types go here
///

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AsbKey {
    service_identifier: String,
    asb_identifier: String,
}

type AsbFactory = HashMap<AsbKey, Arc<Mutex<dyn AbstractServiceBus>>>;

fn get_asb<S: Into<String>>(
    service_identifier: S,
    asb_identifier: S,
    logger: slog::Logger,
) -> CalResult<Arc<Mutex<dyn AbstractServiceBus>>> {
    let mut fact = ASB_FACTORY.lock().unwrap();
    let key = AsbKey {
        service_identifier: service_identifier.into(),
        asb_identifier: asb_identifier.into(),
    };
    fact.entry(key.clone())
        .or_try_insert_with(|| match key.asb_identifier.as_str() {
            ZMQ_ASB_ID => Ok(Arc::new(Mutex::new(ZmqAsb::new(
                key.service_identifier.clone(),
                logger,
            )))),
            _ => Err(CalError::new(
                CalErrorKind::InitializationFailure,
                "Invalid ASB type",
            )),
        })
        .cloned()
}

lazy_static! {
    static ref ASB_FACTORY: Mutex<AsbFactory> = Mutex::new(AsbFactory::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asb_factory() {
        let logger = rcal_macros::init_test_logger!();
        let a = get_asb("test", "zmq", logger.clone()).unwrap();
        let b = get_asb("test", "zmq", logger.clone()).unwrap();
        let c = get_asb("test2", "zmq", logger.clone()).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(get_asb("test2", "dummy", logger).is_err());
    }
}
