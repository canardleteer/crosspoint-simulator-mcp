//! Connected simulator instances keyed by `Register.instance_id`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    Heartbeat, Register, ServerToSim, SimToServer, sim_to_server,
};
use tokio::sync::{mpsc, oneshot};

/// Capacity of each per-instance inbound and outbound queue.
pub const QUEUE_CAPACITY: usize = 32;

/// How long MCP waits for a corr-matched `InputAck` or snapshot reply.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum length of a [`Register::instance_id`] and of any later selector.
pub const INSTANCE_ID_MAX_LEN: usize = 64;

/// True when `id` is a usable instance id: 1–64 bytes, not empty.
pub fn is_valid_instance_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= INSTANCE_ID_MAX_LEN
}

/// Why [`InstanceMap::try_send`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendError {
    /// No connected instance with that id.
    UnknownInstance,
    /// Outbound queue is full; a stuck peer must not block the caller.
    QueueFull,
}

/// Why [`InstanceMap::send_and_wait`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// No connected instance with that id.
    UnknownInstance,
    /// Outbound queue is full; a stuck peer must not block the caller.
    QueueFull,
    /// No corr-matched reply arrived before [`REPLY_TIMEOUT`].
    Timeout,
    /// The instance disconnected while waiting.
    Disconnected,
}

/// Why [`InstanceMap::resolve`] could not pick an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// The selector was empty, omitted, or longer than [`INSTANCE_ID_MAX_LEN`].
    EmptyId,
    /// The named (or default) instance is not connected.
    UnknownInstance,
}

/// Snapshot of a connected instance for listing and tests.
#[derive(Clone, Debug)]
pub struct InstanceSnapshot {
    /// Session token that currently owns this id.
    pub token: u64,
    /// Identity from the stream's first `Register`.
    pub register: Register,
    /// Latest heartbeat, if any.
    pub last_heartbeat: Option<Heartbeat>,
}

#[derive(Clone)]
struct InboundQueue {
    items: Arc<Mutex<VecDeque<SimToServer>>>,
    cap: usize,
}

impl InboundQueue {
    fn new(cap: usize) -> Self {
        Self {
            items: Arc::new(Mutex::new(VecDeque::with_capacity(cap))),
            cap,
        }
    }

    fn push(&self, msg: SimToServer) {
        let mut q = self.items.lock().expect("inbound queue lock");
        if q.len() == self.cap {
            q.pop_front();
        }
        q.push_back(msg);
    }

    fn try_recv(&self) -> Option<SimToServer> {
        self.items.lock().expect("inbound queue lock").pop_front()
    }
}

struct Instance {
    token: u64,
    register: Register,
    last_heartbeat: Option<Heartbeat>,
    inbound: InboundQueue,
    outbound_tx: mpsc::Sender<ServerToSim>,
    /// Last `SetSessionView.read_mask` enqueued by MCP. Empty means emit all.
    read_mask: Vec<String>,
}

/// Shared map of connected simulator sessions.
#[derive(Clone)]
pub struct InstanceMap {
    inner: Arc<Mutex<HashMap<String, Instance>>>,
    next_token: Arc<AtomicU64>,
    next_corr: Arc<AtomicU64>,
    default_instance: Arc<Mutex<Option<String>>>,
    waiters: Arc<Mutex<HashMap<u64, oneshot::Sender<SimToServer>>>>,
    waiter_instance: Arc<Mutex<HashMap<u64, String>>>,
}

impl Default for InstanceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl InstanceMap {
    /// Empty map.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            next_token: Arc::new(AtomicU64::new(1)),
            next_corr: Arc::new(AtomicU64::new(1)),
            default_instance: Arc::new(Mutex::new(None)),
            waiters: Arc::new(Mutex::new(HashMap::new())),
            waiter_instance: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Insert or replace the session for `register.instance_id`.
    ///
    /// Returns the token that now owns the id. A later stream with the same
    /// id replaces this one.
    pub fn insert(
        &self,
        register: Register,
        inbound_cap: usize,
        outbound_tx: mpsc::Sender<ServerToSim>,
    ) -> (u64, InboundSink) {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let inbound = InboundQueue::new(inbound_cap);
        let sink = InboundSink {
            queue: inbound.clone(),
        };
        debug_assert!(
            is_valid_instance_id(&register.instance_id),
            "instance_id must be 1-64 bytes"
        );
        let id = register.instance_id.clone();
        let inst = Instance {
            token,
            register,
            last_heartbeat: None,
            inbound,
            outbound_tx,
            read_mask: Vec::new(),
        };
        self.inner
            .lock()
            .expect("instance map lock")
            .insert(id, inst);
        (token, sink)
    }

