//! Main UCI (`uci:`) namespace.
//!
//! Provides the shared error types used throughout the Rust CAL.
//! Sub-modules contain generated message types for each OMS schema namespace.
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L §5.1.2 (Error Handling)
//! - OMSC-SPC-008 Rev K §9.2 (UCIException)

#![warn(missing_docs)]

use std::fmt;

pub mod base;
/// Generated UCI message types, produced from an XSD by the build script.
pub mod types;

// ════════════════════════════════════════════════════════════════════════════
// Sealed constructor token
// ════════════════════════════════════════════════════════════════════════════

/// Crate-private construction token embedded in generated message structs.
///
/// Users of the library can see struct fields but cannot construct a message
/// struct directly — only `AbstractServiceBus::create_message` may do so.
pub mod sealed {
    /// Opaque token used in the sealed-trait pattern to prevent external
    /// construction of generated message structs.
    ///
    /// The inner field is `pub(crate)` so the code-generator (inside this
    /// crate) can emit `Token(())` in `cal_create()` implementations.
    /// External crates see the field but cannot name the unit type to
    /// construct it — only code within this crate can write `Token(())`.
    #[derive(Debug, Clone, Default)]
    pub struct Token(pub(crate) ());
}

// ════════════════════════════════════════════════════════════════════════════
// CalMessage / CalSubMessage
// ════════════════════════════════════════════════════════════════════════════

/// Marker trait for CAL Message types — top-level, publishable messages.
///
/// Only types implementing `CalMessage` may be associated with a Client Topic
/// (CERT CAL-005208), published via an `AbstractWriter`, or received via an
/// `AbstractReader`.
///
/// Abstract message types (schema `abstract="true"`) must NOT implement this
/// trait (CERT CAL-016035). Only concrete, instantiable message types are
/// permitted.
pub trait CalMessage: Send + Sync + 'static {
    /// Returns the fully-qualified OMS message type name as defined in the
    /// OMS Message Schema. Used to enforce one-type-per-topic association
    /// (CERT CAL-005208).
    ///
    /// The returned [`QName`](crate::QName) carries the namespace URI and local
    /// name; its `Display` form is the shortest resolvable representation as
    /// determined when the code was generated (bare local name for the default
    /// namespace, `prefix:local` for mapped namespaces, Clark notation as a
    /// last resort).
    fn message_type_name() -> crate::QName
    where
        Self: Sized;

    /// Creates a default-initialised instance of this message type.
    ///
    /// Not part of the public API — use
    /// [`AbstractServiceBusCreateMessage::create_message`] instead.
    #[doc(hidden)]
    fn cal_create() -> Self
    where
        Self: Sized;

    /// Validates this message against its XSD schema constraints.
    ///
    /// Returns `Ok(())` if all fields satisfy their schema constraints.
    /// Returns `Err(ValidationError)` for the first failing field, with a
    /// dot-separated path (e.g. `"SystemStatus.MessageHeader.SystemId"`) and
    /// a human-readable reason.
    ///
    /// Generated element wrapper types override this with a full recursive
    /// check. The default always returns `Ok(())`.
    fn is_valid(&self) -> Result<(), ValidationError> {
        Ok(())
    }

    /// Returns a mutable reference to the underlying [`MessageType`](crate::uci::types::MessageType)
    /// if this message has a `MessageHeader` and `SecurityInformation`.
    ///
    /// Generated top-level message wrappers override this to return `Some(self)`.
    /// The default returns `None` (e.g. for test stubs that do not implement the full schema).
    fn as_message_type_mut(&mut self) -> Option<&mut dyn crate::uci::types::MessageType> {
        None
    }
}

// ════════════════════════════════════════════════════════════════════════════
// ValidationError
// ════════════════════════════════════════════════════════════════════════════

/// Describes a schema constraint violation found by [`CalMessage::is_valid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// Dot-separated path to the failing field,
    /// e.g. `"SystemStatus.MessageHeader.SystemId"`.
    pub path: String,
    /// Human-readable failure reason, e.g. `"enum not set"`.
    pub reason: String,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for ValidationError {}

/// Marker trait for CAL Sub-message types — complex nested field types.
///
/// Sub-messages cannot be independently published or subscribed to;
/// they exist only within an enclosing `CalMessage` (§5.5).
pub trait CalSubMessage: Send + Sync + 'static {}

// ════════════════════════════════════════════════════════════════════════════
// CalErrorKind
// ════════════════════════════════════════════════════════════════════════════

