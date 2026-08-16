use std::net::SocketAddr;
use std::process::ExitCode;

use clap::Parser;
use csm_proxy::InstanceMap;

/// MCP proxy that accepts inbound eBook firmware simulator Session streams.
#[derive(Parser, Debug)]
#[command(name = "crosspoint-simulator-mcp-proxy", version, about)]
struct Args {
    /// Listen address for inbound simulator Session (gRPC, plaintext).
    #[arg(long, env = "CSM_LISTEN", default_value = "127.0.0.1:50051")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let instances = InstanceMap::new();
    match csm_proxy::serve(args.listen, instances).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("failed to listen on {}: {err}", args.listen);
            ExitCode::FAILURE
        }
    }
}