    /// True when `token` still owns `instance_id`.
    pub fn owns(&self, instance_id: &str, token: u64) -> bool {
        self.inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
            .is_some_and(|inst| inst.token == token)
    }

    /// Record a heartbeat if `token` still owns the id.
    pub fn set_heartbeat(&self, instance_id: &str, token: u64, heartbeat: Heartbeat) {
        let mut map = self.inner.lock().expect("instance map lock");
        if let Some(inst) = map.get_mut(instance_id)
            && inst.token == token
        {
            inst.last_heartbeat = Some(heartbeat);
        }
    }

    /// Drop the instance if `token` still owns it.
    pub fn remove_if(&self, instance_id: &str, token: u64) {
        let mut map = self.inner.lock().expect("instance map lock");
        if map.get(instance_id).is_some_and(|inst| inst.token == token) {
            map.remove(instance_id);
            drop(map);
            self.fail_waiters(instance_id);
        }
    }

    /// Connected instance ids in arbitrary order.
    pub fn list(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("instance map lock")
            .keys()
            .cloned()
            .collect()
    }

    /// Snapshots of every connected instance, in arbitrary order.
    pub fn snapshots(&self) -> Vec<InstanceSnapshot> {
        self.inner
            .lock()
            .expect("instance map lock")
            .values()
            .map(|inst| InstanceSnapshot {
                token: inst.token,
                register: inst.register.clone(),
                last_heartbeat: inst.last_heartbeat.clone(),
            })
            .collect()
    }

    /// Next `ServerToSim.corr` for an MCP enqueue.
    pub fn next_corr(&self) -> u64 {
        self.next_corr.fetch_add(1, Ordering::Relaxed)
    }

    /// Optional process default used only by [`Self::resolve_or_default`].
    ///
    /// Empty or over-long values are stored as unset. This is never inferred
    /// from how many simulators are connected.
    pub fn set_default_instance(&self, id: Option<String>) {
        *self.default_instance.lock().expect("default instance lock") =
            id.filter(|s| is_valid_instance_id(s));
    }

    /// Current default instance id, if any.
    pub fn default_instance(&self) -> Option<String> {
        self.default_instance
            .lock()
            .expect("default instance lock")
            .clone()
    }

    /// Snapshot of the instance named by `id`.
    ///
    /// `id` must be a valid instance id. This never picks "the only connected
    /// simulator" and never substitutes the process default.
    pub fn resolve(&self, id: &str) -> Result<InstanceSnapshot, ResolveError> {
        if !is_valid_instance_id(id) {
            return Err(ResolveError::EmptyId);
        }
        self.get(id).ok_or(ResolveError::UnknownInstance)
    }

    /// Resolve `id`, or the process default when `id` is omitted.
    ///
    /// An empty or over-long `id` is an error, not a wildcard. A missing `id`
    /// uses [`Self::default_instance`] when that is a valid id. Connection
    /// count is not consulted.
    pub fn resolve_or_default(&self, id: Option<&str>) -> Result<InstanceSnapshot, ResolveError> {
        match id {
            Some(id) => self.resolve(id),
            None => match self.default_instance() {
                Some(default) => self.resolve(&default),
                None => Err(ResolveError::EmptyId),
            },
        }
    }

    /// Snapshot of one instance, if connected.
    pub fn get(&self, instance_id: &str) -> Option<InstanceSnapshot> {
        self.inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
            .map(|inst| InstanceSnapshot {
                token: inst.token,
                register: inst.register.clone(),
                last_heartbeat: inst.last_heartbeat.clone(),
            })
    }

