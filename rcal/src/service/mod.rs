//! AbstractService trait and default implementation.
//!
//! Provides a service-level abstraction above the ASB: lifecycle management,
//! identity, and convenience passthroughs for creating typed readers/writers.
//!
//! ## Specification references
//! - OMSC-SPC-001 Rev L §5 (service lifecycle)
//! - OMSC-SPC-008 Rev K §9 (AbstractService)

use std::any::Any;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use slog::{error, info, trace, warn};

use crate::cal::{
    AbstractCal, AbstractCalCreateMessage, AbstractCalExt, AbstractWriter,
    MessageListener, TopicQos,
};
use crate::calconfig::CalConfig;
use crate::uci::base::UUID;
use crate::uci::{CalMessage, CalResult};

// ════════════════════════════════════════════════════════════════════════════
// ServiceLifecycleState
// ════════════════════════════════════════════════════════════════════════════

/// Lifecycle state of an AbstractService instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceLifecycleState {
    /// Service is not active; will not send or receive traffic.
    Inactive,
    /// Service is active and processing messages.
    Active,
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractService trait
// ════════════════════════════════════════════════════════════════════════════

/// High-level service interface sitting above AbstractServiceBus.
///
/// Manages service identity, lifecycle state, and provides passthroughs to
/// the underlying ASB for typed message creation, readers, and writers.
pub trait AbstractService: Send + Sync {
    /// The OMS System Identifier for this service.
    fn system_id(&self) -> &str;

    /// The OMS Service Identifier.
    fn service_id(&self) -> &str;

    /// The OMS Subsystem Identifiers managed by this service.
    fn subsystem_ids(&self) -> Option<&[crate::uci::base::UUID]>;

    /// Current lifecycle state.
    fn lifecycle_state(&self) -> ServiceLifecycleState;

    /// Transitions the service to `Active`. Idempotent — no-op if already active.
    fn activate(&mut self) -> impl Future<Output = CalResult<()>> + Send;

    /// Transitions the service to `Inactive`. Idempotent — no-op if already inactive.
    fn deactivate(&mut self) -> impl Future<Output = CalResult<()>> + Send;

    /// Resets the service to its initial state.
    fn reset(&mut self) -> impl Future<Output = CalResult<()>> + Send;
}

// ════════════════════════════════════════════════════════════════════════════
// AbstractServiceImpl
// ════════════════════════════════════════════════════════════════════════════

/// Concrete [`AbstractService`] implementation, generic over the CAL type `A`.
///
/// `A` is held directly (not via `dyn AbstractCal`) so that the generic
/// [`AbstractCalExt<M>`] methods remain callable at the method level.
#[rcal_macros::monitor]
pub struct AbstractServiceImpl<A> {
    service_id: String,
    system_id: String,
    subsystem_ids: Vec<UUID>,
    state: ServiceLifecycleState,
    asb: A,
    #[allow(dead_code)]
    config: Arc<CalConfig>,
    logger: slog::Logger,
    _readers: std::sync::Mutex<Vec<Box<dyn Any + Send>>>,
}

impl<A: AbstractCal> AbstractServiceImpl<A> {
    /// Constructs a new `AbstractServiceImpl`.
    pub fn new(
        service_id: impl Into<String>,
        system_id: impl Into<String>,
        subsystem_ids: Vec<UUID>,
        asb: A,
        config: Arc<CalConfig>,
        logger: slog::Logger,
    ) -> Self {
        let service_id = service_id.into();
        let system_id = system_id.into();
        trace!(logger, "AbstractServiceImpl::new";
            "service_id" => &service_id,
            "system_id" => &system_id,
        );
        Self {
            service_id,
            system_id,
            subsystem_ids,
            state: ServiceLifecycleState::Inactive,
            asb,
            config,
            logger,
            _readers: std::sync::Mutex::new(Vec::new()),
            __monitor_mutex: std::sync::Mutex::new(()),
        }
    }

    /// Creates a typed message, pre-populated with ASB header defaults.
    pub fn create_message<M: CalMessage>(&self) -> CalResult<M> {
        trace!(self.logger, "AbstractServiceImpl::create_message";
            "service_id" => &self.service_id,
        );
        self.asb.create_message::<M>()
    }

    /// Creates a typed writer for `topic`.
    pub fn create_writer<M>(
        &mut self,
        topic: &str,
        qos: TopicQos,
    ) -> CalResult<Box<dyn AbstractWriter<M>>>
    where
        M: CalMessage,
        A: AbstractCalExt<M>,
    {
        trace!(self.logger, "AbstractServiceImpl::create_writer";
            "service_id" => &self.service_id,
            "topic" => topic,
        );
        let _guard = self.__monitor_mutex.lock().unwrap();
        self.asb.create_writer(topic, qos)
    }

