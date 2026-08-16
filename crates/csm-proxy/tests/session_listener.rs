//! gRPC client tests for inbound Session registration and queues.

use std::net::SocketAddr;
use std::time::Duration;

use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{Protocol, Server};
use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    Goodbye, Heartbeat, InputAck, LogLine, Register, ServerToSim, ShutdownRequest, SimToServer,
    SnapshotError, SnapshotFrame, server_to_sim,
};
use csm_pb_bindings::rpc::crosspoint::sim::control::v1alpha1::SimulatorControlServiceClient;
use csm_proxy::{InstanceMap, McpServer, QUEUE_CAPACITY, SessionService, TrySendError};

fn register_msg(instance_id: &str) -> SimToServer {
    SimToServer {
        seq: 1,
        payload: Some(
            Register {
                instance_id: instance_id.into(),
                board_id: "x4".into(),
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
}

fn heartbeat_msg(generation: u64) -> SimToServer {
    SimToServer {
        seq: 2,
        payload: Some(
            Heartbeat {
                framebuffer_generation: generation,
                inject_enabled: true,
                headless: false,
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
}

fn goodbye_msg() -> SimToServer {
    SimToServer {
        seq: 3,
        payload: Some(
            Goodbye {
                reason: "done".into(),
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
}

fn log_msg(seq: u64) -> SimToServer {
    SimToServer {
        seq,
        payload: Some(
            LogLine {
                text: "line".into(),
                ..Default::default()
            }
            .into(),
        ),
        ..Default::default()
    }
}

fn outbound_msg(corr: u64) -> ServerToSim {
    ServerToSim {
        corr,
        payload: Some(ShutdownRequest::default().into()),
        ..Default::default()
    }
}

async fn start_listener() -> (SocketAddr, InstanceMap) {
    let instances = InstanceMap::new();
    let router = SessionService::new(instances.clone()).router();
    let bound = Server::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = bound.local_addr().expect("local addr");
    tokio::spawn(async move {
        bound.serve(router).await.expect("serve Session");
    });
    (addr, instances)
}

fn grpc_client(addr: SocketAddr) -> SimulatorControlServiceClient<HttpClient> {
    let uri = format!("http://{addr}").parse().expect("listen uri");
    let config = ClientConfig::new(uri).with_protocol(Protocol::Grpc);
    SimulatorControlServiceClient::new(HttpClient::plaintext_http2_only(), config)
}

async fn wait_until(mut pred: impl FnMut() -> bool) {
    for _ in 0..100 {
        if pred() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for Session state");
}

#[tokio::test]
async fn register_appears_in_the_map() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-a"))
        .await
        .expect("send Register");

    wait_until(|| instances.get("sim-a").is_some()).await;
    let snap = instances.get("sim-a").expect("registered");
    assert_eq!(snap.register.instance_id, "sim-a");
    assert_eq!(snap.register.board_id, "x4");
    session.close_send();
}

#[tokio::test]
async fn heartbeat_updates_last_seen() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-hb"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-hb").is_some()).await;

    session
        .send(heartbeat_msg(7))
        .await
        .expect("send Heartbeat");
    wait_until(|| {
        instances
            .get("sim-hb")
            .and_then(|s| s.last_heartbeat)
            .is_some_and(|hb| hb.framebuffer_generation == 7 && hb.inject_enabled)
    })
    .await;
    session.close_send();
}

#[tokio::test]
async fn disconnect_removes_the_instance() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-gone"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-gone").is_some()).await;

    session.close_send();
    drop(session);
    wait_until(|| instances.get("sim-gone").is_none()).await;
}

#[tokio::test]
async fn reregister_same_id_replaces_the_handle() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);

    let mut first = client.session().await.expect("open first Session");
    first
        .send(register_msg("sim-re"))
        .await
        .expect("first Register");
    wait_until(|| instances.get("sim-re").is_some()).await;
    let first_token = instances.get("sim-re").expect("first").token;

    let mut second = client.session().await.expect("open second Session");
    second
        .send(register_msg("sim-re"))
        .await
        .expect("second Register");
    wait_until(|| {
        instances
            .get("sim-re")
            .is_some_and(|s| s.token != first_token)
    })
    .await;

    assert_eq!(instances.list(), vec!["sim-re".to_string()]);
    first.close_send();
    second.close_send();
}

#[tokio::test]
async fn two_ids_coexist() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);

    let mut a = client.session().await.expect("open a");
    a.send(register_msg("one")).await.expect("register one");
    let mut b = client.session().await.expect("open b");
    b.send(register_msg("two")).await.expect("register two");

    wait_until(|| instances.get("one").is_some() && instances.get("two").is_some()).await;
    let mut ids = instances.list();
    ids.sort();
    assert_eq!(ids, vec!["one".to_string(), "two".to_string()]);
    a.close_send();
    b.close_send();
}

#[tokio::test]
async fn outbound_try_send_fails_when_full() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-full"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-full").is_some()).await;

    for corr in 0..QUEUE_CAPACITY as u64 {
        instances
            .try_send("sim-full", outbound_msg(corr))
            .expect("enqueue while under capacity");
    }
    assert_eq!(
        instances.try_send("sim-full", outbound_msg(99)),
        Err(TrySendError::QueueFull)
    );
    session.close_send();
}

#[tokio::test]
async fn empty_stream_is_rejected() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session.close_send();
    drop(session);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(instances.list().is_empty());
}

#[tokio::test]
async fn first_message_must_be_register() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    let _ = session.send(heartbeat_msg(1)).await;
    session.close_send();
    drop(session);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(instances.list().is_empty());
}

#[tokio::test]
async fn empty_instance_id_is_rejected() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    let _ = session.send(register_msg("")).await;
    session.close_send();
    drop(session);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(instances.list().is_empty());
}