    /// Non-blocking enqueue of a server-to-sim envelope.
    pub fn try_send(&self, instance_id: &str, msg: ServerToSim) -> Result<(), TrySendError> {
        let map = self.inner.lock().expect("instance map lock");
        let inst = map.get(instance_id).ok_or(TrySendError::UnknownInstance)?;
        inst.outbound_tx
            .try_send(msg)
            .map_err(|_| TrySendError::QueueFull)
    }

    /// Store the host-side observe mask for `instance_id`.
    ///
    /// Paths are `SimToServer` payload names. An empty mask emits everything.
    pub fn set_read_mask(&self, instance_id: &str, paths: Vec<String>) {
        if let Some(inst) = self
            .inner
            .lock()
            .expect("instance map lock")
            .get_mut(instance_id)
        {
            inst.read_mask = paths;
        }
    }

    /// Current observe mask, if the instance is connected.
    pub fn read_mask(&self, instance_id: &str) -> Option<Vec<String>> {
        self.inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
            .map(|inst| inst.read_mask.clone())
    }

    /// `SimToServer` oneof name used by `SetSessionView.read_mask`.
    pub fn inbound_payload_name(msg: &SimToServer) -> Option<&'static str> {
        match msg.payload.as_ref() {
            Some(sim_to_server::Payload::Register(_)) => Some("register"),
            Some(sim_to_server::Payload::Heartbeat(_)) => Some("heartbeat"),
            Some(sim_to_server::Payload::Snapshot(_)) => Some("snapshot"),
            Some(sim_to_server::Payload::SnapshotError(_)) => Some("snapshot_error"),
            Some(sim_to_server::Payload::Log(_)) => Some("log"),
            Some(sim_to_server::Payload::InputAck(_)) => Some("input_ack"),
            Some(sim_to_server::Payload::InputObserved(_)) => Some("input_observed"),
            Some(sim_to_server::Payload::Goodbye(_)) => Some("goodbye"),
            None => None,
        }
    }

    /// True when `observe` should emit `msg` for this mask.
    pub fn inbound_visible(mask: &[String], msg: &SimToServer) -> bool {
        mask.is_empty()
            || Self::inbound_payload_name(msg)
                .is_some_and(|name| mask.iter().any(|path| path == name))
    }

    /// Pop one inbound envelope if present (tests and later MCP observe).
    pub fn try_recv_inbound(&self, instance_id: &str) -> Option<SimToServer> {
        self.inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
            .and_then(|inst| inst.inbound.try_recv())
    }

    /// Complete a corr waiter (if any) and enqueue the inbound envelope.
    pub fn push_inbound(&self, instance_id: &str, msg: SimToServer) {
        self.complete_waiter(&msg);
        if let Some(inst) = self
            .inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
        {
            inst.inbound.push(msg);
        }
    }

    /// Enqueue `msg` and wait for a corr-matched inbound reply.
    pub async fn send_and_wait(
        &self,
        instance_id: &str,
        msg: ServerToSim,
        timeout: Duration,
    ) -> Result<SimToServer, WaitError> {
        if self.get(instance_id).is_none() {
            return Err(WaitError::UnknownInstance);
        }
        let corr = msg.corr;
        let rx = self.register_waiter(instance_id, corr);
        match self.try_send(instance_id, msg) {
            Ok(()) => {}
            Err(TrySendError::UnknownInstance) => {
                self.take_waiter(corr);
                return Err(WaitError::UnknownInstance);
            }
            Err(TrySendError::QueueFull) => {
                self.take_waiter(corr);
                return Err(WaitError::QueueFull);
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(WaitError::Disconnected),
            Err(_) => {
                self.take_waiter(corr);
                Err(WaitError::Timeout)
            }
        }
    }

    fn register_waiter(&self, instance_id: &str, corr: u64) -> oneshot::Receiver<SimToServer> {
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().expect("waiters lock").insert(corr, tx);
        self.waiter_instance
            .lock()
            .expect("waiter instance lock")
            .insert(corr, instance_id.to_string());
        rx
    }

    fn take_waiter(&self, corr: u64) -> Option<oneshot::Sender<SimToServer>> {
        self.waiter_instance
            .lock()
            .expect("waiter instance lock")
            .remove(&corr);
        self.waiters.lock().expect("waiters lock").remove(&corr)
    }

    fn complete_waiter(&self, msg: &SimToServer) {
        if msg.corr == 0 {
            return;
        }
        if let Some(tx) = self.take_waiter(msg.corr) {
            let _ = tx.send(msg.clone());
        }
    }

    fn fail_waiters(&self, instance_id: &str) {
        let corrs: Vec<u64> = self
            .waiter_instance
            .lock()
            .expect("waiter instance lock")
            .iter()
            .filter(|(_, id)| id.as_str() == instance_id)
            .map(|(corr, _)| *corr)
            .collect();
        for corr in corrs {
            self.take_waiter(corr);
        }
    }
}

