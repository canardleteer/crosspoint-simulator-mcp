//! gRPC client tests for inbound Session registration and queues.

use std::net::SocketAddr;
use std::time::Duration;

use connectrpc::client::{ClientConfig, HttpClient};
use connectrpc::{Protocol, Server};
use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    Heartbeat, Register, ServerToSim, ShutdownRequest, SimToServer,
};
use csm_pb_bindings::rpc::crosspoint::sim::control::v1alpha1::SimulatorControlServiceClient;
use csm_proxy::{InstanceMap, QUEUE_CAPACITY, SessionService, TrySendError};

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
