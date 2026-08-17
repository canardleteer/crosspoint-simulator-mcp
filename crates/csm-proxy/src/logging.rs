//! Process logs on stderr via `tracing` and `RUST_LOG`.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;

/// Used when `RUST_LOG` is unset or empty.
pub const DEFAULT_FILTER: &str = "csm_proxy=info";

/// Build the filter from an optional `RUST_LOG` spec.
pub fn filter_from_env(rust_log: Option<&str>) -> EnvFilter {
    match rust_log {
        Some(spec) if !spec.is_empty() => EnvFilter::new(spec),
        _ => EnvFilter::new(DEFAULT_FILTER),
    }
}

/// Install a stderr subscriber. Safe to call more than once.
///
/// Reads `RUST_LOG`. When it is unset or empty, uses [`DEFAULT_FILTER`]
/// so this process logs without requiring the operator to export it.
/// Writes only to stderr so stdio MCP keeps stdout as JSON-RPC.
pub fn init_tracing() {
    let spec = std::env::var("RUST_LOG").ok();
    let _ = fmt()
        .with_env_filter(filter_from_env(spec.as_deref()))
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_uses_default_crate_info() {
        let unset = filter_from_env(None).to_string();
        let empty = filter_from_env(Some("")).to_string();
        assert!(
            unset.contains("csm_proxy") && unset.contains("info"),
            "{unset}"
        );
        assert_eq!(unset, empty);
        assert_eq!(DEFAULT_FILTER, "csm_proxy=info");
    }

    #[test]
    fn rust_log_overrides_default() {
        let filter = filter_from_env(Some("csm_proxy=debug,rmcp=warn")).to_string();
        assert!(
            filter.contains("csm_proxy") && filter.contains("debug"),
            "{filter}"
        );
        assert!(
            filter.contains("rmcp") && filter.contains("warn"),
            "{filter}"
        );
    }

    #[test]
    fn init_tracing_is_idempotent() {
        init_tracing();
        init_tracing();
    }
}
