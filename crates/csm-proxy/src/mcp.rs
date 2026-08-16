//! MCP peer: tools and resources over [`InstanceMap`].

use std::sync::Arc;

use axum::Router;
use buffa_types::google::protobuf::FieldMask;
use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    InjectHome, InjectKey, InjectSwipe, InjectTouch, ServerToSim, SetInjectEnabled, SetSessionView,
    ShutdownRequest, SnapshotRequest,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, schemars, tool, tool_handler,
    tool_router, transport::stdio,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::instances::{InstanceMap, InstanceSnapshot, ResolveError, TrySendError};

const INSTANCES_URI: &str = "csm://instances";
const INSTANCE_URI_PREFIX: &str = "csm://instances/";

/// MCP handler shared by stdio and Streamable HTTP.
#[derive(Clone)]
pub struct McpServer {
    instances: InstanceMap,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// Serve tools and resources against `instances`.
    pub fn new(instances: InstanceMap) -> Self {
        Self {
            instances,
            tool_router: Self::tool_router(),
        }
    }

    fn resolve(&self, instance_id: Option<&str>) -> Result<InstanceSnapshot, String> {
        self.instances
            .resolve_or_default(instance_id)
            .map_err(|err| match err {
                ResolveError::EmptyId => {
                    "instance_id is required (1-64 bytes) or set --default-instance".into()
                }
                ResolveError::UnknownInstance => "unknown instance".into(),
            })
    }

    fn enqueue(
        &self,
        instance_id: Option<&str>,
        payload: impl Into<
            csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::server_to_sim::Payload,
        >,
    ) -> Result<Value, String> {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id.clone();
        let corr = self.instances.next_corr();
        let msg = ServerToSim {
            corr,
            ack_requested: false,
            payload: Some(payload.into()),
            ..Default::default()
        };
        self.instances.try_send(&id, msg).map_err(|err| match err {
            TrySendError::UnknownInstance => "unknown instance".to_string(),
            TrySendError::QueueFull => "outbound queue is full".to_string(),
        })?;
        Ok(json!({
            "queued": true,
            "instanceId": id,
            "corr": corr,
        }))
    }

    fn snapshot_json(snap: &InstanceSnapshot) -> Value {
        json!({
            "instanceId": snap.register.instance_id,
            "register": snap.register,
            "lastHeartbeat": snap.last_heartbeat,
        })
    }

    /// Connected instances as JSON.
    pub fn list_instances_json(&self) -> Value {
        let mut instances: Vec<Value> = self
            .instances
            .snapshots()
            .iter()
            .map(Self::snapshot_json)
            .collect();
        instances.sort_by(|a, b| {
            a["instanceId"]
                .as_str()
                .unwrap_or("")
                .cmp(b["instanceId"].as_str().unwrap_or(""))
        });
        json!({ "instances": instances })
    }

    /// One instance as JSON.
    pub fn get_instance_json(&self, instance_id: Option<&str>) -> Result<Value, String> {
        Ok(Self::snapshot_json(&self.resolve(instance_id)?))
    }

    /// Drain inbound envelopes for an instance.
    pub fn observe_json(&self, instance_id: Option<&str>) -> Result<Value, String> {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id;
        let mut events = Vec::new();
        while let Some(msg) = self.instances.try_recv_inbound(&id) {
            events.push(serde_json::to_value(&msg).unwrap_or(Value::Null));
        }
        Ok(json!({
            "instanceId": id,
            "events": events,
        }))
    }

