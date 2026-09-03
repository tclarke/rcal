pub mod config;
pub mod data;
pub mod replay;

use std::sync::Arc;
use std::time::{Duration, Instant};

use slog::{error, info, warn};

use rcal::cal::{AbstractCal, AbstractCalExt, AbstractWriter, TopicQos};
use rcal::calconfig::CalConfig;
use rcal::service::{AbstractService, AbstractServiceImpl, ServiceLifecycleState};
use rcal::uci::base::UUID;
use rcal::uci::types::*;
use rcal::uci::CalResult;
use rcal::update_message_header;
use rcal::xs;

use config::AdsbSimConfig;
use data::{in_geo_filter, AdsbSnapshot, Aircraft};
use replay::wall_send_time;

pub struct AdsbSimService {
    lifecycle: Box<dyn AbstractService>,
    entity_writer: Option<Box<dyn AbstractWriter<Entity_>>>,
    sys_writer: Option<Box<dyn AbstractWriter<SystemStatus_>>>,
    svc_writer: Option<Box<dyn AbstractWriter<ServiceStatus_>>>,
    entity_template: Entity_,
    sys_msg: SystemStatus_,
    svc_msg: ServiceStatus_,
    adsb_config: AdsbSimConfig,
    system_uuid: UUID,
    service_uuid: UUID,
    logger: slog::Logger,
    task_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl AdsbSimService {
    pub fn new<A>(asb: A, cal_config: Arc<CalConfig>, logger: slog::Logger) -> CalResult<Self>
    where
        A: AbstractCal
            + AbstractCalExt<Entity_>
            + AbstractCalExt<SystemStatus_>
            + AbstractCalExt<ServiceStatus_>
            + 'static,
    {
        let adsb_config: AdsbSimConfig = cal_config.get_extension("adsb_sim").unwrap_or_default();

        let system_uuid = cal_config.system.uuid;
        let service_uuid = cal_config
            .get_service("adsb_sim")
            .and_then(|s| s.uuid)
            .unwrap_or_else(UUID::generate_v4);

        let mut svc = AbstractServiceImpl::new(
            "adsb_sim",
            cal_config.system.id.clone(),
            vec![],
            asb,
            Arc::clone(&cal_config),
            logger.clone(),
        );

        let entity_writer = svc.create_writer::<Entity_>("Entity", TopicQos::default())?;
        let sys_writer = svc.create_writer::<SystemStatus_>("SystemStatus", TopicQos::default())?;
        let svc_writer =
            svc.create_writer::<ServiceStatus_>("ServiceStatus", TopicQos::default())?;

        let entity_template = svc.create_message::<Entity_>()?;
        let sys_msg = svc.create_message::<SystemStatus_>()?;
        let svc_msg = svc.create_message::<ServiceStatus_>()?;

        Ok(Self {
            lifecycle: Box::new(svc),
            entity_writer: Some(entity_writer),
            sys_writer: Some(sys_writer),
            svc_writer: Some(svc_writer),
            entity_template,
            sys_msg,
            svc_msg,
            adsb_config,
            system_uuid,
            service_uuid,
            logger,
            task_handles: Vec::new(),
        })
    }
}

impl AbstractService for AdsbSimService {
    fn system_id(&self) -> &str {
        self.lifecycle.system_id()
    }

    fn service_id(&self) -> &str {
        self.lifecycle.service_id()
    }

    fn subsystem_ids(&self) -> &[String] {
        self.lifecycle.subsystem_ids()
    }

    fn lifecycle_state(&self) -> ServiceLifecycleState {
        self.lifecycle.lifecycle_state()
    }

    fn activate(&mut self) -> CalResult<()> {
        self.lifecycle.activate()?;

        let mut sys_writer = self.sys_writer.take().expect("sys_writer already taken");
        let mut sys_msg = self.sys_msg.clone();
        let sys_logger = self.logger.clone();
        let sys_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                update_message_header!(sys_msg);
                if let Err(e) = sys_writer.write(&sys_msg) {
                    error!(sys_logger, "SystemStatus write failed"; "error" => %e);
                }
            }
        });
        self.task_handles.push(sys_handle);

        let mut svc_writer = self.svc_writer.take().expect("svc_writer already taken");
        let mut svc_msg = self.svc_msg.clone();
        let svc_logger = self.logger.clone();
        let svc_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                update_message_header!(svc_msg);
                if let Err(e) = svc_writer.write(&svc_msg) {
                    error!(svc_logger, "ServiceStatus write failed"; "error" => %e);
                }
            }
        });
        self.task_handles.push(svc_handle);

        let entity_writer = self
            .entity_writer
            .take()
            .expect("entity_writer already taken");
        let entity_template = self.entity_template.clone();
        let adsb_config = self.adsb_config.clone();
        let system_uuid = self.system_uuid;
        let service_uuid = self.service_uuid;
        let replay_logger = self.logger.clone();
        let replay_handle = tokio::spawn(async move {
            run_replay(
                entity_writer,
                entity_template,
                adsb_config,
                system_uuid,
                service_uuid,
                replay_logger,
            )
            .await;
        });
        self.task_handles.push(replay_handle);

        info!(self.logger, "AdsbSimService activated");
        Ok(())
    }

    fn deactivate(&mut self) -> CalResult<()> {
        for handle in self.task_handles.drain(..) {
            handle.abort();
        }
        self.lifecycle.deactivate()?;
        info!(self.logger, "AdsbSimService deactivated");
        Ok(())
    }

    fn reset(&mut self) -> CalResult<()> {
        self.deactivate()?;
        self.lifecycle.reset()
    }
}

