//! # OMS Rust Critical Abstraction Layer – Abstract Service Bus Interfaces
//!
//! Abstract Rust trait definitions for the OMS CAL Abstract Service Bus (ASB).
//! These types and traits constitute the contract that any platform-provided CAL
//! Implementation must satisfy.
//!
//! ## Specification references
//! - OMSC-SPC-001 rev L – CAL Specification
//! - OMSC-SPC-008 rev K – C++ CAL Interface Generation Specification
//!
//! ## Design notes
//! * [`AbstractServiceBus`] is object-safe; generic factory methods live in
//!   the non-object-safe extension trait [`AbstractServiceBusExt`].
//! * `Send + Sync` is required on all shared types (§5.1.1, CAL-016015).
//! * `Box<Self>` receivers are used for consuming trait-object methods, which
//!   is the Rust equivalent of C++ destructors and `shutdown()` calls.
//!

#![allow(dead_code)]
#![warn(missing_docs)]

pub use uuid::timestamp::context::ContextV1;

use crate::calconfig::{CAL_CONFIG, UUIDFactoryType};
use crate::uci::{CalError, CalErrorKind, CalResult};
use std::fmt;
use uuid::{Uuid, Variant as UuidVariant, Version as UuidVersion};

/// Re-exports for uci::asb
///
pub use crate::asb::{
    AbstractReader, AbstractServiceBus, AbstractServiceBusExt, AbstractWriter,
    AsbConnectionState, AsbStatus, AsbStatusListener, MessageListener,
    Expiration, MessageBuffer, Reliability, TimeBasedFilter, TopicQos,
};
pub use crate::calconfig::SerializationFormat;

/// Re-exported [`uuid::Timestamp`] so callers can build version-1 UUID
/// timestamps without declaring a direct `uuid` crate dependency.
///
/// Used with [`UUID::generate_v1`].
pub use uuid::Timestamp as UuidTimestamp;

/// RFC 4122–conformant Universally Unique Identifier backed by
/// [`uuid::Uuid`].
///
/// Wraps the `uuid` crate (features `v1`, `v3`, `v4`, `slog`) and enforces
/// OMS invariants at **every construction site**:
///
/// * **Variant** must be [`UuidVariant::RFC4122`] (CAL-016479).
///   The nil UUID (all-zero bytes) is exempt – it carries no meaningful
///   variant or version bits.
/// * **Version** must be one of:
///   - `v1` / [`UuidVersion::Mac`] – time-based
///   - `v3` / [`UuidVersion::Md5`] – MD5 name-based
///   - `v4` / [`UuidVersion::Random`] – randomly generated
///     (CAL-005181)
///
/// Constructors that accept external data ([`parse_str`][UUID::parse_str],
/// [`from_octets`][UUID::from_octets], [`try_from_raw`][UUID::try_from_raw])
/// return [`CalResult`].  Generation methods
/// ([`generate_v4`][UUID::generate_v4], [`generate_v1`][UUID::generate_v1],
/// [`generate_v3`][UUID::generate_v3]) are infallible because the `uuid`
/// crate always produces conformant output.
///
/// ## `slog` support
/// When the crate feature `slog` is enabled, `UUID` implements
/// [`slog::Value`] by delegating to [`uuid::Uuid`]'s own implementation
/// (also enabled by the `slog` feature of the `uuid` dependency).  The UUID
/// is serialised as its canonical lowercase-hyphenated string.
///
/// # CERT coverage
/// CAL-016477, CAL-016479, CAL-005181
#[derive(
    Clone, Copy, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct UUID(Uuid);

impl UUID {
    // ── Private validation ────────────────────────────────────────────────