    /// Enqueue a touch inject.
    pub fn inject_touch_json(
        &self,
        instance_id: Option<&str>,
        kind: u32,
        x: u32,
        y: u32,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            InjectTouch {
                kind,
                x,
                y,
                ..Default::default()
            },
        )
    }

    /// Enqueue a key inject.
    pub fn inject_key_json(
        &self,
        instance_id: Option<&str>,
        name: String,
        hold_ms: u32,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            InjectKey {
                name,
                hold_ms,
                ..Default::default()
            },
        )
    }

    /// Enqueue a Home inject.
    pub fn inject_home_json(
        &self,
        instance_id: Option<&str>,
        hold_ms: u32,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            InjectHome {
                hold_ms,
                ..Default::default()
            },
        )
    }

    /// Enqueue a swipe inject.
    pub fn inject_swipe_json(
        &self,
        instance_id: Option<&str>,
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            InjectSwipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
                ..Default::default()
            },
        )
    }

    /// Enqueue inject-enabled.
    pub fn set_inject_enabled_json(
        &self,
        instance_id: Option<&str>,
        enabled: bool,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            SetInjectEnabled {
                enabled,
                ..Default::default()
            },
        )
    }

    /// Enqueue a snapshot request; does not wait for a frame.
    pub fn request_snapshot_json(
        &self,
        instance_id: Option<&str>,
        region: bool,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        format: u32,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            SnapshotRequest {
                region,
                x,
                y,
                width,
                height,
                format,
                ..Default::default()
            },
        )
    }

    /// Enqueue a session view mask.
    pub fn set_session_view_json(
        &self,
        instance_id: Option<&str>,
        paths: Vec<String>,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            SetSessionView {
                read_mask: FieldMask {
                    paths,
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
        )
    }

    /// Enqueue a shutdown request.
    pub fn shutdown_instance_json(&self, instance_id: Option<&str>) -> Result<Value, String> {
        self.enqueue(instance_id, ShutdownRequest::default())
    }

    fn tool_result(result: Result<Value, String>) -> Result<CallToolResult, McpError> {
        match result {
            Ok(value) => Ok(CallToolResult::success(vec![ContentBlock::text(
                value.to_string(),
            )])),
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    fn instance_uri(id: &str) -> String {
        format!("{INSTANCE_URI_PREFIX}{id}")
    }

    fn read_instances_resource(&self) -> String {
        self.list_instances_json().to_string()
    }

    fn read_instance_resource(&self, id: &str) -> Result<String, McpError> {
        self.get_instance_json(Some(id))
            .map(|v| v.to_string())
            .map_err(|_| {
                McpError::resource_not_found(
                    "resource_not_found",
                    Some(json!({ "uri": Self::instance_uri(id) })),
                )
            })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InstanceParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InjectTouchParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// 0=down, 1=move, 2=up, 3=tap.
    kind: u32,
    /// Touch x in panel pixels.
    x: u32,
    /// Touch y in panel pixels.
    y: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InjectKeyParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// Key name: BACK, ENTER, LEFT, RIGHT, UP, DOWN, POWER, SLEEP, or QUIT.
    name: String,
    /// Hold duration in milliseconds; 0 means the default 80.
    #[serde(default)]
    hold_ms: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InjectHomeParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// Hold duration in milliseconds.
    #[serde(default)]
    hold_ms: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InjectSwipeParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// Start x in panel pixels.
    start_x: u32,
    /// Start y in panel pixels.
    start_y: u32,
    /// End x in panel pixels.
    end_x: u32,
    /// End y in panel pixels.
    end_y: u32,
    /// Duration of the swipe in milliseconds.
    #[serde(default)]
    duration_ms: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetInjectEnabledParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// When false, remote injects are not applied.
    enabled: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SnapshotParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// When true, crop using x, y, width, and height.
    #[serde(default)]
    region: bool,
    /// Crop origin x, in panel pixels.
    #[serde(default)]
    x: u32,
    /// Crop origin y, in panel pixels.
    #[serde(default)]
    y: u32,
    /// Crop width, in panel pixels.
    #[serde(default)]
    width: u32,
    /// Crop height, in panel pixels.
    #[serde(default)]
    height: u32,
    /// Encoded format; 0 is PNG with a device-appropriate palette.
    #[serde(default)]
    format: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SessionViewParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// SimToServer payload names (register, heartbeat, snapshot, log, input_observed, …).
    #[serde(default)]
    paths: Vec<String>,
}

#[tool_router]
impl McpServer {
    #[tool(description = "List connected simulator instances")]
    fn list_instances(&self) -> Result<CallToolResult, McpError> {
        Self::tool_result(Ok(self.list_instances_json()))
    }

    #[tool(description = "Get one connected simulator instance")]
    fn get_instance(
        &self,
        Parameters(params): Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.get_instance_json(params.instance_id.as_deref()))
    }

    #[tool(description = "Inject a touch edge or tap on the named instance")]
    fn inject_touch(
        &self,
        Parameters(params): Parameters<InjectTouchParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.inject_touch_json(
            params.instance_id.as_deref(),
            params.kind,
            params.x,
            params.y,
        ))
    }

    #[tool(description = "Inject a named device key on the named instance")]
    fn inject_key(
        &self,
        Parameters(params): Parameters<InjectKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.inject_key_json(
            params.instance_id.as_deref(),
            params.name,
            params.hold_ms,
        ))
    }

    #[tool(description = "Inject the Home key on the named instance")]
    fn inject_home(
        &self,
        Parameters(params): Parameters<InjectHomeParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.inject_home_json(params.instance_id.as_deref(), params.hold_ms))
    }

    #[tool(description = "Inject a swipe on the named instance")]
    fn inject_swipe(
        &self,
        Parameters(params): Parameters<InjectSwipeParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.inject_swipe_json(
            params.instance_id.as_deref(),
            params.start_x,
            params.start_y,
            params.end_x,
            params.end_y,
            params.duration_ms,
        ))
    }

    #[tool(description = "Enable or disable remote inject on the named instance")]
    fn set_inject_enabled(
        &self,
        Parameters(params): Parameters<SetInjectEnabledParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.set_inject_enabled_json(params.instance_id.as_deref(), params.enabled),
        )
    }

    #[tool(description = "Queue a panel snapshot request; does not wait for the frame")]
    fn request_snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.request_snapshot_json(
            params.instance_id.as_deref(),
            params.region,
            params.x,
            params.y,
            params.width,
            params.height,
            params.format,
        ))
    }

    #[tool(description = "Set which SimToServer payloads the session should emit")]
    fn set_session_view(
        &self,
        Parameters(params): Parameters<SessionViewParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.set_session_view_json(params.instance_id.as_deref(), params.paths))
    }

    #[tool(description = "Ask the named simulator instance to shut down")]
    fn shutdown_instance(
        &self,
        Parameters(params): Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.shutdown_instance_json(params.instance_id.as_deref()))
    }

    #[tool(description = "Drain inbound session events, including human and remote InputObserved")]
    fn observe(
        &self,
        Parameters(params): Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.observe_json(params.instance_id.as_deref()))
    }
}

