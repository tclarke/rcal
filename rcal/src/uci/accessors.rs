// =============================================================================
//  OMS Open Mission Systems — Rust Critical Abstraction Layer (CAL)
//  Core Interface Definitions
//
//  Document: (Future) Rust CAL Interface Generation Specification
//  References: OMSC-SPC-001 Rev L (Generic CAL Specification)
//              OMSC-SPC-008 Rev K (C++ CAL — inspiration)
//              IETF RFC 4122 (UUID)
//
//  Distribution Statement A. Approved for public release: distribution unlimited.
// =============================================================================
//
//  Crate dependencies (Cargo.toml):
//
//  [dependencies]
//  uuid = { version = "1", features = ["v1", "v3", "v4"] }
//  thiserror = "1"
//
// =============================================================================

// ─── Module declarations ─────────────────────────────────────────────────────
pub mod error;
pub mod uuid;
pub mod accessors;
pub mod asb;

// ─── Crate-level re-exports ───────────────────────────────────────────────────
pub use error::{CalError, CalErrorKind, CalResult};
pub use uuid::{CalUuid, ServiceUuids};
pub use accessors::{
    BoundedListField, CalMessage, CalSubMessage, ChoiceField, OptionalField, RequiredField,
};
pub use asb::{
    AbstractServiceBus, AsbConnectionState, AsbStatus, AsbStatusListener,
    CalInstanceConfig,
};

// =============================================================================
// MODULE: accessors
// Traits defining the field-level accessor interface for CAL Messages.
//
// In C++, these are generated from the OMS XSD by the CAL Generation Tool.
// In Rust, we define the trait contracts here; the generator emits structs
// that implement these traits for each message type.
//
// Key mappings from spec → Rust:
//   Optional field (§5.5.1.1)  → OptionalField<T>  (wraps Option<T>)
//   Required field (§5.5.1.2)  → RequiredField<T>
//   Bounded list   (§5.5.1.3)  → BoundedListField<T>
//   Choice         (§5.5.1.4)  → ChoiceField<V>
//   Sequence       (§5.5.1.5)  → plain struct fields / sub-messages
//   Enumeration    (§5.5.1.6)  → Rust enum wrapped in Option (uninitialized = None)
//
// References: §5.5 (CAL Messages), §5.5.1–§5.5.4, CERTs CAL-005254,
//             CAL-005264, CAL-005267, CAL-005290–CAL-005296, CAL-016033–038
// =============================================================================
pub mod accessors {
    use crate::error::{CalError, CalErrorKind, CalResult};

    // ─── Required Fields (§5.5.1.2) ──────────────────────────────────────────

    /// Read/write access to a **required** CAL Message field of type `T`.
    ///
    /// Required fields always contain a value. There is no enabled/disabled
    /// state. Upon message construction, required primitive fields are
    /// zero-initialized; required sub-message fields are default-constructed.
    ///
    /// C++ equivalent: a simple `get_fieldName()` / `set_fieldName()` pair.
    pub trait RequiredField<T> {
        /// Returns a shared reference to the field value.
        fn get(&self) -> &T;

        /// Returns a mutable reference to the field value.
        fn get_mut(&mut self) -> &mut T;

        /// Replaces the field value.
        fn set(&mut self, value: T);
    }

    // ─── Optional Fields (§5.5.1.1, §5.5.2.1, §5.5.4.1) ───────────────────

    /// Read/write access to an **optional** CAL Message field of type `T`.
    ///
    /// Optional fields begin in the **disabled** state upon message creation
    /// (CERT CAL-005254). The CAL Client must check `is_enabled()` before
    /// calling `get()` (CERT CAL-005293).
    ///
    /// # Rust Idiom
    ///
    /// Rust's `Option<T>` type naturally models enabled/disabled semantics.
    /// Generated message structs store optional fields as `Option<T>` internally;
    /// this trait provides the CAL-specified enable/disable vocabulary on top.
    ///
    /// C++ equivalent: `has_fieldName()`, `get_fieldName()`, `set_fieldName()`,
    ///                 `clear_fieldName()` generated accessor methods.
    pub trait OptionalField<T> {
        /// Returns `true` if this field has been populated (is enabled).
        ///
        /// Satisfies CERT CAL-005293.
        fn is_enabled(&self) -> bool;