    /// Validate that `uuid` satisfies OMS invariants and wrap it.
    ///
    /// The nil UUID (all-zero bytes) is unconditionally accepted – its bytes
    /// contain no meaningful variant or version fields.  All other UUIDs must
    /// carry RFC 4122 variant bits and a version number of 1, 3, or 4.
    fn validate(uuid: Uuid) -> CalResult<Self> {
        if uuid.is_nil() {
            return Ok(Self(uuid));
        }

        // ── Variant check (CAL-016479) ────────────────────────────────────
        if uuid.get_variant() != UuidVariant::RFC4122 {
            return Err(CalError::new(
                CalErrorKind::UuidConformanceError,
                format!(
                    "UUID variant `{:?}` does not satisfy RFC 4122 \
                     (expected Variant::RFC4122)",
                    uuid.get_variant()
                ),
            ));
        }

        // ── Version check (CAL-005181) ────────────────────────────────────
        // Only v1 (time-based), v3 (MD5 name-based), and v4 (random) are
        // permitted by the OMS CAL specification.
        match uuid.get_version() {
            Some(UuidVersion::Mac)      // v1 – time-based
            | Some(UuidVersion::Md5)    // v3 – MD5 name-based
            | Some(UuidVersion::Random) // v4 – randomly generated
            => Ok(Self(uuid)),

            other => Err(CalError::new(
                CalErrorKind::UuidConformanceError,
                format!(
                    "UUID version `{:?}` is not permitted; \
                     OMS allows only v1 (Mac), v3 (Md5), and v4 (Random)",
                    other
                ),
            )),
        }
    }

    // ── Parsing / conversion ──────────────────────────────────────────────

    /// Parse a hyphenated RFC 4122 UUID string, e.g.
    /// `"550e8400-e29b-41d4-a716-446655440000"`.
    ///
    /// Delegates to [`uuid::Uuid::parse_str`].
    ///
    /// Returns [`CalErrorKind::UuidConformanceError`] if the string is
    /// syntactically malformed or the resulting UUID violates OMS invariants.
    ///
    /// Mirrors `uci::base::UUID::fromString()` (OMSC-SPC-008 §9.8.1.2.1).
    ///
    /// # CERT coverage
    /// CAL-016477
    pub fn parse_str(s: &str) -> CalResult<Self> {
        let uuid = Uuid::parse_str(s).map_err(|e| {
            CalError::new(
                CalErrorKind::UuidConformanceError,
                format!("UUID parse error: {e}"),
            )
        })?;
        Self::validate(uuid)
    }

    /// Construct from 16 big-endian (network-order) octets.
    ///
    /// Delegates to [`uuid::Uuid::from_bytes`].
    ///
    /// Returns [`CalErrorKind::UuidConformanceError`] if the bytes represent
    /// a UUID that violates OMS invariants.
    ///
    /// Mirrors `uci::base::UUID::fromOctets()` (OMSC-SPC-008 §9.8.1.2.2).
    ///
    /// # CERT coverage
    /// CAL-016477
    pub fn from_octets(bytes: [u8; 16]) -> CalResult<Self> {
        Self::validate(Uuid::from_bytes(bytes))
    }

    /// Wrap a raw [`uuid::Uuid`] after validating OMS invariants.
    ///
    /// Returns [`CalErrorKind::UuidConformanceError`] if `uuid` violates OMS
    /// constraints.
    ///
    /// # CERT coverage
    /// CAL-016477
    pub fn try_from_raw(uuid: Uuid) -> CalResult<Self> {
        Self::validate(uuid)
    }

    // ── Generation ────────────────────────────────────────────────────────

    /// Return the nil UUID (all-zero bytes).  Always valid.
    ///
    /// Delegates to [`uuid::Uuid::nil`].
    ///
    /// Mirrors the default-constructed `uci::base::UUID`
    /// (OMSC-SPC-008 §9.8.1.2.23.1).
    pub const fn nil() -> Self {
        Self(Uuid::nil())
    }

    /// Generate a new UUID using the configured type.
    /// This is configured via the configuration section "UUIDFactoryType"
    pub fn generate() -> Self {
        let config = CAL_CONFIG.lock().unwrap();
        match config.uuidfactory.type_ {
            UUIDFactoryType::Random => Self::generate_v4(),
            UUIDFactoryType::TimeBased => {
                let ctx = ContextV1::new_random();
                let ts = UuidTimestamp::now(&ctx);
                if let Some(node) = config.uuidfactory.node {
                    Self::generate_v1(ts, &node.bytes())
                } else {
                    Self::generate_v1(
                        ts,
                        &mac_address::get_mac_address().unwrap().unwrap().bytes(),
                    )
                }
            }
        }
    }