    /// Creates a typed reader for `topic` with a callback closure.
    ///
    /// The reader is kept alive for the lifetime of this service instance.
    pub fn create_reader<M, F>(&mut self, topic: &str, qos: TopicQos, callback: F) -> CalResult<()>
    where
        M: CalMessage + 'static,
        A: AbstractCalExt<M>,
        F: Fn(Arc<M>, &str) + Send + Sync + 'static,
    {
        trace!(self.logger, "AbstractServiceImpl::create_reader";
            "service_id" => &self.service_id,
            "topic" => topic,
        );
        let _guard = self.__monitor_mutex.lock().unwrap();
        let mut reader = self.asb.create_reader(topic, qos).map_err(|e| {
            error!(self.logger, "create_reader failed";
                "service_id" => &self.service_id,
                "topic" => topic,
                "error" => %e,
            );
            e
        })?;
        let listener: Arc<dyn MessageListener<M>> = Arc::new(CallbackListener {
            topic: topic.to_string(),
            callback,
            _p: PhantomData,
        });
        reader.add_listener(listener).map_err(|e| {
            error!(self.logger, "add_listener failed";
                "service_id" => &self.service_id,
                "topic" => topic,
                "error" => %e,
            );
            e
        })?;
        self._readers
            .lock()
            .unwrap()
            .push(Box::new(reader) as Box<dyn Any + Send>);
        Ok(())
    }

    /// Parses `status_delay` from the service config.
    pub fn status_delay(&self) -> Option<Duration> {
        let cfg = self.config.get_service(&self.service_id)?;
        let s = cfg.status_delay.as_deref()?;
        parse_duration(s)
            .map_err(|e| {
                warn!(self.logger, "invalid status_delay, ignored";
                    "service_id" => &self.service_id,
                    "value" => s,
                    "error" => e,
                );
            })
            .ok()
    }

    /// Returns `service_status_data_request_enable` from config.
    pub fn status_data_request_enabled(&self) -> bool {
        self.config
            .get_service(&self.service_id)
            .map(|s| s.service_status_data_request_enable)
            .unwrap_or(false)
    }

    /// Mutable reference to the underlying CAL.
    pub fn asb_mut(&mut self) -> &mut A {
        &mut self.asb
    }
}

impl<A: AbstractCal> AbstractService for AbstractServiceImpl<A> {
    fn system_id(&self) -> &str {
        &self.system_id
    }

    fn service_id(&self) -> &str {
        &self.service_id
    }

    fn subsystem_ids(&self) -> Option<&[UUID]> {
        Some(&self.subsystem_ids)
    }

    fn lifecycle_state(&self) -> ServiceLifecycleState {
        let _guard = self.__monitor_mutex.lock().unwrap();
        self.state
    }

    async fn activate(&mut self) -> CalResult<()> {
        let _guard = self.__monitor_mutex.lock().unwrap();
        if self.state == ServiceLifecycleState::Active {
            warn!(self.logger, "activate called while already active";
                "service_id" => &self.service_id,
            );
            return Ok(());
        }
        self.state = ServiceLifecycleState::Active;
        info!(self.logger, "service activated"; "service_id" => &self.service_id);
        Ok(())
    }

    async fn deactivate(&mut self) -> CalResult<()> {
        let _guard = self.__monitor_mutex.lock().unwrap();
        if self.state == ServiceLifecycleState::Inactive {
            warn!(self.logger, "deactivate called while already inactive";
                "service_id" => &self.service_id,
            );
            return Ok(());
        }
        self.state = ServiceLifecycleState::Inactive;
        info!(self.logger, "service deactivated"; "service_id" => &self.service_id);
        Ok(())
    }

    async fn reset(&mut self) -> CalResult<()> {
        trace!(self.logger, "AbstractService::reset"; "service_id" => &self.service_id);
        Err(crate::uci::CalError::new(
            crate::uci::CalErrorKind::OperationNotPermitted,
            format!("reset() not implemented for service '{}'", self.service_id),
        ))
    }
}

// ════════════════════════════════════════════════════════════════════════════
// CallbackListener
// ════════════════════════════════════════════════════════════════════════════

struct CallbackListener<M, F> {
    topic: String,
    callback: F,
    _p: PhantomData<M>,
}

impl<M, F> MessageListener<M> for CallbackListener<M, F>
where
    M: CalMessage,
    F: Fn(Arc<M>, &str) + Send + Sync,
{
    fn on_message(&self, message: &Arc<M>) {
        (self.callback)(Arc::clone(message), &self.topic);
    }
}

// ════════════════════════════════════════════════════════════════════════════
// service_status_loop! macro
// ════════════════════════════════════════════════════════════════════════════