        /// Returns a shared reference to the value if enabled, else `None`.
        ///
        /// Satisfies CERTs CAL-005294 (disabled → `None`) and
        /// CAL-005296 (enabled → `Some(&value)`).
        fn get(&self) -> Option<&T>;

        /// Returns a mutable reference to the value if enabled, else `None`.
        fn get_mut(&mut self) -> Option<&mut T>;

        /// Enables the field by setting its value.
        ///
        /// Satisfies CERT CAL-005290 (enable mechanism).
        fn set(&mut self, value: T);

        /// Disables the field, removing its value.
        ///
        /// Satisfies CERT CAL-005290 (disable mechanism).
        fn disable(&mut self);

        /// Returns `Ok(&value)` when enabled, or a `CalError` with kind
        /// `OperationNotPermitted` when disabled.
        ///
        /// Convenience combinator; equivalent to the C++ pattern of calling
        /// `has_fieldName()` and then `get_fieldName()` with an assertion.
        fn get_or_err(&self) -> CalResult<&T> {
            self.get().ok_or_else(|| {
                CalError::new(
                    CalErrorKind::OperationNotPermitted,
                    "Accessed a disabled optional field; \
                     call is_enabled() before accessing optional field values \
                     (CERT CAL-005294)",
                )
            })
        }
    }

    // ─── Bounded Lists (§5.5.1.3, §5.5.2.3, §5.5.4.3) ──────────────────────

    /// Read/write access to a **bounded list** CAL Message field.
    ///
    /// Bounded lists are created in the **empty** state (CERT CAL-005264).
    /// All elements within a list share the same base type `T`.
    ///
    /// The maximum and minimum bounds are fixed by the OMS Message Schema
    /// and cannot be changed at runtime.
    ///
    /// Implements the full operation set required by §5.5.4.3.
    ///
    /// C++ equivalent: A `std::vector`-backed container with `getXxx()` /
    /// `setXxx()` / `addXxx()` / `clearXxx()` generated methods.
    pub trait BoundedListField<T> {
        /// Returns the current number of elements in the list.
        fn len(&self) -> usize;

        /// Returns the maximum number of elements allowed by the schema.
        fn max_bound(&self) -> usize;

        /// Returns the minimum number of elements required by the schema.
        fn min_bound(&self) -> usize;

        /// Returns `true` when the list contains no elements.
        fn is_empty(&self) -> bool {
            self.len() == 0
        }

        /// Pre-allocates capacity for `n` elements without changing the length.
        /// Has no observable semantic effect beyond performance.
        fn reserve(&mut self, n: usize);

        /// Appends an element to the end of the list.
        ///
        /// Returns `Err(ResourcesUnavailable)` if `len() == max_bound()`.
        fn push(&mut self, element: T) -> CalResult<()>;

        /// Removes and returns the last element, or `None` if the list is empty.
        fn pop(&mut self) -> Option<T>;

        /// Removes all elements, returning the list to the empty state.
        fn clear(&mut self);

        /// Returns a shared reference to the element at `index`,
        /// or `None` if `index >= len()`.
        fn get(&self, index: usize) -> Option<&T>;

        /// Returns a mutable reference to the element at `index`,
        /// or `None` if `index >= len()`.
        fn get_mut(&mut self, index: usize) -> Option<&mut T>;
    }

    // ─── Choices (§5.5.1.4, §5.5.2.4, §5.5.4.4) ─────────────────────────────

    /// Read/write access to a **choice** CAL Message field.
    ///
    /// A choice is a discriminated union: at any time it holds exactly one
    /// of several possible typed variants, or is uninitialized.
    ///
    /// Choice fields are created in the **uninitialized** state
    /// (CERT CAL-005267).
    ///
    /// # Type Parameter
    ///
    /// `V` is a Rust `enum` generated from the XSD `<xs:choice>` element.
    /// Each enum variant wraps the data for that choice branch.
    ///
    /// C++ equivalent: A union-like accessor with `getXxxChoice()`,
    /// `setXxx()`, `getChoiceType()`, `clearChoice()` methods.
    pub trait ChoiceField<V> {
        /// Returns `true` if a variant has been selected (the field is initialized).
        fn is_initialized(&self) -> bool;

