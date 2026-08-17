//! Host-side RPC listener and MCP peer for inbound simulator `Session` streams.

pub mod cli;
pub mod instances;
pub mod logging;
pub mod mcp;
pub mod session;
pub mod spawn;

use std::net::SocketAddr;

use connectrpc::Server;

pub use cli::Args;
pub use instances::{
    INSTANCE_ID_MAX_LEN, InstanceMap, InstanceSnapshot, QUEUE_CAPACITY, REPLY_TIMEOUT,
    ResolveError, TrySendError, WaitError, is_valid_instance_id,
};
pub use logging::{DEFAULT_FILTER, filter_from_env, init_tracing};
pub use mcp::{
    CAPABILITIES_URI, INSTRUCTIONS, McpServer, TOOL_NAMES, serve_mcp_http, serve_mcp_http_listener,
    serve_mcp_stdio,
};
pub use session::SessionService;
pub use spawn::{
    DEFAULT_OBSERVE_WAIT_MS, NEVER_SLEEP_TIMEOUT_MINUTES, SAMPLE_BOOK_EPUB, SAMPLE_BOOK_FILENAME,
    SETTINGS_RELATIVE, SPAWN_WAIT, SpawnConfig, SpawnError, SpawnSupervisor, default_cwd,
    seed_never_sleep_settings, seed_sample_book, spawn_argv,
};

/// Bind and serve inbound `Session` on `addr` until the process exits.
pub async fn serve(
    addr: SocketAddr,
    instances: InstanceMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::info!(%addr, "session listener binding");
    let router = SessionService::new(instances).router();
    Server::new(router).serve(addr).await
}

/// Listen for inbound `Session` and serve one MCP transport until either ends.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    init_tracing();
    let instances = InstanceMap::new();
    args.apply_instance_hints(&instances);
    let spawn = SpawnConfig::from_args(&args);
    tracing::info!(
        listen = %args.listen,
        mcp = args.mcp_http.map(|addr| addr.to_string()).unwrap_or_else(|| "stdio".into()),
        default_instance = args.default_instance.as_deref(),
        simulator = ?spawn.binary,
        "proxy starting"
    );
    let session = serve(args.listen, instances.clone());
    match args.mcp_http {
        Some(mcp_addr) => {
            tokio::select! {
                result = session => result,
                result = serve_mcp_http(mcp_addr, instances, spawn) => result,
            }
        }
        None => {
            tokio::select! {
                result = session => result,
                result = serve_mcp_stdio(instances, spawn) => result,
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
