//! Clap flags for listen, instance selection, and later spawn hints.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use crate::InstanceMap;

/// MCP proxy that accepts inbound eBook firmware simulator Session streams.
///
/// A session appears when a simulator dials this process. `--simulator` and
/// `--simulator-arg` are reserved for a later spawn path and are not
/// executed.
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(name = "crosspoint-simulator-mcp-proxy", version, about)]
pub struct Args {
    /// Listen address for inbound simulator Session (gRPC, plaintext).
    #[arg(long, env = "CSM_LISTEN", default_value = "127.0.0.1:50051")]
    pub listen: SocketAddr,

    /// Streamable HTTP MCP listen address. When set, MCP uses HTTP
    /// instead of stdio. `--mcp-http` alone binds `127.0.0.1:8765`.
    #[arg(
        long,
        env = "CSM_MCP_HTTP",
        num_args = 0..=1,
        default_missing_value = "127.0.0.1:8765",
        value_name = "ADDR"
    )]
    pub mcp_http: Option<SocketAddr>,

    /// Instance id a later tool may use when it does not pass one.
    /// Must be 1–64 bytes. Never inferred from connection count.
    #[arg(long, env = "CSM_DEFAULT_INSTANCE", value_parser = parse_instance_id)]
    pub default_instance: Option<String>,

    /// Path of a known simulator binary for a later spawn. Not executed.
    #[arg(long, env = "CSM_SIMULATOR")]
    pub simulator: Option<PathBuf>,

    /// Extra argv for a later spawn. Repeatable. Not executed.
    /// `CSM_SIMULATOR_ARGS` is a comma-separated list.
    #[arg(
        long = "simulator-arg",
        env = "CSM_SIMULATOR_ARGS",
        value_delimiter = ',',
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    pub simulator_arg: Vec<String>,
}

fn parse_instance_id(s: &str) -> Result<String, String> {
    if crate::is_valid_instance_id(s) {
        Ok(s.to_string())
    } else {
        Err("must be 1-64 bytes".into())
    }
}

impl Args {
    /// Apply the default-instance hint. Does not spawn `--simulator`.
    pub fn apply_instance_hints(&self, instances: &InstanceMap) {
        instances.set_default_instance(self.default_instance.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn defaults_listen_and_leaves_spawn_unset() {
        let args = Args::try_parse_from(["crosspoint-simulator-mcp-proxy"]).unwrap();
        assert_eq!(args.listen, "127.0.0.1:50051".parse().unwrap());
        assert!(args.mcp_http.is_none());
        assert!(args.default_instance.is_none());
        assert!(args.simulator.is_none());
        assert!(args.simulator_arg.is_empty());
    }

    #[test]
    fn mcp_http_flag_defaults_and_overrides_addr() {
        let bare = Args::try_parse_from(["crosspoint-simulator-mcp-proxy", "--mcp-http"]).unwrap();
        assert_eq!(bare.mcp_http, Some("127.0.0.1:8765".parse().unwrap()));
        let named = Args::try_parse_from([
            "crosspoint-simulator-mcp-proxy",
            "--mcp-http",
            "127.0.0.1:0",
        ])
        .unwrap();
        assert_eq!(named.mcp_http, Some("127.0.0.1:0".parse().unwrap()));
    }

    #[test]
    fn parses_listen_default_and_unused_simulator_hints() {
        let args = Args::try_parse_from([
            "crosspoint-simulator-mcp-proxy",
            "--listen",
            "127.0.0.1:0",
            "--default-instance",
            "sim-a",
            "--simulator",
            "/opt/sim",
            "--simulator-arg=--headless",
            "--simulator-arg=--foo",
        ])
        .unwrap();
        assert_eq!(args.listen, "127.0.0.1:0".parse().unwrap());
        assert_eq!(args.default_instance.as_deref(), Some("sim-a"));
        assert_eq!(
            args.simulator.as_deref(),
            Some(std::path::Path::new("/opt/sim"))
        );
        assert_eq!(
            args.simulator_arg,
            vec!["--headless".to_string(), "--foo".to_string()]
        );
        let map = InstanceMap::new();
        args.apply_instance_hints(&map);
        assert_eq!(map.default_instance().as_deref(), Some("sim-a"));
    }

    #[test]
    fn rejects_empty_or_overlong_default_instance() {
        assert!(
            Args::try_parse_from(["crosspoint-simulator-mcp-proxy", "--default-instance", ""])
                .is_err()
        );
        assert!(
            Args::try_parse_from([
                "crosspoint-simulator-mcp-proxy",
                "--default-instance",
                &"x".repeat(65)
            ])
            .is_err()
        );
    }
}