/// Spawns a tokio task that executes `$body` then sleeps `$interval`, forever.
///
/// All variables referenced in `$body` are captured by move into the spawned
/// task — they must be `Send + 'static`. Returns a `tokio::task::JoinHandle<()>`.
///
/// # Example
/// ```ignore
/// let mut writer = svc.create_writer::<SystemStatus_>("SystemStatus", TopicQos::default())?;
/// let mut msg = svc.create_message::<SystemStatus_>()?;
/// service_status_loop!(Duration::from_secs(1), {
///     rcal::update_message_header!(msg);
///     if let Err(e) = writer.write(&msg) {
///         slog::error!(logger, "write failed"; "error" => %e);
///     }
/// });
/// // `writer` and `msg` are now owned by the spawned task.
/// ```
#[macro_export]
macro_rules! service_status_loop {
    ($interval:expr, $body:block) => {
        ::tokio::spawn(async move {
            loop {
                $body;
                ::tokio::time::sleep($interval).await;
            }
        })
    };
}

// ════════════════════════════════════════════════════════════════════════════
// Duration parser
// ════════════════════════════════════════════════════════════════════════════

/// Parses a human-readable duration string: "1s", "500ms", "2m", etc.
pub fn parse_duration(s: &str) -> Result<Duration, &'static str> {
    if let Some(ms) = s.strip_suffix("ms") {
        ms.parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| "invalid milliseconds value")
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<u64>()
            .map(Duration::from_secs)
            .map_err(|_| "invalid seconds value")
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<u64>()
            .map(|m| Duration::from_secs(m * 60))
            .map_err(|_| "invalid minutes value")
    } else {
        Err("unrecognised duration suffix (expected ms, s, or m)")
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Unit tests
// ════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asb::{AbstractServiceBus, AsbStatus, AsbStatusListener};
    use crate::cal::{AbstractCal, MessageHeaderDefaults};
    use crate::uci::base::UUID;

    struct NullAsb;

    impl AbstractServiceBus for NullAsb {
        fn get_logger(&self) -> &slog::Logger {
            unimplemented!()
        }
        fn service_identifier(&self) -> &str {
            "null"
        }
        fn asb_identifier(&self) -> &str {
            "null"
        }
        fn get_system_uuid(&self) -> UUID {
            unimplemented!()
        }
        fn get_service_uuid(&self) -> Option<UUID> {
            None
        }
        fn get_subsystem_uuid(&self) -> Option<UUID> {
            None
        }
        fn get_component_uuid(&self, _: &str) -> Option<UUID> {
            None
        }
        fn get_capability_uuid(&self, _: &str) -> Option<UUID> {
            None
        }
        fn oms_schema_version(&self) -> &str {
            ""
        }
        fn oms_schema_compiler_version(&self) -> &str {
            ""
        }
        fn get_system_label(&self) -> Option<&str> {
            None
        }
        fn get_asb_connection_version(&self) -> &str {
            ""
        }
        fn get_oms_api_version(&self) -> &str {
            ""
        }
        fn connection_status(&self) -> &AsbStatus {
            unimplemented!()
        }
        fn register_status_listener(
            &mut self,
            _: Arc<dyn AsbStatusListener>,
        ) -> crate::uci::CalResult<()> {
            Ok(())
        }
        fn unregister_status_listener(
            &mut self,
            _: &Arc<dyn AsbStatusListener>,
        ) -> crate::uci::CalResult<()> {
            Ok(())
        }
        fn close(&mut self) -> crate::uci::CalResult<()> {
            Ok(())
        }
    }

    impl AbstractCal for NullAsb {
        fn message_header_defaults(&self) -> MessageHeaderDefaults {
            unimplemented!()
        }
    }

    struct DummyService {
        state: ServiceLifecycleState,
    }

    impl DummyService {
        pub fn new() -> DummyService {
            DummyService {
                state: ServiceLifecycleState::Inactive,
            }
        }
    }

    impl AbstractService for DummyService {
        fn system_id(&self) -> &str {
            "sys'"
        }

        fn service_id(&self) -> &str {
            "svc"
        }

        fn subsystem_ids(&self) -> Option<&[crate::uci::base::UUID]> {
            None
        }

        fn lifecycle_state(&self) -> ServiceLifecycleState {
            self.state
        }

        async fn activate(&mut self) -> CalResult<()> {
            self.state = ServiceLifecycleState::Active;
            Ok(())
        }

        async fn deactivate(&mut self) -> CalResult<()> {
            self.state = ServiceLifecycleState::Inactive;
            Ok(())
        }

        async fn reset(&mut self) -> CalResult<()> {
            self.deactivate().await
        }
    }

    #[tokio::test]
    async fn test_reset_sets_state_inactive() {
        let mut svc = DummyService::new();
        svc.activate().await.unwrap();
        assert_eq!(svc.lifecycle_state(), ServiceLifecycleState::Active);
        svc.reset().await.unwrap();
        assert_eq!(svc.lifecycle_state(), ServiceLifecycleState::Inactive);
    }

    #[tokio::test]
    async fn test_reset_idempotent_when_inactive() {
        let mut svc = DummyService::new();
        assert!(svc.reset().await.is_ok());
        assert_eq!(svc.lifecycle_state(), ServiceLifecycleState::Inactive);
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert!(parse_duration("bad").is_err());
        assert!(parse_duration("1x").is_err());
    }
}