    /// Generate a random (version 4) UUID using a cryptographically secure
    /// pseudo-random number generator.
    ///
    /// Delegates to [`uuid::Uuid::new_v4`].  Infallible – the `uuid` crate
    /// always produces a valid RFC 4122 v4 UUID.
    ///
    /// Mirrors `uci::base::UUID::generateUUID()` (OMSC-SPC-008 §9.8.1.2.3).
    ///
    /// # CERT coverage
    /// CAL-005181, CAL-016477, CAL-016479
    pub fn generate_v4() -> Self {
        Self(Uuid::new_v4())
    }

    /// Generate a time-based (version 1) UUID.
    ///
    /// Delegates to [`uuid::Uuid::new_v1`].  Infallible – the `uuid` crate
    /// always produces a valid RFC 4122 v1 UUID.
    ///
    /// `timestamp` is a [`UuidTimestamp`] (re-exported [`uuid::Timestamp`]);
    /// `node_id` is the 6-byte IEEE 802 MAC address or a randomly generated
    /// substitute.
    ///
    /// # Example
    /// ```rust,no_run
    /// use rcal::uci::base::{UUID, UuidTimestamp, ContextV1};
    ///
    /// let ctx = ContextV1::new(42);
    /// let ts  = UuidTimestamp::now(&ctx);
    /// let id  = UUID::generate_v1(ts, &[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    /// assert!(id.is_valid());
    /// ```
    ///
    /// Mirrors `uci::base::UUID::generateUUID()` (OMSC-SPC-008 §9.8.1.2.3).
    ///
    /// # CERT coverage
    /// CAL-005181, CAL-016477, CAL-016479
    pub fn generate_v1(timestamp: UuidTimestamp, node_id: &[u8; 6]) -> Self {
        Self(Uuid::new_v1(timestamp, node_id))
    }

    /// Generate a deterministic name-based (version 3) UUID.
    ///
    /// Delegates to [`uuid::Uuid::new_v3`]; computes the MD5 hash of the
    /// concatenation of `namespace` and `name`.  The same `(namespace, name)`
    /// pair always produces the same UUID, making this suitable for
    /// deterministic (*a priori*) identifiers.
    ///
    /// Infallible – the `uuid` crate always produces a valid RFC 4122 v3 UUID.
    ///
    /// Mirrors
    /// `uci::base::UUID::createVersion3UUID(const UUID&, const string&)`
    /// (OMSC-SPC-008 §9.8.1.2.5.1).
    ///
    /// # CERT coverage
    /// CAL-005181, CAL-016477, CAL-016479
    pub fn generate_v3(namespace: &UUID, name: &[u8]) -> Self {
        Self(Uuid::new_v3(&namespace.0, name))
    }

    // ── Inspection ────────────────────────────────────────────────────────

    /// Return `true` if this is the nil UUID (all bytes zero).
    ///
    /// Delegates to [`uuid::Uuid::is_nil`].
    ///
    /// Mirrors `uci::base::UUID::isNil()` (OMSC-SPC-008 §9.8.1.2.20).
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }

    /// Return `true` if this UUID satisfies OMS invariants:
    /// nil **or** (RFC 4122 variant **and** version ∈ {1, 3, 4}).
    ///
    /// For [`UUID`] values obtained via the public constructors this is
    /// always `true`; the method is provided for defensive cross-checking.
    ///
    /// Mirrors `uci::base::UUID::isValid()` (OMSC-SPC-008 §9.8.1.2.21).
    ///
    /// # CERT coverage
    /// CAL-016477
    pub fn is_valid(&self) -> bool {
        if self.0.is_nil() {
            return true;
        }
        if self.0.get_variant() != UuidVariant::RFC4122 {
            return false;
        }
        matches!(
            self.0.get_version(),
            Some(UuidVersion::Mac) | Some(UuidVersion::Md5) | Some(UuidVersion::Random)
        )
    }

    /// Return the UUID variant.
    ///
    /// Delegates to [`uuid::Uuid::get_variant`].
    ///
    /// Mirrors `uci::base::UUID::getVariant()` (OMSC-SPC-008 §9.8.1.2.9).
    pub fn get_variant(&self) -> UuidVariant {
        self.0.get_variant()
    }

    /// Return the UUID version, if recognised.
    ///
    /// Delegates to [`uuid::Uuid::get_version`].
    ///
    /// Mirrors `uci::base::UUID::getVersion()` (OMSC-SPC-008 §9.8.1.2.10).
    pub fn get_version(&self) -> Option<UuidVersion> {
        self.0.get_version()
    }

    /// Return the raw 16-byte big-endian representation.
    ///
    /// Delegates to [`uuid::Uuid::as_bytes`].
    ///
    /// Mirrors `uci::base::UUID::getOctets()` (OMSC-SPC-008 §9.8.1.2.6).
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    /// Borrow the underlying [`uuid::Uuid`].
    pub fn as_raw(&self) -> &Uuid {
        &self.0
    }

    /// Consume `self` and return the underlying [`uuid::Uuid`].
    pub fn into_raw(self) -> Uuid {
        self.0
    }
}

