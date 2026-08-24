//! Abstract Service Bus (ASB) interface — pure transport layer.
//!
//! Provides:
//!   - Service identity and UUID retrieval (CERT CAL-005203)
//!   - ASB connection state machine and status (§5.9)
//!   - ASB connection lifecycle and status callbacks (CERT CAL-016366)
//!
//! Application-facing concerns (message creation, typed writers/readers, QoS)
//! live in [`crate::cal`].
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L §5.3, §5.9, Table 5.9-1/2, Figure 5.9-1/2
//! - OMSC-SPC-008 Rev K §9.9 (`AbstractServiceBusConnection`)

#![allow(dead_code)]
#![warn(missing_docs)]

use crate::uci::base::UUID;
use crate::uci::{CalError, CalErrorKind, CalResult};
use std::env;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

#[cfg(feature = "zmq")]
pub mod zmq;

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

/// Pure transport-layer CAL instance interface (object-safe).
///
/// Covers identity (service/system UUIDs — CERT CAL-005203), connection status
/// polling and callback (§5.9, CERT CAL-016366), and lifecycle (close()).
/// Has no knowledge of message types.
///
/// Application-facing concerns (message creation, typed writers/readers, QoS)
/// are in [`crate::cal::AbstractCal`] which extends this trait.
///
/// # CERT coverage
/// CAL-005201, CAL-005202, CAL-005203, CAL-016366
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
    /// CERT CXX-011176 (`getAbstractServiceBusConnectionVersion()`).
    fn get_asb_connection_version(&self) -> &str;

    /// Version string identifying the OMS API against which this CAL was built.
    ///
    /// CERT CXX-012694 (`getOMSApiVersion()`).
    fn get_oms_api_version(&self) -> &str;

    // ── Connection Status — Polling ───────────────────────────────────────

    /// Returns the current ASB connection status (polling interface, §5.9).
    fn connection_status(&self) -> &AsbStatus;

    // ── Connection Status — Callback ──────────────────────────────────────

    /// Registers an ASB connection-status listener.
    ///
    /// `on_status_change` is called **immediately** with the current state
    /// (CERT CAL-016366) and subsequently on every state change.
    ///
    /// Returns `Err(InvalidState)` when called in the `Failed` state.
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
    fn close(&mut self) -> CalResult<()>;
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

    // Compile-only assertion — fails at definition site if AbstractServiceBus is not object-safe.
    #[allow(dead_code)]
    type _Asb = Arc<std::sync::Mutex<dyn AbstractServiceBus>>;
}

#[cfg(test)]
mod tests {
    use super::*;

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
