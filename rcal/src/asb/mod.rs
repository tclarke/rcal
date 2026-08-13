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
use crate::uci::base::ServiceUuids;
use crate::uci::{CalError, CalErrorKind, CalResult};
use lazy_static::lazy_static;
use std::collections::HashMap;
use std::env;
use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

mod zmq;
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
    fn on_status_change(&mut self, status: &AsbStatus);
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

    /// Returns the UUIDs identifying System, Service, Subsystem, Components,
    /// and Capabilities (CERT CAL-005203).
    fn service_uuids(&self) -> CalResult<&ServiceUuids>;

    /// Version of the OMS Schema Definition used to generate the CAL.
    fn oms_schema_version(&self) -> &str;

    /// Version of the OMS Schema Compiler used to generate the CAL.
    fn oms_schema_compiler_version(&self) -> &str;

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
    fn register_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()>;

    /// Unregisters a previously registered ASB status listener.
    ///
    /// After this returns, `on_status_change` will no longer be invoked.
    /// No-op if the listener is not registered.
    fn unregister_status_listener(
        &mut self,
        listener: Arc<Mutex<dyn AsbStatusListener>>,
    ) -> CalResult<()>;

    // ── Lifecycle ─────────────────────────────────────────────────────────

    /// Shuts down the CAL instance and releases all associated resources.
    ///
    /// After `close()` returns, all Writers and Readers are invalidated,
    /// registered listeners will not receive further callbacks, and blocked
    /// `read()` calls are released with an error.
    fn close(&mut self) -> CalResult<()>;
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
    static ref ASB_FACTORY: Mutex<AsbFactoryMap> = Mutex::new(AsbFactoryMap::new());
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

    let mut map = ASB_FACTORY.lock().unwrap();

    // Return an existing instance without constructing a new one.
    if let Some(existing) = map.get(&key) {
        return Ok(Arc::clone(existing));
    }

    // Resolve transport: exact match first, then fall back to default.
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

    // Construct the appropriate ASB implementation.
    let instance: AsbInstance = match transport.type_.as_str() {
        ZMQ_ASB_ID => Arc::new(Mutex::new(ZmqAsb::new(
            key.service_identifier.clone(),
            key.asb_identifier.clone(),
            logger,
            Arc::clone(&config),
            transport,
        ).await?)),
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
    use crate::calconfig::{get_test_config_path, parse_config_from_file};
    use rcal_macros::init_test_logger;

    fn test_config() -> Arc<CalConfig> {
        Arc::new(
            parse_config_from_file(&get_test_config_path("calconfig_sample.toml"))
                .expect("test config must load"),
        )
    }

    #[tokio::test]
    async fn test_asb_factory_same_key_returns_same_instance() {
        let config = test_config();
        let logger = init_test_logger!();

        let a = get_asb("test_svc", "TestZmq", Arc::clone(&config), logger.clone()).await
            .expect("first get_asb must succeed");
        let b = get_asb("test_svc", "TestZmq", Arc::clone(&config), logger.clone()).await
            .expect("second get_asb must succeed");
        let c = get_asb("test_svc_2", "TestZmq", Arc::clone(&config), logger.clone()).await
            .expect("different service must succeed");

        // Same (service, asb) key → same Arc (CERT CAL-005202)
        assert!(Arc::ptr_eq(&a, &b), "same key should return the same Arc");
        // Different service key → different Arc
        assert!(
            !Arc::ptr_eq(&a, &c),
            "different service key must be a distinct instance"
        );
    }

    #[tokio::test]
    async fn test_asb_factory_unknown_transport_returns_err() {
        let logger = init_test_logger!();
        let no_default_config = Arc::new(
            parse_config_from_file(&get_test_config_path("calconfig_no_default.toml")).unwrap(),
        );
        assert!(get_asb("svc", "dummy", no_default_config, logger).await.is_err());
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
