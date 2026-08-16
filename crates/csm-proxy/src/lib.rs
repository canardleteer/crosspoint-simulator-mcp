//! Host-side RPC listener and MCP peer for inbound simulator `Session` streams.

pub mod cli;
pub mod instances;
pub mod mcp;
pub mod session;

use std::net::SocketAddr;

use connectrpc::Server;

pub use cli::Args;
pub use instances::{
    INSTANCE_ID_MAX_LEN, InstanceMap, InstanceSnapshot, QUEUE_CAPACITY, ResolveError, TrySendError,
    is_valid_instance_id,
};
pub use mcp::{McpServer, serve_mcp_http, serve_mcp_http_listener, serve_mcp_stdio};
pub use session::SessionService;

/// Bind and serve inbound `Session` on `addr` until the process exits.
pub async fn serve(
    addr: SocketAddr,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let router = SessionService::new(instances).router();
    Server::new(router).serve(addr).await
}

/// Listen for inbound `Session` and serve one MCP transport until either ends.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let instances = InstanceMap::new();
    args.apply_instance_hints(&instances);
    let session = serve(args.listen, instances.clone());
    match args.mcp_http {
        Some(mcp_addr) => {
            tokio::select! {
                result = session => result,
                result = serve_mcp_http(mcp_addr, instances) => result,
            }
        }
        None => {
            tokio::select! {
                result = session => result,
                result = serve_mcp_stdio(instances) => result,
            }
        }
    }
}

#[cfg(test)]
mod run_tests {
    use std::time::Duration;

    use clap::Parser;

    use super::*;

    #[tokio::test]
    async fn run_http_starts_both_listeners() {
        let args = Args::try_parse_from([
            "crosspoint-simulator-mcp-proxy",
            "--listen",
            "127.0.0.1:0",
            "--mcp-http",
            "127.0.0.1:0",
            "--default-instance",
            "sim-a",
        ])
        .unwrap();
        let handle = tokio::spawn(run(args));
        tokio::time::sleep(Duration::from_millis(80)).await;
        handle.abort();
        let _ = handle.await;
    }
}