// ── std trait implementations ─────────────────────────────────────────────

impl fmt::Display for UUID {
    /// Formats as the lowercase hyphenated RFC 4122 string, e.g.
    /// `550e8400-e29b-41d4-a716-446655440000`.
    ///
    /// Delegates to [`uuid::Uuid`]'s [`Display`][fmt::Display] impl.
    ///
    /// Mirrors `uci::base::UUID::toString()` (OMSC-SPC-008 §9.8.1.2.8).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for UUID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UUID({})", self.0)
    }
}

impl std::str::FromStr for UUID {
    type Err = CalError;

    /// Parse a hyphenated RFC 4122 UUID string.
    ///
    /// Equivalent to [`UUID::parse_str`]; provided so that the idiomatic
    /// Rust expression `"…".parse::<UUID>()` works.
    fn from_str(s: &str) -> CalResult<Self> {
        UUID::parse_str(s)
    }
}

/// [`slog`] structured-logging support – delegates to [`uuid::Uuid`]'s own
/// `slog::Value` implementation (enabled by the `slog` feature of the `uuid`
/// crate).  The UUID is serialised as its canonical lowercase-hyphenated
/// string.
///
/// # Cargo setup
/// Enable the `slog` feature of this crate **and** declare `slog` as a direct
/// dependency.  Because `uuid` already pulls `slog` in as a transitive
/// dependency when its own `slog` feature is active, adding
/// `slog = "2"` to `[dependencies]` is sufficient.
///
/// ```toml
/// [features]
/// slog = ["dep:slog"]
///
/// [dependencies]
/// slog = { version = "2", optional = true }
/// uuid = { version = "1", features = ["v1", "v3", "v4", "slog"] }
/// ```
impl slog::Value for UUID {
    fn serialize(
        &self,
        record: &slog::Record<'_>,
        key: slog::Key,
        serializer: &mut dyn slog::Serializer,
    ) -> slog::Result {
        // Forward to uuid::Uuid's slog::Value impl, which the `slog` feature
        // of the uuid crate provides.
        slog::Value::serialize(&self.0, record, key, serializer)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// §4  Service UUID aggregate  (CAL-005203)
// ════════════════════════════════════════════════════════════════════════════

/// Aggregated UUIDs that identify a CAL Client's service context.
///
/// Obtained after successful initialisation via
/// [`AbstractServiceBus::service_uuids`].
///
/// # CERT coverage
/// CAL-005203
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceUuids {
    /// UUID of the OMS Platform System.
    pub system: UUID,
    /// UUID of this Service instance.
    pub service: UUID,
    /// UUID of the Subsystem, if applicable.
    pub subsystem: Option<UUID>,
    /// UUIDs of named components within this Service.
    pub components: Vec<UUID>,
    /// UUIDs of named capabilities within this Service.
    pub capabilities: Vec<UUID>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calconfig::UUIDFactoryType;
    use rcal_macros::init_test_logger;
    use slog::debug;

    #[test]
    fn test_uuid_factory() {
        let logger = init_test_logger!();

        debug!(logger, "Default (random): {}", UUID::generate());
        {
            let mut config = CAL_CONFIG.lock().unwrap();
            config.uuidfactory.type_ = UUIDFactoryType::TimeBased;
            debug!(logger, "Change to time based");
        }
        debug!(logger, "TimeBased: {}", UUID::generate());
        {
            let mut config = CAL_CONFIG.lock().unwrap();
            config.uuidfactory.node = mac_address::get_mac_address().unwrap();
            debug!(
                logger,
                "Time based with local node {}",
                mac_address::get_mac_address().unwrap().unwrap()
            );
        }
        debug!(logger, "TimeBased: {}", UUID::generate());
    }
}