        /// Returns a shared reference to the active variant, or `None` if
        /// the field is uninitialized.
        fn get(&self) -> Option<&V>;

        /// Returns a mutable reference to the active variant, or `None` if
        /// the field is uninitialized.
        fn get_mut(&mut self) -> Option<&mut V>;

        /// Sets the active variant, replacing any previous selection.
        fn set(&mut self, variant: V);

        /// Returns the field to the uninitialized state, dropping any value.
        fn clear(&mut self);
    }

    // ─── CAL Message / Sub-message Marker Traits ─────────────────────────────

    /// Marker trait for CAL Message types — top-level, publishable messages.
    ///
    /// Only types implementing `CalMessage` may be:
    /// - Associated with a Client Topic (CERT CAL-005208)
    /// - Published via a `CalWriter`
    /// - Received via a `CalReader`
    ///
    /// Sub-messages (nested complex types) implement `CalSubMessage` instead
    /// and cannot be independently published (§5.5).
    ///
    /// Abstract message types (schema `abstract="true"`) must NOT implement
    /// this trait (CERT CAL-016035). Only concrete, instantiable message
    /// types are permitted.
    pub trait CalMessage: Send + Sync + 'static {
        /// Returns the fully-qualified OMS message type name as defined in the
        /// OMS Message Schema. Used to enforce one-type-per-topic association
        /// (CERT CAL-005208).
        ///
        /// Example: `"uci.core.SystemStatusType"`
        fn message_type_name() -> &'static str
        where
            Self: Sized;
    }

    /// Marker trait for CAL Sub-message types — complex nested field types.
    ///
    /// Sub-messages cannot be independently published or subscribed to;
    /// they only exist within the context of an enclosing `CalMessage` (§5.5).
    pub trait CalSubMessage: Send + Sync + 'static {}
}

