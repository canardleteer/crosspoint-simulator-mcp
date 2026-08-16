//! Host-side RPC listener for inbound simulator `Session` streams.

pub mod instances;
pub mod session;

use std::net::SocketAddr;

use connectrpc::Server;

pub use instances::{InstanceMap, InstanceSnapshot, QUEUE_CAPACITY, TrySendError};
pub use session::SessionService;

/// Bind and serve inbound `Session` on `addr` until the process exits.
pub async fn serve(
    addr: SocketAddr,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = SessionService::new(instances).router();
    Server::new(router).serve(addr).await
}