/// Push side of the inbound queue, held by the Session read loop.
#[derive(Clone)]
pub struct InboundSink {
    queue: InboundQueue,
}

impl InboundSink {
    /// Push, dropping the oldest item when the queue is full.
    pub fn push(&self, msg: SimToServer) {
        self.queue.push(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
        Goodbye, InputAck, InputObserved, LogLine, ShutdownRequest, SnapshotError, SnapshotFrame,
    };

    fn register(id: &str) -> Register {
        Register {
            instance_id: id.into(),
            ..Default::default()
        }
    }

    fn inbound(seq: u64) -> SimToServer {
        SimToServer {
            seq,
            ..Default::default()
        }
    }

    fn outbound(corr: u64) -> ServerToSim {
        ServerToSim {
            corr,
            payload: Some(ShutdownRequest::default().into()),
            ..Default::default()
        }
    }

    #[test]
    fn default_map_is_empty() {
        let map = InstanceMap::default();
        assert!(map.list().is_empty());
        assert!(map.get("missing").is_none());
        assert!(!map.owns("missing", 1));
        assert_eq!(
            map.try_send("missing", outbound(1)),
            Err(TrySendError::UnknownInstance)
        );
        assert!(map.try_recv_inbound("missing").is_none());
    }

    #[test]
    fn insert_owns_and_replace() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        let (token, _sink) = map.insert(register("a"), 4, tx);
        assert!(map.owns("a", token));
        assert!(!map.owns("a", token + 1));
        assert_eq!(map.list(), vec!["a".to_string()]);
        assert_eq!(map.get("a").unwrap().register.instance_id, "a");

        let (tx2, _rx2) = mpsc::channel(1);
        let (token2, _sink2) = map.insert(register("a"), 4, tx2);
        assert_ne!(token, token2);
        assert!(map.owns("a", token2));
        assert!(!map.owns("a", token));
    }

    #[test]
    fn heartbeat_and_remove_respect_token() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        let (token, _sink) = map.insert(register("a"), 4, tx);
        let hb = Heartbeat {
            framebuffer_generation: 3,
            inject_enabled: true,
            ..Default::default()
        };
        map.set_heartbeat("a", token + 1, hb.clone());
        assert!(map.get("a").unwrap().last_heartbeat.is_none());
        map.set_heartbeat("a", token, hb);
        assert_eq!(
            map.get("a")
                .unwrap()
                .last_heartbeat
                .unwrap()
                .framebuffer_generation,
            3
        );

