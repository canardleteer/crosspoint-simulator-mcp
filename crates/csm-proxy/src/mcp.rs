//! MCP peer: tools and resources over [`InstanceMap`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use buffa_types::google::protobuf::FieldMask;
use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    InjectHome, InjectKey, InjectSwipe, InjectTouch, ServerToSim, SetInjectEnabled, SetSessionView,
    ShutdownRequest, SnapshotRequest, sim_to_server,
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

use crate::instances::{
    InstanceMap, InstanceSnapshot, REPLY_TIMEOUT, ResolveError, TrySendError, WaitError,
};
use crate::spawn::{SpawnConfig, SpawnError, SpawnSupervisor};

const INSTANCES_URI: &str = "csm://instances";
const INSTANCE_URI_PREFIX: &str = "csm://instances/";
/// Machine-readable MCP surface for clients that read resources.
pub const CAPABILITIES_URI: &str = "csm://capabilities";

/// Initialize instructions. Clients that only read this text can operate.
pub const INSTRUCTIONS: &str = "Control and observe eBook firmware simulator instances over Session. \
A session appears when a simulator dials this process, or when start_instance launches the \
operator-configured --simulator binary. This server does not build firmware or accept a \
client-supplied binary path. Board and compile-time firmware options stay in the binary; \
Register reports them after connect. Tools that target a session require instance_id \
(1-64 bytes) unless --default-instance is set. Do not omit an id when only one simulator \
is connected. start_instance always requires an explicit instance_id and defaults to \
headless. sample_book (default true) seeds fs_/books/CrossPoint-Reader.epub from the \
committed README fixture; pass false for an empty library. auto_sleep (default false, \
or --auto-sleep / CSM_AUTO_SLEEP) seeds fs_/.crosspoint/settings.json with \
sleepTimeoutMinutes 31 (never); pass true to keep firmware's 10-minute idle sleep. \
Use list_instances and \
get_instance to see connected peers. inject_touch, \
inject_key, inject_home, and inject_swipe wait for InputAck unless wait is false. \
request_snapshot waits for a PNG SnapshotFrame. observe drains inbound events including \
human and remote InputObserved. Pass until_log and/or until_generation_gt to wait; \
wait_ms overrides the process default (--observe-wait-ms / CSM_OBSERVE_WAIT_MS, 8000). \
InputAck means the inject was queued, not that the panel \
painted; after inject, observe for ACT/ERS/GFX logs or a framebufferGeneration bump. \
Do not sleep as synchronization. Tap (inject_touch) when Register.capTouch is true; \
default X4 has no touch and no Home. shutdown_instance sends ShutdownRequest and reaps a child \
this server started. Read csm://capabilities for the machine-readable surface. \
csm://instances lists connections; csm://instances/{id} is one snapshot.";

/// Tool names advertised in `csm://capabilities` and `tools/list`.
pub const TOOL_NAMES: &[&str] = &[
    "list_instances",
    "get_instance",
    "start_instance",
    "inject_touch",
    "inject_key",
    "inject_home",
    "inject_swipe",
    "set_inject_enabled",
    "request_snapshot",
    "set_session_view",
    "shutdown_instance",
    "observe",
];

/// MCP handler shared by stdio and Streamable HTTP.
#[derive(Clone)]
pub struct McpServer {
    instances: InstanceMap,
    spawn: Arc<SpawnSupervisor>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// Serve tools and resources against `instances` with spawn disabled.
    pub fn new(instances: InstanceMap) -> Self {
        Self::with_spawn(instances, SpawnConfig::default())
    }

    /// Serve tools and resources; `start_instance` uses `spawn` when configured.
    pub fn with_spawn(instances: InstanceMap, spawn: SpawnConfig) -> Self {
        Self {
            instances,
            spawn: Arc::new(SpawnSupervisor::new(spawn)),
            tool_router: Self::tool_router(),
        }
    }

    /// Reap children started by `start_instance`.
    pub async fn reap_spawned(&self) {
        self.spawn.reap_all().await;
    }

    /// Machine-readable surface. `spawn_configured` is whether `--simulator` is set.
    pub fn capabilities_json(spawn_configured: bool) -> Value {
        json!({
            "tools": TOOL_NAMES,
            "resources": [CAPABILITIES_URI, INSTANCES_URI, "csm://instances/{id}"],
            "instanceId": {
                "required": true,
                "minBytes": 1,
                "maxBytes": 64,
                "omitWhenOne": false,
                "startInstanceRequiresExplicitId": true,
            },
            "keys": ["BACK", "ENTER", "LEFT", "RIGHT", "UP", "DOWN", "POWER", "SLEEP", "QUIT"],
            "snapshot": { "mimeType": "image/png", "format": 0 },
            "spawn": {
                "configured": spawn_configured,
                "tool": "start_instance",
                "headlessDefault": true,
                "sampleBookDefault": true,
                "sampleBook": {
                    "param": "sample_book",
                    "filename": "CrossPoint-Reader.epub",
                    "sdPath": "/books/CrossPoint-Reader.epub",
                    "source": "https://github.com/canardleteer/crosspoint-reader/blob/develop/README.md",
                },
                "autoSleepDefault": false,
                "autoSleep": {
                    "param": "auto_sleep",
                    "neverMinutes": crate::spawn::NEVER_SLEEP_TIMEOUT_MINUTES,
                    "firmwareDefaultMinutes": 10,
                    "settingsPath": crate::spawn::SETTINGS_RELATIVE,
                },
                "clientPassesBinary": false,
                "firmwareBuild": false,
            },
            "observe": {
                "waitMsParam": "wait_ms",
                "waitMsDefault": crate::spawn::DEFAULT_OBSERVE_WAIT_MS,
                "untilLogParam": "until_log",
                "untilGenerationGtParam": "until_generation_gt",
            },
            "firmware": {
                "compileTime": true,
                "see": "Register",
            },
        })
    }