#[tool_handler(
    name = "crosspoint-simulator-mcp-proxy",
    version = "0.1.0",
    instructions = "Control and observe connected eBook firmware simulator instances. Tools that target a session require instance_id unless --default-instance is set."
)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(
            "Control and observe connected eBook firmware simulator instances. Tools that target a session require instance_id unless --default-instance is set.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut resources = vec![Resource::new(INSTANCES_URI, "instances")];
        let mut ids = self.instances.list();
        ids.sort();
        for id in ids {
            resources.push(Resource::new(Self::instance_uri(&id), id));
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("csm://instances/{id}", "instance"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri == INSTANCES_URI {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                self.read_instances_resource(),
                INSTANCES_URI,
            )])
            .into());
        }
        if let Some(id) = request.uri.strip_prefix(INSTANCE_URI_PREFIX)
            && !id.is_empty()
        {
            let text = self.read_instance_resource(id)?;
            return Ok(
                ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into(),
            );
        }
        Err(McpError::resource_not_found(
            "resource_not_found",
            Some(json!({ "uri": request.uri })),
        ))
    }
}

/// Serve MCP on stdio. Writes only JSON-RPC to stdout.
pub async fn serve_mcp_stdio(
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let running = McpServer::new(instances).serve(stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// Streamable HTTP router mounted at `/mcp`.
pub fn mcp_http_router(instances: InstanceMap) -> Router {
    let config = StreamableHttpServerConfig::default().with_json_response(true);
    let service = StreamableHttpService::new(
        move || Ok(McpServer::new(instances.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new().nest_service("/mcp", service)
}

/// Serve MCP over Streamable HTTP on `addr`.
pub async fn serve_mcp_http(
    addr: std::net::SocketAddr,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    serve_mcp_http_listener(listener, instances).await
}

/// Serve MCP over Streamable HTTP on an already-bound listener.
pub async fn serve_mcp_http_listener(
    listener: TcpListener,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    axum::serve(listener, mcp_http_router(instances)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
        InputObserved, InputSource, KeyEdge, Register, SimToServer, server_to_sim,
    };
    use tokio::sync::mpsc;

    fn register(id: &str) -> Register {
        Register {
            instance_id: id.into(),
            board_id: "x4".into(),
            ..Default::default()
        }
    }

    fn insert(
        map: &InstanceMap,
        id: &str,
    ) -> (mpsc::Receiver<ServerToSim>, crate::instances::InboundSink) {
        let (tx, rx) = mpsc::channel(8);
        let (_token, sink) = map.insert(register(id), 8, tx);
        (rx, sink)
    }

    #[test]
    fn list_and_get_require_a_real_id() {
        let map = InstanceMap::new();
        let mcp = McpServer::new(map.clone());
        assert_eq!(mcp.list_instances_json()["instances"], json!([]));
        assert!(mcp.get_instance_json(None).is_err());
        assert!(mcp.get_instance_json(Some("")).is_err());
        insert(&map, "sim-a");
        assert!(mcp.get_instance_json(None).is_err());
        assert_eq!(
            mcp.get_instance_json(Some("sim-a")).unwrap()["instanceId"],
            "sim-a"
        );
        map.set_default_instance(Some("sim-a".into()));
        assert_eq!(mcp.get_instance_json(None).unwrap()["instanceId"], "sim-a");
        assert!(mcp.get_instance_json(Some("missing")).is_err());
    }

    #[test]
    fn inject_is_readable_on_the_session_queue() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map);
        let queued = mcp.inject_touch_json(Some("sim-a"), 3, 10, 20).unwrap();
        assert_eq!(queued["queued"], true);
        let msg = rx.try_recv().unwrap();
        assert!(!msg.ack_requested);
        assert_eq!(msg.corr, queued["corr"].as_u64().unwrap());
        match msg.payload {
            Some(server_to_sim::Payload::InjectTouch(touch)) => {
                assert_eq!(touch.kind, 3);
                assert_eq!(touch.x, 10);
                assert_eq!(touch.y, 20);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn enqueue_reports_unknown_and_queue_full() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        map.insert(register("sim-a"), 4, tx);
        let mcp = McpServer::new(map);
        assert!(mcp.shutdown_instance_json(Some("nope")).is_err());
        mcp.shutdown_instance_json(Some("sim-a")).unwrap();
        assert_eq!(
            mcp.inject_key_json(Some("sim-a"), "ENTER".into(), 0)
                .unwrap_err(),
            "outbound queue is full"
        );
    }

    #[test]
    fn observe_drains_human_and_remote_edges() {
        let map = InstanceMap::new();
        let (_rx, sink) = insert(&map, "sim-a");
        sink.push(SimToServer {
            seq: 2,
            payload: Some(
                InputObserved {
                    source: InputSource::Human.into(),
                    event: Some(
                        KeyEdge {
                            name: "ENTER".into(),
                            down: true,
                            ..Default::default()
                        }
                        .into(),
                    ),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        sink.push(SimToServer {
            seq: 3,
            payload: Some(
                InputObserved {
                    source: InputSource::Remote.into(),
                    event: Some(
                        KeyEdge {
                            name: "BACK".into(),
                            down: false,
                            ..Default::default()
                        }
                        .into(),
                    ),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        let mcp = McpServer::new(map);
        let observed = mcp.observe_json(Some("sim-a")).unwrap();
        assert_eq!(observed["events"].as_array().unwrap().len(), 2);
        assert_eq!(
            observed["events"][0]["inputObserved"]["source"],
            "INPUT_SOURCE_HUMAN"
        );
        assert_eq!(
            observed["events"][1]["inputObserved"]["source"],
            "INPUT_SOURCE_REMOTE"
        );
        assert!(
            mcp.observe_json(Some("sim-a")).unwrap()["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn remaining_enqueues_and_snapshot_do_not_wait() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map);
        mcp.inject_home_json(Some("sim-a"), 40).unwrap();
        mcp.inject_swipe_json(Some("sim-a"), 1, 2, 3, 4, 50)
            .unwrap();
        mcp.set_inject_enabled_json(Some("sim-a"), false).unwrap();
        mcp.request_snapshot_json(Some("sim-a"), true, 0, 0, 8, 8, 0)
            .unwrap();
        mcp.set_session_view_json(Some("sim-a"), vec!["log".into(), "input_observed".into()])
            .unwrap();
        let kinds: Vec<_> = (0..5)
            .map(|_| match rx.try_recv().unwrap().payload {
                Some(server_to_sim::Payload::InjectHome(_)) => "home",
                Some(server_to_sim::Payload::InjectSwipe(_)) => "swipe",
                Some(server_to_sim::Payload::SetInjectEnabled(_)) => "inject",
                Some(server_to_sim::Payload::SnapshotRequest(_)) => "snap",
                Some(server_to_sim::Payload::SetSessionView(_)) => "view",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["home", "swipe", "inject", "snap", "view"]);
    }
}
