//! Integration tests: RADIO/DISH via omq-tokio across inproc, tcp, and udp.
//!
//! Each test is independent and exercises fanout from one RADIO to multiple
//! DISH sockets. No broker is required — RADIO fans out natively.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use omq_tokio::endpoint::Host;
use omq_tokio::{Endpoint, Message, MonitorEvent, Options, Socket, SocketType};
use tokio::time::sleep;

const SEND_SETTLE: Duration = Duration::from_millis(30);

// ── helpers ──────────────────────────────────────────────────────────────────

fn radio_msg(group: &'static str, body: &'static str) -> Message {
    Message::multipart([group, body])
}

async fn recv_parts(dish: &Socket) -> (Vec<u8>, Vec<u8>) {
    let msg = dish.recv().await.expect("recv failed");
    let group = msg.part_bytes(0).expect("no group frame").to_vec();
    let body = msg.part_bytes(1).expect("no body frame").to_vec();
    (group, body)
}

// ── inproc ────────────────────────────────────────────────────────────────────

/// RADIO binds inproc, 3 DISH sockets connect and join, all receive the message.
#[tokio::test]
async fn test_inproc_radio_dish_fanout() {
    let ep: Endpoint = "inproc://test-fanout-inproc".parse().unwrap();

    let radio = Socket::new(SocketType::Radio, Options::default());
    radio.bind(ep.clone()).await.expect("radio bind failed");

    let mut dishes: Vec<Socket> = Vec::new();
    for _ in 0..3 {
        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.connect(ep.clone()).await.expect("dish connect failed");
        dish.join("news").await.expect("dish join failed");
        dishes.push(dish);
    }

    radio.send(radio_msg("news", "breaking")).await.expect("send failed");
    radio.send(radio_msg("other", "dropped")).await.expect("send failed");

    for (i, dish) in dishes.iter().enumerate() {
        let (group, body) = recv_parts(dish).await;
        assert_eq!(group, b"news", "dish {i}: wrong group");
        assert_eq!(body, b"breaking", "dish {i}: wrong body");
    }

    radio.close().await.unwrap();
    for dish in dishes { dish.close().await.unwrap(); }
}

// ── tcp ───────────────────────────────────────────────────────────────────────

/// RADIO binds tcp://127.0.0.1:0, 3 DISH sockets connect and join, all receive.
#[tokio::test]
async fn test_tcp_radio_dish_fanout() {
    let radio = Socket::new(SocketType::Radio, Options::default());
    let bound = radio
        .bind("tcp://127.0.0.1:0".parse::<Endpoint>().unwrap())
        .await
        .expect("radio bind failed");

    let port = match bound {
        Endpoint::Tcp { port, .. } => port,
        _ => panic!("expected TCP endpoint from bind"),
    };

    let mut dishes: Vec<Socket> = Vec::new();
    for _ in 0..3 {
        let dish = Socket::new(SocketType::Dish, Options::default());
        dish.join("status").await.expect("dish join failed");
        dish.connect(format!("tcp://127.0.0.1:{port}").parse::<Endpoint>().unwrap())
            .await
            .expect("dish connect failed");
        dishes.push(dish);
    }

    // Allow ZMTP handshakes to complete.
    sleep(Duration::from_millis(80)).await;

    radio.send(radio_msg("status", "ok")).await.expect("send failed");
    sleep(SEND_SETTLE).await;

    for (i, dish) in dishes.iter().enumerate() {
        let (group, body) = recv_parts(dish).await;
        assert_eq!(group, b"status", "dish {i}: wrong group");
        assert_eq!(body, b"ok", "dish {i}: wrong body");
    }

    radio.close().await.unwrap();
    for dish in dishes { dish.close().await.unwrap(); }
}

// ── udp ───────────────────────────────────────────────────────────────────────

/// UDP polarity: DISH binds, RADIO connects. Single DISH receives from RADIO.
/// (UDP unicast; no multicast. Each DISH would need its own port for fanout.)
#[tokio::test]
async fn test_udp_radio_dish() {
    let dish = Socket::new(SocketType::Dish, Options::default());
    let mut mon = dish.monitor();
    dish.bind(Endpoint::Udp {
        group: None,
        host: Host::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        port: 0,
    })
    .await
    .expect("dish bind failed");

    // Read the OS-assigned port from the monitor stream.
    let port = loop {
        match mon.recv().await.expect("monitor recv failed") {
            MonitorEvent::Listening {
                endpoint: Endpoint::Udp { port, .. },
            } => break port,
            _ => continue,
        }
    };

    dish.join("sensor").await.expect("dish join failed");

    let radio = Socket::new(SocketType::Radio, Options::default());
    radio
        .connect(Endpoint::Udp {
            group: None,
            host: Host::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port,
        })
        .await
        .expect("radio connect failed");

    sleep(Duration::from_millis(20)).await;

    radio.send(radio_msg("sensor", "37.2")).await.expect("send failed");
    radio.send(radio_msg("other", "ignored")).await.expect("send failed");
    sleep(SEND_SETTLE).await;

    let (group, body) = recv_parts(&dish).await;
    assert_eq!(group, b"sensor");
    assert_eq!(body, b"37.2");

    radio.close().await.unwrap();
    dish.close().await.unwrap();
}