        map.remove_if("a", token + 1);
        assert!(map.get("a").is_some());
        map.remove_if("a", token);
        assert!(map.get("a").is_none());
    }

    #[test]
    fn inbound_drops_oldest_when_full() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        let (_token, sink) = map.insert(register("q"), 2, tx);
        sink.push(inbound(1));
        sink.push(inbound(2));
        sink.push(inbound(3));
        assert_eq!(map.try_recv_inbound("q").unwrap().seq, 2);
        assert_eq!(map.try_recv_inbound("q").unwrap().seq, 3);
        assert!(map.try_recv_inbound("q").is_none());
    }

    #[test]
    fn outbound_try_send_and_queue_full() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(1);
        let (_token, _sink) = map.insert(register("q"), 2, tx);
        map.try_send("q", outbound(1)).unwrap();
        assert_eq!(map.try_send("q", outbound(2)), Err(TrySendError::QueueFull));
    }

    fn insert_id(map: &InstanceMap, id: &str) {
        let (tx, _rx) = mpsc::channel(1);
        map.insert(register(id), 4, tx);
    }

    #[test]
    fn instance_id_rejects_empty_and_overlong() {
        assert!(!is_valid_instance_id(""));
        assert!(!is_valid_instance_id(&"x".repeat(INSTANCE_ID_MAX_LEN + 1)));
        assert!(is_valid_instance_id("a"));
        assert!(is_valid_instance_id(&"x".repeat(INSTANCE_ID_MAX_LEN)));
    }

    #[test]
    fn resolve_requires_a_real_id() {
        let map = InstanceMap::new();
        insert_id(&map, "only");
        assert_eq!(map.resolve("").unwrap_err(), ResolveError::EmptyId);
        assert_eq!(
            map.resolve(&"x".repeat(INSTANCE_ID_MAX_LEN + 1))
                .unwrap_err(),
            ResolveError::EmptyId
        );
        assert_eq!(
            map.resolve_or_default(None).unwrap_err(),
            ResolveError::EmptyId
        );
        assert_eq!(
            map.resolve_or_default(Some("")).unwrap_err(),
            ResolveError::EmptyId
        );
        assert_eq!(map.resolve("only").unwrap().register.instance_id, "only");
    }

    #[test]
    fn resolve_does_not_infer_the_only_connected_instance() {
        let map = InstanceMap::new();
        insert_id(&map, "only");
        assert_eq!(
            map.resolve_or_default(None).unwrap_err(),
            ResolveError::EmptyId
        );
        insert_id(&map, "other");
        assert_eq!(
            map.resolve_or_default(None).unwrap_err(),
            ResolveError::EmptyId
        );
    }

    #[test]
    fn resolve_named_id() {
        let map = InstanceMap::new();
        insert_id(&map, "a");
        insert_id(&map, "b");
        assert_eq!(map.resolve("b").unwrap().register.instance_id, "b");
        assert_eq!(
            map.resolve("nope").unwrap_err(),
            ResolveError::UnknownInstance
        );
    }

    #[test]
    fn resolve_or_default_uses_configured_id_only() {
        let map = InstanceMap::new();
        map.set_default_instance(Some("hint".into()));
        assert_eq!(map.default_instance().as_deref(), Some("hint"));
        assert_eq!(
            map.resolve_or_default(None).unwrap_err(),
            ResolveError::UnknownInstance
        );

        insert_id(&map, "a");
        insert_id(&map, "hint");
        assert_eq!(
            map.resolve_or_default(None).unwrap().register.instance_id,
            "hint"
        );
        assert_eq!(
            map.resolve_or_default(Some("a"))
                .unwrap()
                .register
                .instance_id,
            "a"
        );
        assert_eq!(map.resolve("a").unwrap().register.instance_id, "a");
    }

    #[test]
    fn resolve_ignores_empty_or_overlong_default() {
        let map = InstanceMap::new();
        map.set_default_instance(Some(String::new()));
        assert!(map.default_instance().is_none());
        map.set_default_instance(Some("x".repeat(INSTANCE_ID_MAX_LEN + 1)));
        assert!(map.default_instance().is_none());
        insert_id(&map, "only");
        assert_eq!(
            map.resolve_or_default(None).unwrap_err(),
            ResolveError::EmptyId
        );
    }

    #[tokio::test]
    async fn send_and_wait_completes_on_matching_corr() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(4);
        map.insert(register("a"), 4, tx);
        let corr = map.next_corr();
        let wait = map.send_and_wait("a", outbound(corr), REPLY_TIMEOUT);
        tokio::pin!(wait);
        assert!(matches!(
            futures::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        map.push_inbound(
            "a",
            SimToServer {
                corr,
                ..Default::default()
            },
        );
        let reply = wait.await.unwrap();
        assert_eq!(reply.corr, corr);
        assert_eq!(map.try_recv_inbound("a").unwrap().corr, corr);
    }

    #[tokio::test]
    async fn send_and_wait_times_out() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(4);
        map.insert(register("a"), 4, tx);
        let err = map
            .send_and_wait("a", outbound(7), Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err, WaitError::Timeout);
    }

    #[tokio::test]
    async fn send_and_wait_fails_when_instance_drops() {
        let map = InstanceMap::new();
        let (tx, _rx) = mpsc::channel(4);
        let (token, _sink) = map.insert(register("a"), 4, tx);
        let wait = map.send_and_wait("a", outbound(3), REPLY_TIMEOUT);
        tokio::pin!(wait);
        assert!(matches!(
            futures::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        map.remove_if("a", token);
        assert_eq!(wait.await.unwrap_err(), WaitError::Disconnected);
    }

    #[tokio::test]
    async fn send_and_wait_unknown_and_queue_full() {
        let map = InstanceMap::new();
        assert_eq!(
            map.send_and_wait("missing", outbound(1), REPLY_TIMEOUT)
                .await
                .unwrap_err(),
            WaitError::UnknownInstance
        );
        let (tx, _rx) = mpsc::channel(1);
        map.insert(register("a"), 4, tx);
        map.try_send("a", outbound(1)).unwrap();
        assert_eq!(
            map.send_and_wait("a", outbound(2), REPLY_TIMEOUT)
                .await
                .unwrap_err(),
            WaitError::QueueFull
        );
    }

    #[test]
    fn read_mask_defaults_empty_and_can_be_set() {
        let map = InstanceMap::new();
        assert!(map.read_mask("missing").is_none());
        map.set_read_mask("missing", vec!["log".into()]);
        insert_id(&map, "a");
        assert_eq!(map.read_mask("a").unwrap(), Vec::<String>::new());
        map.set_read_mask("a", vec!["log".into(), "goodbye".into()]);
        assert_eq!(
            map.read_mask("a").unwrap(),
            vec!["log".to_string(), "goodbye".to_string()]
        );
    }

    #[test]
    fn inbound_visible_matches_payload_names() {
        let named = [
            (
                SimToServer {
                    payload: Some(register("a").into()),
                    ..Default::default()
                },
                "register",
            ),
            (
                SimToServer {
                    payload: Some(Heartbeat::default().into()),
                    ..Default::default()
                },
                "heartbeat",
            ),
            (
                SimToServer {
                    payload: Some(SnapshotFrame::default().into()),
                    ..Default::default()
                },
                "snapshot",
            ),
            (
                SimToServer {
                    payload: Some(SnapshotError::default().into()),
                    ..Default::default()
                },
                "snapshot_error",
            ),
            (
                SimToServer {
                    payload: Some(LogLine::default().into()),
                    ..Default::default()
                },
                "log",
            ),
            (
                SimToServer {
                    payload: Some(InputAck::default().into()),
                    ..Default::default()
                },
                "input_ack",
            ),
            (
                SimToServer {
                    payload: Some(InputObserved::default().into()),
                    ..Default::default()
                },
                "input_observed",
            ),
            (
                SimToServer {
                    payload: Some(Goodbye::default().into()),
                    ..Default::default()
                },
                "goodbye",
            ),
        ];
        for (msg, name) in named {
            assert_eq!(InstanceMap::inbound_payload_name(&msg), Some(name));
            assert!(InstanceMap::inbound_visible(&[], &msg));
            assert!(InstanceMap::inbound_visible(&[name.to_string()], &msg));
            assert!(!InstanceMap::inbound_visible(&["other".into()], &msg));
        }
        let empty = SimToServer::default();
        assert_eq!(InstanceMap::inbound_payload_name(&empty), None);
        assert!(InstanceMap::inbound_visible(&[], &empty));
        assert!(!InstanceMap::inbound_visible(&["log".into()], &empty));
    }
}
