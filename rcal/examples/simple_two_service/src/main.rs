//! Two-service OMS example demonstrating AbstractService lifecycle,
//! periodic status broadcasting, and ServiceStatusDataRequest/response.
//!
//! # Running
//!
//! Open two terminals and run each command in one:
//!
//! ```text
//! cargo run -- TestService1 3
//! cargo run -- TestService2 3
//! ```
//!
//! TestService1 binds on port 2000; TestService2 on port 2001. Each peers
//! to the other so all topics flow bidirectionally.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use slog::{error, info, o, warn};

use rcal::asb::TopicQos;
use rcal::asb::zmq::ZmqAsb;
use rcal::service::{AbstractService, AbstractServiceImpl};
use rcal::uci::types::*;
use rcal::update_message_header;

#[rcal_macros::rcal_main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    let service_name = match args.get(1) {
        Some(n) => n.clone(),
        None => {
            eprintln!("Usage: simple_two_service <TestService1|TestService2> [request_count]");
            std::process::exit(1);
        }
    };

    let request_count: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);

    let config = Arc::new(rcal_config);

    // Each service uses a distinct port so both can bind their own RADIO socket.
    // The peer URI points to the other service's RADIO.
    // Delays are staggered per service to make concurrent output easier to visualize.
    let (transport_id, peer_uri, request_delay) = match service_name.as_str() {
        "TestService1" => ("service1", "tcp://127.0.0.1:2001", Duration::from_secs(3)),
        "TestService2" => ("service2", "tcp://127.0.0.1:2000", Duration::from_secs(5)),
        other => {
            error!(root_logger, "unknown service name; use TestService1 or TestService2";
                "name" => other);
            std::process::exit(1);
        }
    };

    let tconfig = match config.get_transport(transport_id) {
        Some(t) => t.clone(),
        None => {
            error!(root_logger, "transport not in config"; "id" => transport_id);
            std::process::exit(1);
        }
    };

    let mut asb = match ZmqAsb::new(
        &service_name,
        transport_id,
        root_logger.clone(),
        Arc::clone(&config),
        &tconfig,
    )
    .await
    {
        Ok(a) => a,
        Err(e) => {
            error!(root_logger, "ZmqAsb init failed"; "error" => %e);
            std::process::exit(1);
        }
    };

    // Connect the DISH side to the other service's RADIO.
    asb.add_receive_peer(peer_uri);

    let mut svc = AbstractServiceImpl::new(
        service_name.clone(),
        config.system.id.clone(),
        vec![],
        asb,
        Arc::clone(&config),
        root_logger.clone(),
    );

    if let Err(e) = svc.activate() {
        error!(root_logger, "activate failed"; "error" => %e);
        std::process::exit(1);
    }

    info!(root_logger, "service started";
        "service" => &service_name,
        "transport" => transport_id,
        "peer" => peer_uri,
    );

    // ── Periodic SystemStatus ──────────────────────────────────────────────────

    let mut sys_writer =
        match svc.create_writer::<SystemStatus_>("SystemStatus", TopicQos::default()) {
            Ok(w) => w,
            Err(e) => {
                error!(root_logger, "create SystemStatus writer failed"; "error" => %e);
                std::process::exit(1);
            }
        };
    let mut sys_msg = svc
        .create_message::<SystemStatus_>()
        .expect("create SystemStatus message");

    let sys_logger = root_logger.new(o!("topic" => "SystemStatus"));
    let sys_svc_name = service_name.clone();
    rcal::service_status_loop!(Duration::from_secs(1), {
        update_message_header!(sys_msg);
        info!(sys_logger, "sending SystemStatus"; "service" => &sys_svc_name);
        if let Err(e) = sys_writer.write(&sys_msg) {
            error!(sys_logger, "SystemStatus write failed"; "service" => &sys_svc_name, "error" => %e);
        }
    });

    // ── Periodic ServiceStatus ─────────────────────────────────────────────────

    let mut svc_status_writer =
        match svc.create_writer::<ServiceStatus_>("ServiceStatus", TopicQos::default()) {
            Ok(w) => w,
            Err(e) => {
                error!(root_logger, "create ServiceStatus writer failed"; "error" => %e);
                std::process::exit(1);
            }
        };
    let mut svc_status_msg = svc
        .create_message::<ServiceStatus_>()
        .expect("create ServiceStatus message");

    let svc_status_logger = root_logger.new(o!("topic" => "ServiceStatus"));
    let svc_status_svc_name = service_name.clone();
    rcal::service_status_loop!(Duration::from_secs(1), {
        update_message_header!(svc_status_msg);
        info!(svc_status_logger, "sending ServiceStatus"; "service" => &svc_status_svc_name);
        if let Err(e) = svc_status_writer.write(&svc_status_msg) {
            error!(svc_status_logger, "ServiceStatus write failed";
                "service" => &svc_status_svc_name,
                "error" => %e,
            );
        }
    });

    // ── ServiceStatusDataRequest handler (TestService1 only) ───────────────────

    if svc.status_data_request_enabled() {
        let resp_writer = match svc.create_writer::<ServiceStatusDataRequestStatus_>(
            "ServiceStatusDataRequestStatus",
            TopicQos::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                error!(root_logger, "create ServiceStatusDataRequestStatus writer failed"; "error" => %e);
                std::process::exit(1);
            }
        };
        let resp_msg_template = svc
            .create_message::<ServiceStatusDataRequestStatus_>()
            .expect("create ServiceStatusDataRequestStatus message");

        let resp_writer = Arc::new(Mutex::new(resp_writer));
        let resp_msg = Arc::new(Mutex::new(resp_msg_template));
        let resp_logger = root_logger.new(o!("topic" => "ServiceStatusDataRequest"));
        let resp_svc_name = service_name.clone();

        if let Err(e) = svc.create_reader::<ServiceStatusDataRequest_, _>(
            "ServiceStatusDataRequest",
            TopicQos::default(),
            move |_request, _topic| {
                let mut writer = resp_writer.lock().unwrap();
                let mut msg = resp_msg.lock().unwrap();
                update_message_header!(*msg);
                info!(resp_logger, "ServiceStatusDataRequest received — sending response";
                    "service" => &resp_svc_name);
                if let Err(e) = writer.write(&*msg) {
                    error!(resp_logger, "ServiceStatusDataRequestStatus write failed";
                        "service" => &resp_svc_name,
                        "error" => %e,
                    );
                }
            },
        ) {
            error!(root_logger, "create_reader for ServiceStatusDataRequest failed"; "error" => %e);
        }
    }

    // ── ServiceStatusDataRequestStatus listener ────────────────────────────────

    let resp_status_logger = root_logger.new(o!("topic" => "ServiceStatusDataRequestStatus"));
    let resp_status_svc_name = service_name.clone();
    if let Err(e) = svc.create_reader::<ServiceStatusDataRequestStatus_, _>(
        "ServiceStatusDataRequestStatus",
        TopicQos::default(),
        move |_msg, _topic| {
            info!(resp_status_logger, "received ServiceStatusDataRequestStatus";
                "service" => &resp_status_svc_name);
        },
    ) {
        warn!(root_logger, "create_reader for ServiceStatusDataRequestStatus failed"; "error" => %e);
    }

    // ── Send ServiceStatusDataRequests ─────────────────────────────────────────

    let mut req_writer = match svc
        .create_writer::<ServiceStatusDataRequest_>("ServiceStatusDataRequest", TopicQos::default())
    {
        Ok(w) => w,
        Err(e) => {
            error!(root_logger, "create ServiceStatusDataRequest writer failed"; "error" => %e);
            std::process::exit(1);
        }
    };
    let mut req_msg = svc
        .create_message::<ServiceStatusDataRequest_>()
        .expect("create ServiceStatusDataRequest message");

    info!(root_logger, "sending {} ServiceStatusDataRequests (delay: {:?})",
        request_count, request_delay;
        "service" => &service_name,
    );

    for i in 0..request_count {
        tokio::time::sleep(request_delay).await;
        update_message_header!(req_msg);
        info!(root_logger, "sending ServiceStatusDataRequest";
            "service" => &service_name,
            "i" => i,
        );
        if let Err(e) = req_writer.write(&req_msg) {
            error!(root_logger, "ServiceStatusDataRequest write failed";
                "service" => &service_name,
                "error" => %e,
            );
        }
    }

    info!(root_logger, "all requests sent — running until interrupted (Ctrl+C)";
        "service" => &service_name);

    // Keep status loops alive.
    tokio::signal::ctrl_c().await.ok();

    if let Err(e) = svc.deactivate() {
        error!(root_logger, "deactivate failed"; "error" => %e);
    }
    info!(root_logger, "service stopped"; "service" => &service_name);
}
