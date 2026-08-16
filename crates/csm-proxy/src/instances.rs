//! Connected simulator instances keyed by `Register.instance_id`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    Heartbeat, Register, ServerToSim, SimToServer,
};
use tokio::sync::mpsc;

/// Capacity of each per-instance inbound and outbound queue.
pub const QUEUE_CAPACITY: usize = 32;

/// Why [`InstanceMap::try_send`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendError {
    /// No connected instance with that id.
    UnknownInstance,
    /// Outbound queue is full; a stuck peer must not block the caller.
    QueueFull,
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
}

/// Shared map of connected simulator sessions.
#[derive(Clone)]
pub struct InstanceMap {
    inner: Arc<Mutex<HashMap<String, Instance>>>,
    next_token: Arc<AtomicU64>,
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
        let id = register.instance_id.clone();
        let inst = Instance {
            token,
            register,
            last_heartbeat: None,
            inbound,
            outbound_tx,
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

    /// Pop one inbound envelope if present (tests and later MCP observe).
    pub fn try_recv_inbound(&self, instance_id: &str) -> Option<SimToServer> {
        self.inner
            .lock()
            .expect("instance map lock")
            .get(instance_id)
            .and_then(|inst| inst.inbound.try_recv())
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
    use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::ShutdownRequest;

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
}