    /// `csm://capabilities` using this process's spawn and observe defaults.
    pub fn capabilities_document(&self) -> Value {
        let mut caps = Self::capabilities_json(self.spawn.configured());
        caps["spawn"]["autoSleepDefault"] = json!(self.spawn.auto_sleep_default());
        caps["observe"]["waitMsDefault"] = json!(self.spawn.observe_wait_ms());
        caps
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
        ack_requested: bool,
        payload: impl Into<
            csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::server_to_sim::Payload,
        >,
    ) -> Result<Value, String> {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id.clone();
        let corr = self.instances.next_corr();
        let msg = ServerToSim {
            corr,
            ack_requested,
            payload: Some(payload.into()),
            ..Default::default()
        };
        self.instances.try_send(&id, msg).map_err(|err| {
            let text = match err {
                TrySendError::UnknownInstance => "unknown instance".to_string(),
                TrySendError::QueueFull => "outbound queue is full".to_string(),
            };
            tracing::warn!(instance_id = %id, error = %text, "enqueue failed");
            text
        })?;
        Ok(json!({
            "queued": true,
            "instanceId": id,
            "corr": corr,
        }))
    }

    async fn enqueue_wait(
        &self,
        instance_id: Option<&str>,
        ack_requested: bool,
        payload: impl Into<
            csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::server_to_sim::Payload,
        >,
    ) -> Result<
        (
            String,
            u64,
            csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::SimToServer,
        ),
        String,
    > {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id.clone();
        let corr = self.instances.next_corr();
        let msg = ServerToSim {
            corr,
            ack_requested,
            payload: Some(payload.into()),
            ..Default::default()
        };
        let reply = self
            .instances
            .send_and_wait(&id, msg, REPLY_TIMEOUT)
            .await
            .map_err(|err| match err {
                WaitError::UnknownInstance => "unknown instance".to_string(),
                WaitError::QueueFull => "outbound queue is full".to_string(),
                WaitError::Timeout => "timed out waiting for session reply".to_string(),
                WaitError::Disconnected => "instance disconnected".to_string(),
            })?;
        Ok((id, corr, reply))
    }

