//! MCP stdio (in-process duplex) and Streamable HTTP protocol tests.

use std::time::Duration;

use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    InputAck, Register, SimToServer, SnapshotFrame,
};
use csm_proxy::{
    CAPABILITIES_URI, INSTRUCTIONS, InstanceMap, McpServer, TOOL_NAMES, serve_mcp_http_listener,
};
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, ContentBlock, ReadResourceRequestParams,
};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

fn register(id: &str) -> Register {
    Register {
        instance_id: id.into(),
        board_id: "x4".into(),
        ..Default::default()
    }
}

fn tool_text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(ContentBlock::as_text)
        .map(|text| text.text.clone())
        .expect("tool text")
}

fn tool_json(result: &CallToolResult) -> Value {
    serde_json::from_str(&tool_text(result)).expect("tool json")
}

fn args(value: Value) -> rmcp::model::JsonObject {
    value.as_object().cloned().expect("object args")
}

async fn connect_duplex(
    instances: InstanceMap,
) -> rmcp::service::RunningService<rmcp::RoleClient, ClientInfo> {
    let (client_to_server, server_from_client) = duplex(64 * 1024);
    let (server_to_client, client_from_server) = duplex(64 * 1024);
    tokio::spawn(async move {
        let running = McpServer::new(instances)
            .serve((server_from_client, server_to_client))
            .await
            .expect("serve mcp duplex");
        let _ = running.waiting().await;
    });
    ClientInfo::default()
        .serve((client_from_server, client_to_server))
        .await
        .expect("client handshake")
}

#[tokio::test]
async fn stdio_lists_tools_and_instances() {
    let instances = InstanceMap::new();
    let (tx, _rx) = mpsc::channel(4);
    instances.insert(register("sim-a"), 4, tx);
    let client = connect_duplex(instances).await;
    let info = client.peer_info().expect("peer info");
    assert_eq!(info.instructions.as_deref(), Some(INSTRUCTIONS));
    let tools = client.list_tools(None).await.expect("list tools");
    let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(names.contains(&"list_instances"));
    assert!(names.contains(&"observe"));
    assert!(names.contains(&"start_instance"));
    for name in TOOL_NAMES {
        assert!(names.contains(name), "{name}");
    }
    let listed = client
        .call_tool(CallToolRequestParams::new("list_instances"))
        .await
        .expect("list_instances");
    assert_ne!(listed.is_error, Some(true));
    let body = tool_json(&listed);
    assert_eq!(body["instances"][0]["instanceId"], "sim-a");
    client.cancel().await.expect("cancel client");
}