async fn run_replay(
    mut entity_writer: Box<dyn AbstractWriter<Entity_>>,
    entity_template: Entity_,
    config: AdsbSimConfig,
    system_uuid: UUID,
    service_uuid: UUID,
    logger: slog::Logger,
) {
    let json_file = match &config.json_file {
        Some(f) => f.clone(),
        None => {
            warn!(logger, "adsb_sim: no json_file configured, replay idle");
            return;
        }
    };

    let json = match std::fs::read_to_string(&json_file) {
        Ok(s) => s,
        Err(e) => {
            error!(logger, "adsb_sim: failed to read json_file"; "path" => &json_file, "error" => %e);
            return;
        }
    };

    let snapshot = match AdsbSnapshot::from_json(&json) {
        Ok(s) => s,
        Err(e) => {
            error!(logger, "adsb_sim: failed to parse json_file"; "error" => %e);
            return;
        }
    };

    let data_t0 = snapshot.now;
    let wall_t0 = Instant::now();

    let snap_ts = chrono::DateTime::from_timestamp(snapshot.now as i64, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Utc);

    if let Some(start) = config.datetime_start {
        if snap_ts < start {
            info!(logger, "adsb_sim: snapshot before datetime_start, skipping");
            return;
        }
    }
    if let Some(end) = config.datetime_end {
        if snap_ts > end {
            info!(logger, "adsb_sim: snapshot after datetime_end, done");
            return;
        }
    }

    let send_at = wall_send_time(snapshot.now, data_t0, wall_t0, config.speed_multiplier);
    tokio::time::sleep_until(send_at.into()).await;

    for aircraft in &snapshot.aircraft {
        let (Some(lat), Some(lon)) = (aircraft.lat, aircraft.lon) else {
            continue;
        };
        if !in_geo_filter(
            lat,
            lon,
            config.geo_center_lat,
            config.geo_center_lon,
            config.geo_radius_km,
        ) {
            continue;
        }

        let entity_uuid = UUID::generate_v3(&service_uuid, aircraft.hex.as_bytes());
        let mut msg = entity_template.clone();
        populate_entity_msg(&mut msg, aircraft, entity_uuid, system_uuid, snap_ts);
        update_message_header!(msg);
        if let Err(e) = entity_writer.write(&msg) {
            error!(logger, "adsb_sim: Entity write failed"; "hex" => &aircraft.hex, "error" => %e);
        }
    }
}

fn populate_entity_msg(
    msg: &mut Entity_,
    aircraft: &Aircraft,
    entity_uuid: UUID,
    system_uuid: UUID,
    snap_ts: chrono::DateTime<chrono::Utc>,
) {
    msg.object_state_set(ObjectStateEnum::New);

    let xs_ts = xs::DateTime::from(snap_ts);

    {
        let mdt = msg.message_data_mut();
        *mdt.entity_id_mut().uuid_mut() = entity_uuid;
        *mdt.source_mut().system_id_mut().uuid_mut() = system_uuid;
        *mdt.entity_status_mut() = EntityStatusEnum::Confirmed;
        *mdt.creation_timestamp_mut().date_time_mut() = xs_ts;
        *mdt.identity_mut().identity_timestamp_mut() = xs_ts;

        if let Some(callsign) = &aircraft.callsign {
            let trimmed = callsign.trim().to_string();
            if !trimmed.is_empty() {
                mdt.identity_mut().self_reported_identity_set(trimmed);
            }
        }
    }

    if let (Some(lat), Some(lon)) = (aircraft.lat, aircraft.lon) {
        let mut fixed = FixedPositionType_::default();
        {
            let pt = fixed.fixed_point_mut();
            *pt.latitude_mut() = lat;
            *pt.longitude_mut() = lon;
            if let Some(alt_ft) = aircraft.alt_baro_feet() {
                pt.altitude_set(alt_ft * 0.3048);
            }
        }

        let mut new_kinem = KinematicsType_::default();
        new_kinem.position_mut().fixed_position_type_set(fixed);
        new_kinem.kinematics_time_stamp_set(xs_ts);
        msg.message_data_mut().kinematics_set(new_kinem);
    }
}