    async fn inject(
        &self,
        instance_id: Option<&str>,
        wait: bool,
        payload: impl Into<
            csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::server_to_sim::Payload,
        >,
    ) -> Result<Value, String> {
        if !wait {
            return self.enqueue(instance_id, false, payload);
        }
        let (id, corr, reply) = self.enqueue_wait(instance_id, true, payload).await?;
        match reply.payload {
            Some(sim_to_server::Payload::InputAck(ack)) => {
                if ack.accepted {
                    Ok(json!({
                        "accepted": true,
                        "instanceId": id,
                        "corr": corr,
                    }))
                } else {
                    let reason = if ack.reason.is_empty() {
                        "inject rejected".to_string()
                    } else {
                        ack.reason.clone()
                    };
                    Err(reason)
                }
            }
            _ => Err("unexpected session reply".into()),
        }
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

    /// Drain inbound envelopes for an instance, honoring the session view mask.
    pub fn observe_json(&self, instance_id: Option<&str>) -> Result<Value, String> {
        let (id, events) = self.drain_observe(instance_id)?;
        tracing::debug!(instance_id = %id, events = events.len(), "observe drain");
        Ok(json!({
            "instanceId": id,
            "events": events,
        }))
    }

    /// Drain, optionally waiting for a log substring and/or a generation bump.
    ///
    /// Succeeds when any specified until-condition matches. `wait_ms` `0` or
    /// omitted with no until-condition is a one-shot drain. Omitted `wait_ms`
    /// with an until-condition uses the process default.
    pub async fn observe_wait_json(
        &self,
        instance_id: Option<&str>,
        wait_ms: Option<u32>,
        until_log: Option<&str>,
        until_generation_gt: Option<u64>,
    ) -> Result<Value, String> {
        let until_log = until_log.filter(|text| !text.is_empty());
        let waiting = until_log.is_some() || until_generation_gt.is_some();
        let timeout_ms = match wait_ms {
            Some(ms) => ms,
            None if waiting => self.spawn.observe_wait_ms(),
            None => 0,
        };
        let (id, mut events) = self.drain_observe(instance_id)?;
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            let generation = self.instances.get(&id).and_then(|snap| {
                snap.last_heartbeat
                    .as_ref()
                    .map(|hb| hb.framebuffer_generation)
            });
            let matched =
                observe_until_matched(until_log, until_generation_gt, &events, generation);
            let timed_out = Instant::now() >= deadline;
            if (waiting && (matched || timed_out)) || (!waiting && timed_out) {
                tracing::debug!(
                    instance_id = %id,
                    events = events.len(),
                    matched,
                    waiting,
                    "observe wait"
                );
                return Ok(json!({
                    "instanceId": id,
                    "events": events,
                    "matched": matched,
                    "timedOut": waiting && !matched,
                }));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
            events.extend(self.drain_observe(Some(&id))?.1);
        }
    }

    fn drain_observe(&self, instance_id: Option<&str>) -> Result<(String, Vec<Value>), String> {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id;
        let mask = self.instances.read_mask(&id).unwrap_or_default();
        let mut events = Vec::new();
        while let Some(msg) = self.instances.try_recv_inbound(&id) {
            if InstanceMap::inbound_visible(&mask, &msg) {
                events.push(serde_json::to_value(&msg).unwrap_or(Value::Null));
            }
        }
        Ok((id, events))
    }

    /// Inject a touch edge. When `wait` is true, wait for `InputAck`.
    pub async fn inject_touch_json(
        &self,
        instance_id: Option<&str>,
        kind: u32,
        x: u32,
        y: u32,
        wait: bool,
    ) -> Result<Value, String> {
        self.inject(
            instance_id,
            wait,
            InjectTouch {
                kind,
                x,
                y,
                ..Default::default()
            },
        )
        .await
    }

    /// Inject a named key. When `wait` is true, wait for `InputAck`.
    pub async fn inject_key_json(
        &self,
        instance_id: Option<&str>,
        name: String,
        hold_ms: u32,
        wait: bool,
    ) -> Result<Value, String> {
        self.inject(
            instance_id,
            wait,
            InjectKey {
                name,
                hold_ms,
                ..Default::default()
            },
        )
        .await
    }

    /// Inject Home. When `wait` is true, wait for `InputAck`.
    pub async fn inject_home_json(
        &self,
        instance_id: Option<&str>,
        hold_ms: u32,
        wait: bool,
    ) -> Result<Value, String> {
        self.inject(
            instance_id,
            wait,
            InjectHome {
                hold_ms,
                ..Default::default()
            },
        )
        .await
    }

    /// Inject a swipe. When `wait` is true, wait for `InputAck`.
    pub async fn inject_swipe_json(
        &self,
        instance_id: Option<&str>,
        start_x: u32,
        start_y: u32,
        end_x: u32,
        end_y: u32,
        duration_ms: u32,
        wait: bool,
    ) -> Result<Value, String> {
        self.inject(
            instance_id,
            wait,
            InjectSwipe {
                start_x,
                start_y,
                end_x,
                end_y,
                duration_ms,
                ..Default::default()
            },
        )
        .await
    }

    /// Enqueue inject-enabled.
    pub fn set_inject_enabled_json(
        &self,
        instance_id: Option<&str>,
        enabled: bool,
    ) -> Result<Value, String> {
        self.enqueue(
            instance_id,
            false,
            SetInjectEnabled {
                enabled,
                ..Default::default()
            },
        )
    }

    /// Request a snapshot. When `wait` is true, wait for a frame or error.
    pub async fn request_snapshot_result(
        &self,
        instance_id: Option<&str>,
        region: bool,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        format: u32,
        wait: bool,
    ) -> Result<CallToolResult, McpError> {
        let payload = SnapshotRequest {
            region,
            x,
            y,
            width,
            height,
            format,
            ..Default::default()
        };
        if !wait {
            return Self::tool_result(self.enqueue(instance_id, false, payload));
        }
        match self.enqueue_wait(instance_id, false, payload).await {
            Ok((id, corr, reply)) => match reply.payload {
                Some(sim_to_server::Payload::Snapshot(frame)) => {
                    let mime = if frame.mime_type.is_empty() {
                        "image/png".to_string()
                    } else {
                        frame.mime_type.clone()
                    };
                    let text = json!({
                        "instanceId": id,
                        "corr": corr,
                        "mimeType": mime,
                        "width": frame.width,
                        "height": frame.height,
                        "generation": frame.generation,
                    });
                    Ok(CallToolResult::success(vec![
                        ContentBlock::text(text.to_string()),
                        ContentBlock::image(BASE64.encode(&frame.pixels), mime),
                    ]))
                }
                Some(sim_to_server::Payload::SnapshotError(err)) => {
                    let message = if err.message.is_empty() {
                        "snapshot failed".to_string()
                    } else {
                        err.message.clone()
                    };
                    Self::tool_result(Err(message))
                }
                _ => Self::tool_result(Err("unexpected session reply".into())),
            },
            Err(message) => Self::tool_result(Err(message)),
        }
    }

    /// Enqueue a session view mask and remember it for host-side observe.
    pub fn set_session_view_json(
        &self,
        instance_id: Option<&str>,
        paths: Vec<String>,
    ) -> Result<Value, String> {
        let snap = self.resolve(instance_id)?;
        let id = snap.register.instance_id;
        let queued = self.enqueue(
            Some(&id),
            false,
            SetSessionView {
                read_mask: FieldMask {
                    paths: paths.clone(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            },
        )?;
        self.instances.set_read_mask(&id, paths);
        Ok(queued)
    }

    /// Enqueue a shutdown request.
    pub fn shutdown_instance_json(&self, instance_id: Option<&str>) -> Result<Value, String> {
        let queued = self.enqueue(instance_id, false, ShutdownRequest::default());
        match &queued {
            Ok(body) => tracing::info!(
                instance_id = body.get("instanceId").and_then(|v| v.as_str()),
                "shutdown_instance queued"
            ),
            Err(err) => tracing::warn!(?instance_id, error = %err, "shutdown_instance failed"),
        }
        queued
    }

    /// Start the configured binary and wait for `Register`.
    pub async fn start_instance_json(
        &self,
        instance_id: &str,
        headless: bool,
        cwd: Option<&str>,
        sample_book: bool,
        auto_sleep: bool,
    ) -> Result<Value, String> {
        if !crate::is_valid_instance_id(instance_id) {
            return Err(SpawnError::InvalidId.as_str());
        }
        let connected = self.instances.get(instance_id).is_some();
        let cwd_path = cwd.map(PathBuf::from);
        let pid = self
            .spawn
            .start(
                instance_id,
                headless,
                cwd_path.as_deref(),
                connected,
                sample_book,
                auto_sleep,
            )
            .await
            .map_err(|err| {
                tracing::warn!(instance_id, error = %err.as_str(), "start_instance failed");
                err.as_str()
            })?;
        let deadline = Instant::now() + self.spawn.wait();
        loop {
            if let Some(snap) = self.instances.get(instance_id) {
                tracing::info!(instance_id, pid, "start_instance registered");
                return Ok(json!({
                    "instanceId": snap.register.instance_id,
                    "pid": pid,
                    "register": snap.register,
                    "lastHeartbeat": snap.last_heartbeat,
                    "sampleBook": sample_book,
                    "autoSleep": auto_sleep,
                }));
            }
            if !self.spawn.is_alive(instance_id) {
                self.spawn.reap(instance_id).await;
                tracing::warn!(instance_id, "start_instance: child exited before register");
                return Err(SpawnError::ExitedBeforeRegister.as_str());
            }
            if Instant::now() >= deadline {
                self.spawn.reap(instance_id).await;
                tracing::warn!(instance_id, "start_instance: register timeout");
                return Err(SpawnError::RegisterTimeout.as_str());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn reap_if_spawned(&self, instance_id: Option<&str>) {
        let id = match instance_id {
            Some(id) if crate::is_valid_instance_id(id) => id.to_string(),
            Some(_) => return,
            None => match self.instances.default_instance() {
                Some(id) => id,
                None => return,
            },
        };
        self.spawn.reap(&id).await;
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

fn default_wait() -> bool {
    true
}

fn default_headless() -> bool {
    true
}

fn default_sample_book() -> bool {
    true
}

fn observe_until_matched(
    until_log: Option<&str>,
    until_generation_gt: Option<u64>,
    events: &[Value],
    generation: Option<u64>,
) -> bool {
    let mut any = false;
    let mut hit = false;
    if let Some(needle) = until_log {
        any = true;
        if events.iter().any(|event| {
            event
                .get("log")
                .and_then(|log| log.get("text"))
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(needle))
        }) {
            hit = true;
        }
    }
    if let Some(min) = until_generation_gt {
        any = true;
        if generation.is_some_and(|value| value > min) {
            hit = true;
        }
    }
    !any || hit
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
    /// When true (default), wait for InputAck.
    #[serde(default = "default_wait")]
    wait: bool,
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
    /// When true (default), wait for InputAck.
    #[serde(default = "default_wait")]
    wait: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct InjectHomeParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// Hold duration in milliseconds.
    #[serde(default)]
    hold_ms: u32,
    /// When true (default), wait for InputAck.
    #[serde(default = "default_wait")]
    wait: bool,
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
    /// When true (default), wait for InputAck.
    #[serde(default = "default_wait")]
    wait: bool,
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
    /// When true (default), wait for SnapshotFrame or SnapshotError.
    #[serde(default = "default_wait")]
    wait: bool,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StartInstanceParams {
    /// Instance id (1–64 bytes). Required. Not taken from `--default-instance`.
    instance_id: String,
    /// When true (default), pass `--sim-headless`.
    #[serde(default = "default_headless")]
    headless: bool,
    /// Working directory. When omitted, a per-instance directory under `$TMPDIR`.
    #[serde(default)]
    cwd: Option<String>,
    /// When true (default), copy the committed CrossPoint Reader README EPUB
    /// into `fs_/books/` under the instance working directory.
    #[serde(default = "default_sample_book")]
    sample_book: bool,
    /// When true, keep firmware's 10-minute idle auto-sleep. When false or
    /// omitted, use `--auto-sleep` / `CSM_AUTO_SLEEP` (default false) and seed
    /// never-sleep settings (`sleepTimeoutMinutes` 31).
    #[serde(default)]
    auto_sleep: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ObserveParams {
    /// Instance id (1–64 bytes). Required unless `--default-instance` is set.
    #[serde(default)]
    instance_id: Option<String>,
    /// Timeout in milliseconds. `0` is a one-shot drain. Omitted with an
    /// until-condition uses `--observe-wait-ms` / `CSM_OBSERVE_WAIT_MS`.
    #[serde(default)]
    wait_ms: Option<u32>,
    /// Succeed when a drained `log.text` contains this substring.
    #[serde(default)]
    until_log: Option<String>,
    /// Succeed when `lastHeartbeat.framebufferGeneration` is greater than this.
    #[serde(default)]
    until_generation_gt: Option<u64>,
}

#[tool_router]
impl McpServer {
    #[tool(
        description = "List connected simulator instances (instanceId, register, lastHeartbeat). A session appears after inbound dial or start_instance."
    )]
    fn list_instances(&self) -> Result<CallToolResult, McpError> {
        Self::tool_result(Ok(self.list_instances_json()))
    }

    #[tool(
        description = "Get one connected instance by instance_id (1-64 bytes, or --default-instance). Returns register and lastHeartbeat."
    )]
    fn get_instance(
        &self,
        Parameters(params): Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.get_instance_json(params.instance_id.as_deref()))
    }

    #[tool(
        description = "Start the operator-configured --simulator binary and wait for Register. Requires an explicit instance_id. Defaults to headless. sample_book (default true) seeds fs_/books/CrossPoint-Reader.epub from the committed README fixture. auto_sleep (default false, or --auto-sleep) seeds never-sleep settings; pass true for firmware's 10-minute idle sleep. Fails if spawn is not configured, the id is already connected, or Register times out. Does not build firmware."
    )]
    async fn start_instance(
        &self,
        Parameters(params): Parameters<StartInstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let auto_sleep = params
            .auto_sleep
            .unwrap_or_else(|| self.spawn.auto_sleep_default());
        Self::tool_result(
            self.start_instance_json(
                &params.instance_id,
                params.headless,
                params.cwd.as_deref(),
                params.sample_book,
                auto_sleep,
            )
            .await,
        )
    }

    #[tool(
        description = "Inject a touch edge or tap on the named instance; waits for InputAck unless wait is false"
    )]
    async fn inject_touch(
        &self,
        Parameters(params): Parameters<InjectTouchParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.inject_touch_json(
                params.instance_id.as_deref(),
                params.kind,
                params.x,
                params.y,
                params.wait,
            )
            .await,
        )
    }

    #[tool(
        description = "Inject a named device key (BACK, ENTER, LEFT, RIGHT, UP, DOWN, POWER, SLEEP, QUIT) on the named instance; waits for InputAck unless wait is false"
    )]
    async fn inject_key(
        &self,
        Parameters(params): Parameters<InjectKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.inject_key_json(
                params.instance_id.as_deref(),
                params.name,
                params.hold_ms,
                params.wait,
            )
            .await,
        )
    }

    #[tool(
        description = "Inject the Home key on the named instance; waits for InputAck unless wait is false. Rejected when the board has no Home key."
    )]
    async fn inject_home(
        &self,
        Parameters(params): Parameters<InjectHomeParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.inject_home_json(params.instance_id.as_deref(), params.hold_ms, params.wait)
                .await,
        )
    }

    #[tool(
        description = "Inject a swipe on the named instance; waits for InputAck unless wait is false. Rejected when the board has no touch."
    )]
    async fn inject_swipe(
        &self,
        Parameters(params): Parameters<InjectSwipeParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.inject_swipe_json(
                params.instance_id.as_deref(),
                params.start_x,
                params.start_y,
                params.end_x,
                params.end_y,
                params.duration_ms,
                params.wait,
            )
            .await,
        )
    }

    #[tool(
        description = "Enable or disable remote inject on the named instance (SetInjectEnabled). Fire-and-forget."
    )]
    fn set_inject_enabled(
        &self,
        Parameters(params): Parameters<SetInjectEnabledParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.set_inject_enabled_json(params.instance_id.as_deref(), params.enabled),
        )
    }

    #[tool(
        description = "Request a panel snapshot as PNG (MCP image plus metadata). Waits for SnapshotFrame unless wait is false. Optional region crop."
    )]
    async fn request_snapshot(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        self.request_snapshot_result(
            params.instance_id.as_deref(),
            params.region,
            params.x,
            params.y,
            params.width,
            params.height,
            params.format,
            params.wait,
        )
        .await
    }

    #[tool(
        description = "Set which SimToServer payloads observe should emit (read_mask names: register, heartbeat, snapshot, snapshot_error, log, input_ack, input_observed, goodbye). Empty mask emits everything."
    )]
    fn set_session_view(
        &self,
        Parameters(params): Parameters<SessionViewParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(self.set_session_view_json(params.instance_id.as_deref(), params.paths))
    }

    #[tool(
        description = "Ask the named instance to shut down (ShutdownRequest). Reaps a child started by start_instance. Fire-and-forget on the session hop."
    )]
    async fn shutdown_instance(
        &self,
        Parameters(params): Parameters<InstanceParams>,
    ) -> Result<CallToolResult, McpError> {
        let queued = self.shutdown_instance_json(params.instance_id.as_deref());
        self.reap_if_spawned(params.instance_id.as_deref()).await;
        Self::tool_result(queued)
    }

    #[tool(
        description = "Drain inbound session events for the named instance, including human and remote InputObserved. Honors the last set_session_view mask. Optional until_log / until_generation_gt wait until any condition matches or wait_ms elapses."
    )]
    async fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        Self::tool_result(
            self.observe_wait_json(
                params.instance_id.as_deref(),
                params.wait_ms,
                params.until_log.as_deref(),
                params.until_generation_gt,
            )
            .await,
        )
    }
}

