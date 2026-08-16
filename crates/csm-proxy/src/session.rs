//! Inbound `SimulatorControlService.Session` handler.

use std::sync::Arc;

use connectrpc::{
    ConnectError, InboundStream, RequestContext, Response, Router, ServiceResult, ServiceStream,
};
use csm_pb_bindings::generated::crosspoint::sim::control::v1alpha1::{
    ServerToSim, SimToServer, sim_to_server,
};
use csm_pb_bindings::rpc::crosspoint::sim::control::v1alpha1::SimulatorControlService;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::instances::{InstanceMap, QUEUE_CAPACITY};

/// connectrpc service that registers streams on [`InstanceMap`].
#[derive(Clone)]
pub struct SessionService {
    instances: InstanceMap,
}

impl SessionService {
    /// Serve `Session` against `instances`.
    pub fn new(instances: InstanceMap) -> Self {
        Self { instances }
    }

    /// Tower router for this service (gRPC / Connect / gRPC-Web).
    pub fn router(self) -> Router {
        Router::new().add_service(Arc::new(self))
    }
}

impl SimulatorControlService for SessionService {
    async fn session(
        &self,
        _ctx: RequestContext,
        mut requests: InboundStream<SimToServer>,
    ) -> ServiceResult<ServiceStream<impl connectrpc::Encodable<ServerToSim> + Send + use<>>> {
        let first = match requests.next().await {
            Some(Ok(msg)) => msg.to_owned_message(),
            Some(Err(err)) => return Err(err),
            None => {
                return Err(ConnectError::invalid_argument(
                    "session opened with no messages",
                ));
            }
        };

        let Some(sim_to_server::Payload::Register(register)) = first.payload else {
            return Err(ConnectError::invalid_argument(
                "first session message must be Register",
            ));
        };
        let register = *register;
        let instance_id = register.instance_id.clone();
        if instance_id.is_empty() || instance_id.len() > 64 {
            return Err(ConnectError::invalid_argument(
                "instance_id must be 1-64 bytes",
            ));
        }

        let (outbound_tx, outbound_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (token, inbound) = self.instances.insert(register, QUEUE_CAPACITY, outbound_tx);
        let instances = self.instances.clone();
        let id_for_loop = instance_id.clone();

        tokio::spawn(async move {
            while let Some(item) = requests.next().await {
                if !instances.owns(&id_for_loop, token) {
                    break;
                }
                let Ok(msg) = item else {
                    break;
                };
                let owned = msg.to_owned_message();
                match &owned.payload {
                    Some(sim_to_server::Payload::Heartbeat(heartbeat)) => {
                        instances.set_heartbeat(&id_for_loop, token, heartbeat.as_ref().clone());
                        inbound.push(owned);
                    }
                    Some(sim_to_server::Payload::Goodbye(_)) => {
                        inbound.push(owned);
                        instances.remove_if(&id_for_loop, token);
                        break;
                    }
                    _ => inbound.push(owned),
                }
            }
            instances.remove_if(&id_for_loop, token);
        });

        let outbound = futures::stream::unfold(outbound_rx, |mut rx| async move {
            rx.recv().await.map(|msg| (Ok(msg), rx))
        });
        Response::stream_ok(outbound)
    }
}
