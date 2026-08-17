//! Clap flags for listen, instance selection, and spawn.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

use crate::InstanceMap;

/// MCP proxy that accepts inbound eBook firmware simulator Session streams.
///
/// A session appears when a simulator dials this process, or when
/// `start_instance` executes `--simulator`. Logs go to stderr (`tracing`,
/// `RUST_LOG`; default `csm_proxy=info`).
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

    /// Path of a known prebuilt simulator binary. Executed only by
    /// `start_instance`.
    #[arg(long, env = "CSM_SIMULATOR")]
    pub simulator: Option<PathBuf>,

    /// Extra argv appended before Session flags when `start_instance` runs.
    /// Repeatable. `CSM_SIMULATOR_ARGS` is a comma-separated list.
    #[arg(
        long = "simulator-arg",
        env = "CSM_SIMULATOR_ARGS",
        value_delimiter = ',',
        action = clap::ArgAction::Append,
        allow_hyphen_values = true
    )]
    pub simulator_arg: Vec<String>,

    /// Process default for `start_instance.auto_sleep` when the client omits it.
    /// false (default) seeds never-sleep settings; true keeps firmware's 10-minute idle sleep.
    #[arg(long, env = "CSM_AUTO_SLEEP", default_value_t = false)]
    pub auto_sleep: bool,

    /// Default `observe` timeout in milliseconds when `until_log` or
    /// `until_generation_gt` is set and `wait_ms` is omitted.
    #[arg(long, env = "CSM_OBSERVE_WAIT_MS", default_value_t = crate::spawn::DEFAULT_OBSERVE_WAIT_MS)]
    pub observe_wait_ms: u32,
}

fn parse_instance_id(s: &str) -> Result<String, String> {
    if crate::is_valid_instance_id(s) {
        Ok(s.to_string())
    } else {
        Err("must be 1-64 bytes".into())
    }
}

impl Args {
    /// Apply the default-instance hint. Does not start `--simulator`.
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
        assert!(!args.auto_sleep);
        assert_eq!(args.observe_wait_ms, crate::spawn::DEFAULT_OBSERVE_WAIT_MS);
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
    fn parses_listen_default_and_simulator_hints() {
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
            "--auto-sleep",
            "--observe-wait-ms",
            "2500",
        ])
        .unwrap();
        assert!(args.auto_sleep);
        assert_eq!(args.observe_wait_ms, 2500);
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
        assert!(Args::try_parse_from([
            "crosspoint-simulator-mcp-proxy",
            "--default-instance",
            &"x".repeat(65)
        ])
        .is_err());
    }
}