#[tool_handler(
    name = "crosspoint-simulator-mcp-proxy",
    version = "0.1.0",
    instructions = "Control and observe eBook firmware simulator instances over Session. A session appears when a simulator dials this process, or when start_instance launches the operator-configured --simulator binary. This server does not build firmware or accept a client-supplied binary path. Board and compile-time firmware options stay in the binary; Register reports them after connect. Tools that target a session require instance_id (1-64 bytes) unless --default-instance is set. Do not omit an id when only one simulator is connected. start_instance always requires an explicit instance_id and defaults to headless. sample_book (default true) seeds fs_/books/CrossPoint-Reader.epub from the committed README fixture; pass false for an empty library. auto_sleep (default false, or --auto-sleep / CSM_AUTO_SLEEP) seeds fs_/.crosspoint/settings.json with sleepTimeoutMinutes 31 (never); pass true to keep firmware's 10-minute idle sleep. Use list_instances and get_instance to see connected peers. inject_touch, inject_key, inject_home, and inject_swipe wait for InputAck unless wait is false. request_snapshot waits for a PNG SnapshotFrame. observe drains inbound events including human and remote InputObserved. Pass until_log and/or until_generation_gt to wait; wait_ms overrides the process default (--observe-wait-ms / CSM_OBSERVE_WAIT_MS, 8000). InputAck means the inject was queued, not that the panel painted; after inject, observe for ACT/ERS/GFX logs or a framebufferGeneration bump. Do not sleep as synchronization. Tap (inject_touch) when Register.capTouch is true; default X4 has no touch and no Home. shutdown_instance sends ShutdownRequest and reaps a child this server started. Read csm://capabilities for the machine-readable surface. csm://instances lists connections; csm://instances/{id} is one snapshot."
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
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut resources = vec![
            Resource::new(CAPABILITIES_URI, "capabilities").with_description(
                "Machine-readable MCP surface: tools, resources, instance-id rules, and spawn",
            ),
            Resource::new(INSTANCES_URI, "instances")
                .with_description("Connected simulator instances (register and last heartbeat)"),
        ];
        let mut ids = self.instances.list();
        ids.sort();
        for id in ids {
            resources.push(
                Resource::new(Self::instance_uri(&id), id.clone())
                    .with_description("One connected instance snapshot"),
            );
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new("csm://instances/{id}", "instance").with_description(
                "Snapshot of one connected instance (register and last heartbeat)",
            ),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri == CAPABILITIES_URI {
            return Ok(ReadResourceResult::new(vec![ResourceContents::text(
                self.capabilities_document().to_string(),
                CAPABILITIES_URI,
            )])
            .into());
        }
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
    spawn: SpawnConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!("mcp serving on stdio");
    let server = McpServer::with_spawn(instances, spawn);
    let running = server.clone().serve(stdio()).await?;
    let result = running.waiting().await;
    server.reap_spawned().await;
    result?;
    Ok(())
}