#[tokio::test]
async fn stdio_inject_and_snapshot_wait_for_session_replies() {
    let instances = InstanceMap::new();
    let (tx, mut rx) = mpsc::channel(4);
    instances.insert(register("sim-a"), 4, tx);
    let map = instances.clone();
    tokio::spawn(async move {
        let inject = rx.recv().await.expect("inject");
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: inject.corr,
                payload: Some(
                    InputAck {
                        accepted: true,
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        let snap = rx.recv().await.expect("snapshot");
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: snap.corr,
                payload: Some(
                    SnapshotFrame {
                        pixels: vec![0x01],
                        mime_type: "image/png".into(),
                        width: 1,
                        height: 1,
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
    });
    let client = connect_duplex(instances).await;
    let injected = client
        .call_tool(
            CallToolRequestParams::new("inject_touch").with_arguments(args(
                json!({ "instance_id": "sim-a", "kind": 3, "x": 1, "y": 2 }),
            )),
        )
        .await
        .expect("inject_touch");
    assert_ne!(injected.is_error, Some(true));
    assert_eq!(tool_json(&injected)["accepted"], true);

    let snap = client
        .call_tool(
            CallToolRequestParams::new("request_snapshot")
                .with_arguments(args(json!({ "instance_id": "sim-a" }))),
        )
        .await
        .expect("request_snapshot");
    assert_ne!(snap.is_error, Some(true));
    assert!(snap.content.iter().any(|block| block.as_image().is_some()));
    client.cancel().await.expect("cancel client");
}

#[tokio::test]
async fn streamable_http_calls_a_tool() {
    let instances = InstanceMap::new();
    let (tx, _rx) = mpsc::channel(4);
    instances.insert(register("sim-http"), 4, tx);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mcp http");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        serve_mcp_http_listener(listener, instances)
            .await
            .expect("serve mcp http");
    });
    let mut client = None;
    for _ in 0..50 {
        let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
        if let Ok(connected) = ClientInfo::default().serve(transport).await {
            client = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let client = client.expect("http mcp client");
    let result = client
        .call_tool(
            CallToolRequestParams::new("get_instance")
                .with_arguments(args(json!({ "instance_id": "sim-http" }))),
        )
        .await
        .expect("get_instance");
    assert_ne!(result.is_error, Some(true));
    assert_eq!(tool_json(&result)["instanceId"], "sim-http");
    let resources = client.list_resources(None).await.expect("list resources");
    let uris: Vec<_> = resources
        .resources
        .iter()
        .map(|resource| resource.uri.as_str())
        .collect();
    assert!(uris.contains(&CAPABILITIES_URI));
    assert!(uris.contains(&"csm://instances"));
    assert!(uris.contains(&"csm://instances/sim-http"));
    let read = client
        .read_resource(ReadResourceRequestParams::new("csm://instances"))
        .await
        .expect("read instances");
    match read.contents.first() {
        Some(rmcp::model::ResourceContents::TextResourceContents { text, .. }) => {
            let body: Value = serde_json::from_str(text).expect("resource json");
            assert_eq!(body["instances"][0]["instanceId"], "sim-http");
        }
        other => panic!("unexpected resource: {other:?}"),
    }
    let caps = client
        .read_resource(ReadResourceRequestParams::new(CAPABILITIES_URI))
        .await
        .expect("read capabilities");
    match caps.contents.first() {
        Some(rmcp::model::ResourceContents::TextResourceContents { text, .. }) => {
            let body: Value = serde_json::from_str(text).expect("capabilities json");
            assert_eq!(body["spawn"]["configured"], false);
            assert_eq!(body["spawn"]["tool"], "start_instance");
            assert_eq!(body["spawn"]["sampleBookDefault"], true);
            assert_eq!(
                body["spawn"]["sampleBook"]["filename"],
                "CrossPoint-Reader.epub"
            );
        }
        other => panic!("unexpected capabilities: {other:?}"),
    }
    client.cancel().await.expect("cancel client");
}

#[tokio::test]
async fn streamable_http_catalog_enqueues_and_reads_resources() {
    let instances = InstanceMap::new();
    let (tx, _rx) = mpsc::channel(32);
    instances.insert(register("sim-http"), 32, tx);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mcp http");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        serve_mcp_http_listener(listener, instances)
            .await
            .expect("serve mcp http");
    });
    let mut client = None;
    for _ in 0..50 {
        let transport = StreamableHttpClientTransport::from_uri(format!("http://{addr}/mcp"));
        if let Ok(connected) = ClientInfo::default().serve(transport).await {
            client = Some(connected);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let client = client.expect("http mcp client");
    let id = json!({ "instance_id": "sim-http" });

    for (name, value) in [
        (
            "inject_touch",
            json!({ "instance_id": "sim-http", "kind": 3, "x": 1, "y": 2, "wait": false }),
        ),
        (
            "inject_key",
            json!({ "instance_id": "sim-http", "name": "ENTER", "hold_ms": 10, "wait": false }),
        ),
        (
            "inject_home",
            json!({ "instance_id": "sim-http", "hold_ms": 10, "wait": false }),
        ),
        (
            "inject_swipe",
            json!({
                "instance_id": "sim-http",
                "start_x": 1, "start_y": 2, "end_x": 3, "end_y": 4, "duration_ms": 20,
                "wait": false
            }),
        ),
        (
            "set_inject_enabled",
            json!({ "instance_id": "sim-http", "enabled": true }),
        ),
        (
            "request_snapshot",
            json!({ "instance_id": "sim-http", "region": false, "wait": false }),
        ),
        (
            "set_session_view",
            json!({ "instance_id": "sim-http", "paths": ["log"] }),
        ),
        ("shutdown_instance", id.clone()),
        ("observe", id.clone()),
        ("start_instance", json!({ "instance_id": "spawn-me" })),
        ("get_instance", json!({ "instance_id": "" })),
        ("get_instance", json!({ "instance_id": "missing" })),
    ] {
        let result = client
            .call_tool(CallToolRequestParams::new(name).with_arguments(args(value)))
            .await
            .expect(name);
        assert!(!result.content.is_empty(), "{name}");
    }

    let templates = client
        .list_resource_templates(None)
        .await
        .expect("templates");
    assert!(!templates.resource_templates.is_empty());
    let one = client
        .read_resource(ReadResourceRequestParams::new("csm://instances/sim-http"))
        .await
        .expect("read one");
    assert!(!one.contents.is_empty());
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("csm://instances/missing"))
            .await
            .is_err()
    );
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("csm://nope"))
            .await
            .is_err()
    );
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("csm://instances/"))
            .await
            .is_err()
    );
    client.cancel().await.expect("cancel client");
}