/// Classification of error conditions raised by the CAL.
///
/// # CERT coverage
/// CAL-005204, CAL-005369, CAL-016035, CAL-016043, CAL-016050, CAL-016477
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalErrorKind {
    /// CAL instance failed to initialize (CAL-005204).
    InitializationFailure,

    /// The requested Client Topic is unavailable (CAL-005369).
    TopicUnavailable,

    /// Required platform resources are not available for the operation
    /// (CAL-016043).
    ResourcesUnavailable,

    /// The operation is invalid in the current ASB connection state
    /// (Table 5.9-2).
    InvalidState {
        /// The ASB state when the error occurred.
        current: crate::asb::AsbConnectionState,
    },

    /// A UUID supplied to or generated by the CAL does not conform to
    /// RFC 4122 (CAL-016477).
    UuidConformanceError,

    /// The operation is not permitted given the current reader or writer
    /// configuration (CAL-016050 — polling and callback modes are mutually
    /// exclusive).
    OperationNotPermitted,

    /// The ASB has transitioned to the terminal `Failed` state; blocked reads
    /// are released with this error (Table 5.9-2).
    AsbFailed,

    /// The supplied `service_identifier` is invalid or not registered on this
    /// platform.
    InvalidServiceIdentifier,

    /// Serialization or deserialization of a CAL Message failed.
    SerializationError,

    /// Attempted to instantiate an abstract CAL Message or Sub-message type
    /// (CAL-016035).
    AbstractInstantiation,

    /// An internal error within the CAL Implementation.
    ImplementationError {
        /// Optional detail about the implementation error.
        kind: Option<CalImplementationErrorKind>,
    },

    /// A message failed schema validation before being sent.
    ValidationError(ValidationError),
}

impl fmt::Display for CalErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitializationFailure => write!(f, "CAL initialization failure"),
            Self::TopicUnavailable => write!(f, "Client topic unavailable"),
            Self::ResourcesUnavailable => write!(f, "Required resources unavailable"),
            Self::InvalidState { current } => {
                write!(f, "Operation invalid in ASB state '{current}'")
            }
            Self::UuidConformanceError => write!(f, "UUID does not conform to RFC 4122"),
            Self::OperationNotPermitted => write!(f, "Operation not permitted"),
            Self::AsbFailed => write!(f, "Abstract Service Bus has permanently failed"),
            Self::InvalidServiceIdentifier => {
                write!(f, "Invalid or unregistered service identifier")
            }
            Self::SerializationError => write!(f, "CAL Message (de)serialization error"),
            Self::AbstractInstantiation => {
                write!(f, "Cannot instantiate abstract CAL Message type")
            }
            Self::ImplementationError { kind: None } => {
                write!(f, "Internal CAL implementation error")
            }
            Self::ImplementationError {
                kind: Some(CalImplementationErrorKind::ConfigError),
            } => write!(f, "Unable to parse or interpret CAL configuration"),
            Self::ImplementationError {
                kind: Some(CalImplementationErrorKind::ListenerError),
            } => write!(f, "Status listener error"),
            Self::ValidationError(e) => write!(f, "Message validation failed: {e}"),
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// CalImplementationErrorKind
// ════════════════════════════════════════════════════════════════════════════

/// Detail for `CalErrorKind::ImplementationError`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalImplementationErrorKind {
    /// An error parsing or interpreting the CAL configuration file.
    ConfigError,
    /// An error occurred registering or unregistering a status listener.
    ListenerError,
}

// ════════════════════════════════════════════════════════════════════════════
// CalError
// ════════════════════════════════════════════════════════════════════════════

/// The primary error type for all CAL operations.
///
/// Equivalent to `UCIException` in the C++ and Java CAL specifications
/// (OMSC-SPC-008 §9.2).  All fallible CAL operations return
/// `CalResult<T> = Result<T, CalError>` rather than using panics or
/// exception-like constructs.
///
/// # Fields
///
/// Fields are **private** to prevent external mutation; use the accessor
/// methods [`kind`][CalError::kind], [`message`][CalError::message], and the
/// [`std::error::Error::source`] method to inspect the error chain.
///
/// # CERT coverage
/// CAL-005204, CAL-005369, CAL-016035, CAL-016043, CAL-016050, CAL-016477
#[derive(Debug)]
pub struct CalError {
    kind: CalErrorKind,
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl CalError {
    /// Constructs a `CalError` with the given classification and message.
    pub fn new(kind: CalErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
        }
    }

    /// Constructs a `CalError` wrapping an existing error as its causal source.
    pub fn with_source(
        kind: CalErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Constructs a `CalError` with kind `ImplementationError { kind: Some(k) }`.
    pub fn new_impl(kind: CalImplementationErrorKind, message: impl Into<String>) -> Self {
        Self::new(
            CalErrorKind::ImplementationError { kind: Some(kind) },
            message,
        )
    }

    /// Constructs a `CalError` with kind `ImplementationError { kind: Some(k) }`
    /// and a causal source.
    pub fn with_impl_source(
        kind: CalImplementationErrorKind,
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::with_source(
            CalErrorKind::ImplementationError { kind: Some(kind) },
            message,
            source,
        )
    }

    /// Returns the error classification.
    pub fn kind(&self) -> &CalErrorKind {
        &self.kind
    }

    /// Returns the human-readable error description.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns `true` if this error represents an ASB terminal failure.
    pub fn is_asb_failure(&self) -> bool {
        self.kind == CalErrorKind::AsbFailed
    }
}

impl fmt::Display for CalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            Some(src) => write!(f, "{}: {}\n  caused by: {src}", self.kind, self.message),
            None => write!(f, "{}: {}", self.kind, self.message),
        }
    }
}

impl std::error::Error for CalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// CalResult
// ════════════════════════════════════════════════════════════════════════════

/// Convenience `Result` alias for all fallible CAL operations.
///
/// Apply `#[must_use]` at call sites where silently discarding errors would
/// be a bug (most write and registration operations).
pub type CalResult<T> = Result<T, CalError>;