/// Streamable HTTP router mounted at `/mcp`.
pub fn mcp_http_router(instances: InstanceMap) -> Router {
    mcp_http_router_from_server(McpServer::new(instances))
}

fn mcp_http_router_from_server(server: McpServer) -> Router {
    let config = StreamableHttpServerConfig::default().with_json_response(true);
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    Router::new().nest_service("/mcp", service)
}

/// Serve MCP over Streamable HTTP on `addr`.
pub async fn serve_mcp_http(
    addr: std::net::SocketAddr,
    instances: InstanceMap,
    spawn: SpawnConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    serve_mcp_http_listener_with_spawn(listener, instances, spawn).await
}

/// Serve MCP over Streamable HTTP on an already-bound listener.
pub async fn serve_mcp_http_listener(
    listener: TcpListener,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_mcp_http_listener_with_spawn(listener, instances, SpawnConfig::default()).await
}

async fn serve_mcp_http_listener_with_spawn(
    listener: TcpListener,
    instances: InstanceMap,
    spawn: SpawnConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let local = listener.local_addr()?;
    tracing::info!(%local, "mcp serving streamable http at /mcp");
    let server = McpServer::with_spawn(instances, spawn);
    let result = axum::serve(listener, mcp_http_router_from_server(server.clone())).await;
    server.reap_spawned().await;
    result?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
        Heartbeat, InputAck, InputObserved, InputSource, KeyEdge, LogLine, Register, SimToServer,
        SnapshotError, SnapshotFrame, server_to_sim,
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

    #[tokio::test]
    async fn inject_is_readable_on_the_session_queue() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map);
        let queued = mcp
            .inject_touch_json(Some("sim-a"), 3, 10, 20, false)
            .await
            .unwrap();
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

    #[tokio::test]
    async fn enqueue_reports_unknown_and_queue_full() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        map.insert(register("sim-a"), 4, tx);
        let mcp = McpServer::new(map);
        assert!(mcp.shutdown_instance_json(Some("nope")).is_err());
        mcp.shutdown_instance_json(Some("sim-a")).unwrap();
        assert_eq!(
            mcp.inject_key_json(Some("sim-a"), "ENTER".into(), 0, false)
                .await
                .unwrap_err(),
            "outbound queue is full"
        );
        assert_eq!(
            mcp.inject_key_json(Some("sim-a"), "ENTER".into(), 0, true)
                .await
                .unwrap_err(),
            "outbound queue is full"
        );
        assert_eq!(
            mcp.inject_touch_json(Some("nope"), 3, 0, 0, true)
                .await
                .unwrap_err(),
            "unknown instance"
        );
        let missing = mcp
            .request_snapshot_result(Some("nope"), false, 0, 0, 0, 0, 0, true)
            .await
            .unwrap();
        assert_eq!(missing.is_error, Some(true));
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
    fn observe_honors_session_view_mask() {
        let map = InstanceMap::new();
        let (_rx, sink) = insert(&map, "sim-a");
        sink.push(SimToServer {
            seq: 2,
            payload: Some(
                csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::LogLine {
                    text: "keep".into(),
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
                    source: InputSource::Human.into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        let mcp = McpServer::new(map);
        mcp.set_session_view_json(Some("sim-a"), vec!["log".into()])
            .unwrap();
        let observed = mcp.observe_json(Some("sim-a")).unwrap();
        let events = observed["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].get("log").is_some());
    }

    #[tokio::test]
    async fn observe_wait_matches_log_or_times_out() {
        let map = InstanceMap::new();
        let (_rx, sink) = insert(&map, "sim-a");
        sink.push(SimToServer {
            seq: 2,
            payload: Some(
                LogLine {
                    text: "ACT Entering activity: Home".into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        let mcp = McpServer::new(map);
        let hit = mcp
            .observe_wait_json(
                Some("sim-a"),
                Some(50),
                Some("Entering activity: Home"),
                None,
            )
            .await
            .unwrap();
        assert_eq!(hit["matched"], true);
        assert_eq!(hit["timedOut"], false);
        assert_eq!(hit["events"].as_array().unwrap().len(), 1);

        let miss = mcp
            .observe_wait_json(Some("sim-a"), Some(40), Some("ERS Rendered page"), None)
            .await
            .unwrap();
        assert_eq!(miss["matched"], false);
        assert_eq!(miss["timedOut"], true);
        assert!(miss["events"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn observe_wait_matches_generation_and_late_log() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(8);
        let (token, sink) = map.insert(register("sim-a"), 8, tx);
        map.set_heartbeat(
            "sim-a",
            token,
            Heartbeat {
                framebuffer_generation: 4,
                ..Default::default()
            },
        );
        let mcp = McpServer::new(map.clone());
        let generation = mcp
            .observe_wait_json(Some("sim-a"), Some(50), None, Some(3))
            .await
            .unwrap();
        assert_eq!(generation["matched"], true);
        assert_eq!(generation["timedOut"], false);

        let mcp_late = McpServer::new(map);
        let waiter = tokio::spawn(async move {
            mcp_late
                .observe_wait_json(Some("sim-a"), Some(400), Some("ERS Rendered page"), None)
                .await
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        sink.push(SimToServer {
            seq: 9,
            payload: Some(
                LogLine {
                    text: "ERS Rendered page in 12ms".into(),
                    ..Default::default()
                }
                .into(),
            ),
            ..Default::default()
        });
        let late = waiter.await.unwrap().unwrap();
        assert_eq!(late["matched"], true);
        assert_eq!(late["timedOut"], false);
    }

    #[tokio::test]
    async fn observe_wait_without_until_is_a_one_shot_drain() {
        let map = InstanceMap::new();
        insert(&map, "sim-a");
        let mcp = McpServer::new(map);
        let observed = mcp
            .observe_wait_json(Some("sim-a"), None, None, None)
            .await
            .unwrap();
        assert_eq!(observed["matched"], true);
        assert_eq!(observed["timedOut"], false);
        assert!(observed["events"].as_array().unwrap().is_empty());
        let waited = mcp
            .observe_wait_json(Some("sim-a"), Some(40), None, None)
            .await
            .unwrap();
        assert_eq!(waited["matched"], true);
        assert_eq!(waited["timedOut"], false);
    }

    #[tokio::test]
    async fn remaining_enqueues_and_snapshot_do_not_wait() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map);
        mcp.inject_home_json(Some("sim-a"), 40, false)
            .await
            .unwrap();
        mcp.inject_swipe_json(Some("sim-a"), 1, 2, 3, 4, 50, false)
            .await
            .unwrap();
        mcp.set_inject_enabled_json(Some("sim-a"), false).unwrap();
        let snap = mcp
            .request_snapshot_result(Some("sim-a"), true, 0, 0, 8, 8, 0, false)
            .await
            .unwrap();
        assert_ne!(snap.is_error, Some(true));
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

    #[tokio::test]
    async fn inject_wait_completes_on_input_ack() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter =
            tokio::spawn(
                async move { mcp.inject_touch_json(Some("sim-a"), 3, 10, 20, true).await },
            );
        let msg = rx.recv().await.unwrap();
        assert!(msg.ack_requested);
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
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
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(result["accepted"], true);
        assert_eq!(result["corr"], msg.corr);
    }

    #[tokio::test]
    async fn inject_wait_rejects_with_reason() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter = tokio::spawn(async move {
            mcp.inject_key_json(Some("sim-a"), "ENTER".into(), 0, true)
                .await
        });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                payload: Some(
                    InputAck {
                        accepted: false,
                        reason: "inject_disabled".into(),
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        assert_eq!(waiter.await.unwrap().unwrap_err(), "inject_disabled");
    }

    #[tokio::test]
    async fn inject_wait_rejects_without_reason() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter =
            tokio::spawn(async move { mcp.inject_home_json(Some("sim-a"), 0, true).await });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                payload: Some(
                    InputAck {
                        accepted: false,
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        assert_eq!(waiter.await.unwrap().unwrap_err(), "inject rejected");
    }

    #[tokio::test]
    async fn inject_wait_rejects_unexpected_reply() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter = tokio::spawn(async move {
            mcp.inject_swipe_json(Some("sim-a"), 1, 2, 3, 4, 10, true)
                .await
        });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                ..Default::default()
            },
        );
        assert_eq!(
            waiter.await.unwrap().unwrap_err(),
            "unexpected session reply"
        );
    }

    #[tokio::test]
    async fn inject_wait_maps_disconnect() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(4);
        let (token, _sink) = map.insert(register("sim-a"), 4, tx);
        let mcp = McpServer::new(map.clone());
        let waiter =
            tokio::spawn(async move { mcp.inject_home_json(Some("sim-a"), 0, true).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        map.remove_if("sim-a", token);
        assert_eq!(waiter.await.unwrap().unwrap_err(), "instance disconnected");
    }

    #[tokio::test]
    async fn snapshot_wait_returns_image_content() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter = tokio::spawn(async move {
            mcp.request_snapshot_result(Some("sim-a"), false, 0, 0, 0, 0, 0, true)
                .await
        });
        let msg = rx.recv().await.unwrap();
        assert!(!msg.ack_requested);
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                payload: Some(
                    SnapshotFrame {
                        pixels: vec![0x89, 0x50],
                        width: 2,
                        height: 3,
                        generation: 9,
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        let result = waiter.await.unwrap().unwrap();
        assert_ne!(result.is_error, Some(true));
        let image = result
            .content
            .iter()
            .find_map(ContentBlock::as_image)
            .expect("image block");
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.data, BASE64.encode([0x89, 0x50]));
        let text = result
            .content
            .iter()
            .find_map(ContentBlock::as_text)
            .expect("text block");
        let body: Value = serde_json::from_str(&text.text).unwrap();
        assert_eq!(body["width"], 2);
        assert_eq!(body["height"], 3);
        assert_eq!(body["generation"], 9);
    }

    #[tokio::test]
    async fn snapshot_wait_returns_error_message() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter = tokio::spawn(async move {
            mcp.request_snapshot_result(Some("sim-a"), false, 0, 0, 0, 0, 0, true)
                .await
        });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                payload: Some(
                    SnapshotError {
                        message: "panel busy".into(),
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
        );
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .iter()
                .find_map(ContentBlock::as_text)
                .map(|text| text.text.as_str()),
            Some("panel busy")
        );
    }

    #[tokio::test]
    async fn snapshot_wait_defaults_empty_error_and_rejects_unexpected() {
        let map = InstanceMap::new();
        let (mut rx, _sink) = insert(&map, "sim-a");
        let mcp = McpServer::new(map.clone());
        let waiter = tokio::spawn({
            let mcp = mcp.clone();
            async move {
                mcp.request_snapshot_result(Some("sim-a"), false, 0, 0, 0, 0, 0, true)
                    .await
            }
        });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
                payload: Some(SnapshotError::default().into()),
                ..Default::default()
            },
        );
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .iter()
                .find_map(ContentBlock::as_text)
                .map(|text| text.text.as_str()),
            Some("snapshot failed")
        );

        let waiter = tokio::spawn(async move {
            mcp.request_snapshot_result(Some("sim-a"), false, 0, 0, 0, 0, 0, true)
                .await
        });
        let msg = rx.recv().await.unwrap();
        map.push_inbound(
            "sim-a",
            SimToServer {
                corr: msg.corr,
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
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result
                .content
                .iter()
                .find_map(ContentBlock::as_text)
                .map(|text| text.text.as_str()),
            Some("unexpected session reply")
        );
    }

    #[test]
    fn capabilities_and_instructions_describe_the_surface() {
        let info = McpServer::new(InstanceMap::new()).get_info();
        let instructions = info.instructions.expect("instructions");
        assert!(instructions.contains("start_instance"));
        assert!(instructions.contains("sample_book"));
        assert!(instructions.contains("auto_sleep"));
        assert!(instructions.contains("until_log"));
        assert!(instructions.contains("framebufferGeneration"));
        assert!(instructions.contains("capTouch"));
        assert!(instructions.contains("csm://capabilities"));
        assert!(!instructions.is_empty());
        let caps = McpServer::capabilities_json(false);
        assert_eq!(caps["spawn"]["configured"], false);
        assert_eq!(caps["spawn"]["firmwareBuild"], false);
        assert_eq!(caps["spawn"]["clientPassesBinary"], false);
        assert_eq!(caps["spawn"]["sampleBookDefault"], true);
        assert_eq!(caps["spawn"]["autoSleepDefault"], false);
        assert_eq!(caps["spawn"]["autoSleep"]["neverMinutes"], 31);
        assert_eq!(caps["observe"]["waitMsDefault"], 8000);
        assert_eq!(caps["observe"]["untilLogParam"], "until_log");
        assert_eq!(
            caps["spawn"]["sampleBook"]["filename"],
            "CrossPoint-Reader.epub"
        );
        let live = McpServer::with_spawn(
            InstanceMap::new(),
            SpawnConfig {
                auto_sleep_default: true,
                observe_wait_ms: 1500,
                ..SpawnConfig::default()
            },
        )
        .capabilities_document();
        assert_eq!(live["spawn"]["autoSleepDefault"], true);
        assert_eq!(live["observe"]["waitMsDefault"], 1500);
        let tools = caps["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), TOOL_NAMES.len());
        for name in TOOL_NAMES {
            assert!(tools.iter().any(|value| value == *name), "{name}");
        }
        assert_eq!(
            McpServer::capabilities_json(true)["spawn"]["configured"],
            true
        );
    }

    #[tokio::test]
    async fn start_instance_errors_when_spawn_is_unset_or_already_connected() {
        let mcp = McpServer::new(InstanceMap::new());
        assert_eq!(
            mcp.start_instance_json("e2e-a", true, None, true, false)
                .await
                .unwrap_err(),
            "spawn is not configured; set --simulator / CSM_SIMULATOR"
        );
        let map = InstanceMap::new();
        insert(&map, "sim-a");
        let mcp = McpServer::with_spawn(
            map,
            SpawnConfig {
                binary: Some(PathBuf::from("/bin/sh")),
                ..SpawnConfig::default()
            },
        );
        assert_eq!(
            mcp.start_instance_json("sim-a", true, None, true, false)
                .await
                .unwrap_err(),
            "instance is already connected"
        );
        assert_eq!(
            mcp.start_instance_json("", true, None, true, false)
                .await
                .unwrap_err(),
            "instance_id is required (1-64 bytes)"
        );
    }

    #[tokio::test]
    async fn start_instance_times_out_without_register() {
        let mcp = McpServer::with_spawn(
            InstanceMap::new(),
            SpawnConfig {
                binary: Some(PathBuf::from("/bin/sh")),
                extra_args: vec!["-c".into(), "sleep 30".into()],
                wait: Duration::from_millis(250),
                ..SpawnConfig::default()
            },
        );
        assert_eq!(
            mcp.start_instance_json("no-reg", true, None, false, false)
                .await
                .unwrap_err(),
            "timed out waiting for register"
        );
    }
}
