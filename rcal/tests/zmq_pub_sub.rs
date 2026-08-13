/// Integration test: zmq_pub_sub
///
/// Exercises ZeroMQ PUB / SUB sockets through the `zeromq` crate.
///
/// Scenario
/// --------
/// 1. Spin up 4 async "clients" inside a single process.
/// 2. One client (id = 0) acts as the *publisher* and also listens.
///    Clients 1-3 are pure subscribers.
/// 3. Round 1 – no-topic broadcast:
///    Client 0 publishes a plain message.  All four clients (including the
///    publisher, which also holds a SubSocket) should receive it and print it.
/// 4. Round 2 – topic-based publish:
///    Client 0 publishes a message with a topic prefix.  Every SubSocket has
///    been subscribed to that topic, so all four clients receive it.
/// 5. Sockets are closed and the test exits cleanly.
///
/// Architecture note
/// -----------------
/// ZeroMQ PUB→SUB is asynchronous and one-directional, so "the publisher also
/// receives its own message" is achieved by having client 0 hold *both* a
/// PubSocket (for writing) and a SubSocket (for reading).  Both share the same
/// bind/connect address.

use std::time::Duration;

use tokio::time::sleep;
use zeromq::{PubSocket, Socket, SocketRecv, SocketSend, SubSocket, ZmqMessage};

const ADDR: &str = "tcp://127.0.0.1:15557";

/// Small delay helpers so the async sockets have time to complete their
/// internal connect / subscribe handshake before messages are sent.
const CONNECT_WAIT: Duration = Duration::from_millis(300);
const SEND_WAIT: Duration = Duration::from_millis(200);

// ── helpers ──────────────────────────────────────────────────────────────────

/// Subscribe `sock` and wait briefly so the subscription arrives at the broker.
async fn subscribe_all(sock: &mut SubSocket) {
    sock.subscribe("").await.expect("subscribe '' failed");
    sleep(CONNECT_WAIT).await;
}

/// Subscribe to a specific topic and wait.
async fn subscribe_topic(sock: &mut SubSocket, topic: &str) {
    sock.subscribe(topic).await.expect("subscribe topic failed");
    sleep(CONNECT_WAIT).await;
}

/// Receive one ZmqMessage and decode all frames as UTF-8, joined by " | ".
async fn recv_string(sock: &mut SubSocket) -> String {
    let msg: ZmqMessage = sock.recv().await.expect("recv failed");
    msg.iter()
        .map(|frame| String::from_utf8_lossy(frame).into_owned())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Build a single-frame ZmqMessage from a string slice.
fn zmq_msg(s: &str) -> ZmqMessage {
    ZmqMessage::from(s.as_bytes().to_vec())
}

// ── the actual test ───────────────────────────────────────────────────────────

#[tokio::test]
async fn zmq_pub_sub() {
    // ── 1. Bind the publisher ────────────────────────────────────────────────
    let mut pub_sock = PubSocket::new();
    pub_sock.bind(ADDR).await.expect("PUB bind failed");

    // ── 2. Connect four SubSockets (one per "client") ────────────────────────
    let mut sub_socks: Vec<SubSocket> = Vec::with_capacity(4);
    for _ in 0..4 {
        let mut s = SubSocket::new();
        s.connect(ADDR).await.expect("SUB connect failed");
        sub_socks.push(s);
    }

    // Give the transport layer a moment to finish the TCP handshake.
    sleep(CONNECT_WAIT).await;

    // ── 3. Round 1: no-topic broadcast ───────────────────────────────────────
    //
    // Subscribe every client to *all* messages (empty prefix = wildcard in ZMQ).
    for s in &mut sub_socks {
        subscribe_all(s).await;
    }

    let plain_msg = "Hello from client 0 (no topic)";
    println!("\n[publisher] sending plain message: {plain_msg:?}");
    pub_sock
        .send(zmq_msg(plain_msg))
        .await
        .expect("PUB send (plain) failed");

    // Give the message time to propagate.
    sleep(SEND_WAIT).await;

    // Each sub socket should have one message waiting.
    for (id, sock) in sub_socks.iter_mut().enumerate() {
        let received = recv_string(sock).await;
        println!("[client {id}] received plain message: {received:?}");
        assert_eq!(
            received, plain_msg,
            "client {id}: plain message content mismatch"
        );
    }

    // ── 4. Round 2: topic-based publish ─────────────────────────────────────
    //
    // In ZeroMQ the topic is simply a prefix of the message bytes.  We send a
    // two-frame message: frame 0 = topic, frame 1 = body.  Subscribers that
    // match the topic prefix (or subscribe to "") will receive both frames.

    let topic = "sensor.temperature";
    let body = "42.7 °C";

    // Subscribe every client to this specific topic in addition to "".
    for s in &mut sub_socks {
        subscribe_topic(s, topic).await;
    }

    println!("\n[publisher] sending topic={topic:?} body={body:?}");

    // Build a two-frame message so the topic and body are distinct.
    let mut topic_msg = ZmqMessage::from(topic.as_bytes().to_vec());
    topic_msg
        .push_back(body.as_bytes().to_vec().into());
    pub_sock
        .send(topic_msg)
        .await
        .expect("PUB send (topic) failed");

    sleep(SEND_WAIT).await;

    for (id, sock) in sub_socks.iter_mut().enumerate() {
        let received = recv_string(sock).await;
        println!("[client {id}] received topic message: {received:?}");
        // The decoded string should contain both the topic and the body.
        assert!(
            received.contains(topic),
            "client {id}: topic missing from message"
        );
        assert!(
            received.contains(body),
            "client {id}: body missing from message"
        );
    }

    // ── 5. Shutdown ──────────────────────────────────────────────────────────
    println!("\n[test] shutting down sockets");

    // SubSockets implement Drop correctly; just let them fall out of scope.
    drop(sub_socks);
    drop(pub_sock);

    println!("[test] done");
}