// =============================================================================
// MODULE: asb
// AbstractServiceBus — the central CAL instance interface.
//
// Provides:
//   - Service identity and UUID retrieval (CERT CAL-005203)
//   - CAL instance lifecycle management (CERT CAL-005201, CAL-005202)
//   - ASB connection status (polling + callback) (§5.9, CERT CAL-016366)
//   - Factory infrastructure for Writers and Readers (§5.6, §5.7)
//
// References: §4.12, §5.3, §5.9, Table 5.9-1, Table 5.9-2,
//             Figure 5.9-1, Figure 5.9-2
// =============================================================================
pub mod asb {
    use std::sync::Arc;
    use crate::error::{CalError, CalErrorKind, CalResult};
    use crate::uuid::ServiceUuids;

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
                (Self::Initializing, Self::Normal)      => true,
                (Self::Initializing, Self::Degraded)    => true,
                (Self::Initializing, Self::Inoperable)  => true,
                (Self::Initializing, Self::Failed)      => true,
                // From NORMAL
                (Self::Normal, Self::Degraded)          => true,
                (Self::Normal, Self::Inoperable)        => true,
                (Self::Normal, Self::Failed)            => true,
                // From DEGRADED
                (Self::Degraded, Self::Normal)          => true,
                (Self::Degraded, Self::Inoperable)      => true,
                (Self::Degraded, Self::Failed)          => true,
                // From INOPERABLE
                (Self::Inoperable, Self::Initializing)  => true,
                (Self::Inoperable, Self::Normal)        => true,
                (Self::Inoperable, Self::Degraded)      => true,
                (Self::Inoperable, Self::Failed)        => true,
                // From FAILED — terminal, no transitions out
                (Self::Failed, _)                       => false,
                // Self-transitions are no-ops; caller should not call this
                _                                       => false,
            };
            if allowed {
                Ok(())
            } else {
                Err(CalError::new(
                    CalErrorKind::InvalidState,
                    format!(
                        "ASB state transition {:?} → {:?} is not permitted \
                         (Figure 5.9-2, OMSC-SPC-001 Rev L)",
                        self, next
                    ),
                ))
            }
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
            Self { state, description: description.into() }
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
        fn on_status_change(&self, status: &AsbStatus);
    }

    // ─── AbstractServiceBus ───────────────────────────────────────────────────

    /// The central CAL instance interface — the Rust equivalent of the C++
    /// `AbstractServiceBus` abstract class.
    ///
    /// Each `AbstractServiceBus` instance represents a single CAL instance
    /// (§4.6) bound to a unique (Service Identifier, ASB Identifier) pair
    /// (CERT CAL-005202). A CAL Client obtains a fully initialized instance
    /// via the [`CalInstanceFactory`] function (CERT CAL-005201).
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
    /// # Usage Pattern
    ///
    /// ```rust
    /// // Obtain via factory (CERT CAL-005201)
    /// let config = CalInstanceConfig {
    ///     service_identifier: "SensorFusion".to_string(),
    ///     asb_identifier:     "primary_network".to_string(),
    ///     network_config:     "/etc/oms/network.xml".to_string(),
    /// };
    /// let mut bus: Box<dyn AbstractServiceBus> = cal_factory(config)?;
    ///
    /// // Retrieve identity
    /// let uuids = bus.service_uuids()?;
    /// println!("Service UUID: {}", uuids.service);
    ///
    /// // Poll connection status
    /// let status = bus.connection_status();
    /// assert_eq!(status.state, AsbConnectionState::Normal);
    ///
    /// // Register callback listener
    /// let listener = Arc::new(MyStatusListener::new());
    /// bus.register_status_listener(listener)?;
    ///
    /// // ... use writers and readers ...
    ///
    /// // Shutdown
    /// bus.close()?;
    /// ```
    pub trait AbstractServiceBus: Send + Sync {
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

        // ─── Connection Status — Polling Interface ────────────────────────────

        /// Returns the current ASB connection status synchronously.
        ///
        /// This is the **polling** interface defined in §5.9. Executes
        /// within the calling thread's context.
        ///
        /// May be called in any connection state, including `Failed`.
        fn connection_status(&self) -> AsbStatus;

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
            listener: Arc<dyn AsbStatusListener>,
        ) -> CalResult<()>;

        /// Unregisters a previously registered ASB status listener.
        ///
        /// After this call returns, `listener.on_status_change()` will no
        /// longer be invoked for future state changes.
        ///
        /// Has no effect if the listener was not registered.
        fn unregister_status_listener(
            &mut self,
            listener: &Arc<dyn AsbStatusListener>,
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

    // ─── CalInstanceConfig ────────────────────────────────────────────────────

    /// Configuration required to obtain a CAL instance.
    ///
    /// Provided to a [`CalInstanceFactory`] function to initialize and
    /// return a fully operational [`AbstractServiceBus`] instance
    /// (CERT CAL-005201).
    ///
    /// The (service_identifier, asb_identifier) pair uniquely identifies a
    /// CAL instance within a process (CERT CAL-005202).
    #[derive(Debug, Clone)]
    pub struct CalInstanceConfig {
        /// Human-readable Service Identifier coordinated between the OMS
        /// Service provider and the CAL provider (§4.10, §5.3).
        pub service_identifier: String,

        /// Identifies the Abstract Service Bus this instance connects to.
        /// Multiple service identifiers may share an ASB, but each
        /// (service_id, asb_id) pair maps to exactly one CAL instance.
        pub asb_identifier: String,

        /// Path, URI, or descriptor for the network configuration resource
        /// that defines valid CAL Topics and network connections (§4.7).
        pub network_config: String,
    }

    /// Factory function type for obtaining a CAL instance.
    ///
    /// Satisfies CERT CAL-005201: provides a mechanism for the CAL Client to
    /// obtain a fully initialized instance of the CAL associated with a
    /// Service Identifier.
    ///
    /// Each CAL Implementation ships a concrete function of this type.
    /// Subsequent calls with the same (service_id, asb_id) return the same
    /// logical instance (CERT CAL-005202).
    ///
    /// Returns `Err(InitializationFailure)` on failure (CERT CAL-005204).
    pub type CalInstanceFactory =
        fn(config: CalInstanceConfig) -> CalResult<Box<dyn AbstractServiceBus>>;
}