#[tokio::test]
async fn long_instance_id_is_rejected() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    let _ = session.send(register_msg(&"x".repeat(65))).await;
    session.close_send();
    drop(session);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(instances.list().is_empty());
}

#[tokio::test]
async fn goodbye_removes_the_instance() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-bye"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-bye").is_some()).await;
    session.send(goodbye_msg()).await.expect("send Goodbye");
    wait_until(|| instances.get("sim-bye").is_none()).await;
    session.close_send();
}

#[tokio::test]
async fn later_envelopes_land_on_the_inbound_queue() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-in"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-in").is_some()).await;
    session.send(log_msg(4)).await.expect("send LogLine");
    wait_until(|| instances.try_recv_inbound("sim-in").is_some()).await;
    session.close_send();
}

#[tokio::test]
async fn serve_accepts_a_register() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("pick port");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);

    let instances = InstanceMap::new();
    let server = tokio::spawn(csm_proxy::serve(addr, instances.clone()));
    let client = grpc_client(addr);
    let mut session = None;
    for _ in 0..50 {
        if let Ok(opened) = client.session().await {
            session = Some(opened);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut session = session.expect("serve accepted Session");
    session
        .send(register_msg("via-serve"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("via-serve").is_some()).await;
    session.close_send();
    server.abort();
}

#[tokio::test]
async fn fake_client_acks_an_inject() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-ack"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-ack").is_some()).await;

    let mcp = McpServer::new(instances);
    let waiter =
        tokio::spawn(async move { mcp.inject_touch_json(Some("sim-ack"), 3, 8, 9, true).await });
    let outbound = session
        .message()
        .await
        .expect("recv inject")
        .expect("inject present")
        .to_owned_message();
    assert!(outbound.ack_requested);
    match outbound.payload {
        Some(server_to_sim::Payload::InjectTouch(touch)) => {
            assert_eq!(touch.x, 8);
            assert_eq!(touch.y, 9);
        }
        other => panic!("unexpected payload: {other:?}"),
    }
    session
        .send(SimToServer {
            corr: outbound.corr,
            payload: Some(
                InputAck {
                    accepted: true,
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        })
        .await
        .expect("send InputAck");
    let result = waiter.await.unwrap().unwrap();
    assert_eq!(result["accepted"], true);
    session.close_send();
}

#[tokio::test]
async fn fake_client_rejects_an_inject() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-nack"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-nack").is_some()).await;

    let mcp = McpServer::new(instances);
    let waiter = tokio::spawn(async move {
        mcp.inject_key_json(Some("sim-nack"), "ENTER".into(), 0, true)
            .await
    });
    let outbound = session
        .message()
        .await
        .expect("recv inject")
        .expect("inject present")
        .to_owned_message();
    session
        .send(SimToServer {
            corr: outbound.corr,
            payload: Some(
                InputAck {
                    accepted: false,
                    reason: "queue_full".into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        })
        .await
        .expect("send InputAck");
    assert_eq!(waiter.await.unwrap().unwrap_err(), "queue_full");
    session.close_send();
}

#[tokio::test]
async fn fake_client_replies_with_a_snapshot_frame() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-snap"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-snap").is_some()).await;

    let mcp = McpServer::new(instances);
    let waiter = tokio::spawn(async move {
        mcp.request_snapshot_result(Some("sim-snap"), false, 0, 0, 0, 0, 0, true)
            .await
    });
    let outbound = session
        .message()
        .await
        .expect("recv snapshot request")
        .expect("snapshot request present")
        .to_owned_message();
    assert!(!outbound.ack_requested);
    assert!(matches!(
        outbound.payload,
        Some(server_to_sim::Payload::SnapshotRequest(_))
    ));
    session
        .send(SimToServer {
            corr: outbound.corr,
            payload: Some(
                SnapshotFrame {
                    pixels: vec![0x89, 0x50, 0x4e, 0x47],
                    mime_type: "image/png".into(),
                    width: 4,
                    height: 2,
                    generation: 11,
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        })
        .await
        .expect("send SnapshotFrame");
    let result = waiter.await.unwrap().unwrap();
    assert_ne!(result.is_error, Some(true));
    assert!(
        result
            .content
            .iter()
            .any(|block| block.as_image().is_some())
    );
    session.close_send();
}

#[tokio::test]
async fn fake_client_replies_with_a_snapshot_error() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-snap-err"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-snap-err").is_some()).await;

    let mcp = McpServer::new(instances);
    let waiter = tokio::spawn(async move {
        mcp.request_snapshot_result(Some("sim-snap-err"), false, 0, 0, 0, 0, 0, true)
            .await
    });
    let outbound = session
        .message()
        .await
        .expect("recv snapshot request")
        .expect("snapshot request present")
        .to_owned_message();
    session
        .send(SimToServer {
            corr: outbound.corr,
            payload: Some(
                SnapshotError {
                    message: "no panel".into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        })
        .await
        .expect("send SnapshotError");
    let result = waiter.await.unwrap().unwrap();
    assert_eq!(result.is_error, Some(true));
    session.close_send();
}

#[tokio::test]
async fn inject_wait_times_out_when_the_client_stays_silent() {
    let (addr, instances) = start_listener().await;
    let client = grpc_client(addr);
    let mut session = client.session().await.expect("open Session");
    session
        .send(register_msg("sim-silent"))
        .await
        .expect("send Register");
    wait_until(|| instances.get("sim-silent").is_some()).await;

    let mcp = McpServer::new(instances);
    let err = mcp
        .inject_home_json(Some("sim-silent"), 0, true)
        .await
        .unwrap_err();
    assert_eq!(err, "timed out waiting for session reply");
    session.close_send();
}
