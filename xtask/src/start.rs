//! Build the proxy (and optional firmware) and exec it for local testing.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clap::{Args, ValueEnum};

/// MCP transport used by `start-csm-proxy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum McpMode {
    /// Streamable HTTP at `/mcp` (the Cursor / agent test path).
    #[default]
    Http,
    /// JSON-RPC on stdout. `cargo` progress stays on stderr; this task
    /// then execs the proxy so xtask itself never writes to stdout.
    Stdio,
}

/// Build `csm-proxy` if needed and start it the way local Session tests do.
#[derive(Args, Debug)]
pub struct StartCsmProxyArgs {
    /// MCP transport. `http` is how this repository is tested with Cursor.
    #[arg(long, env = "CSM_MCP_MODE", default_value = "http")]
    pub mode: McpMode,

    /// Firmware repo directory, or a prebuilt `program` path.
    #[arg(long, env = "CSM_FIRMWARE")]
    pub firmware: Option<PathBuf>,

    /// PlatformIO env used to find or build `{firmware}/.pio/build/<board>/program`.
    #[arg(long, env = "CSM_BOARD", default_value = "simulator")]
    pub board: String,

    /// Local protobuf 35 / grpc++ 1.83 prefix (`lib/pkgconfig`) for `pio` only.
    /// Does not set `LD_LIBRARY_PATH`; use `--ld-library-path`.
    #[arg(long, env = "CSM_GRPC_PREFIX")]
    pub grpc_prefix: Option<PathBuf>,

    /// gRPC Session listen address.
    #[arg(long, env = "CSM_LISTEN", default_value = "127.0.0.1:50051")]
    pub listen: String,

    /// Streamable HTTP MCP listen address (http mode only).
    #[arg(long, env = "CSM_MCP_HTTP", default_value = "127.0.0.1:8765")]
    pub mcp_http: String,

    /// Set `DISPLAY` on the exec'd proxy only. Inherited from the caller if omitted.
    #[arg(long, env = "CSM_DISPLAY")]
    pub display: Option<String>,

    /// Prepend to `LD_LIBRARY_PATH` on the exec'd proxy only. Repeatable.
    /// Inherited from the caller if omitted. Does not invent a grpc++ prefix.
    #[arg(long, env = "CSM_LD_LIBRARY_PATH", value_delimiter = ':')]
    pub ld_library_path: Vec<PathBuf>,

    /// Run `pio run -e <board>` even when `program` already exists.
    #[arg(long)]
    pub build_firmware: bool,

    /// Extra argv forwarded to the proxy (place after `--`).
    #[arg(last = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

/// Resolved paths used to exec the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPlan {
    pub proxy_bin: PathBuf,
    pub listen: String,
    pub mode: McpMode,
    pub mcp_http: String,
    pub simulator: Option<PathBuf>,
    pub grpc_prefix: Option<PathBuf>,
    pub display: Option<String>,
    pub ld_library_path: Vec<PathBuf>,
    pub extra: Vec<String>,
}


/// `program` path, or the file itself when `firmware` is already a binary.
pub fn program_path(firmware: &Path, board: &str) -> PathBuf {
    if firmware.is_file() {
        firmware.to_path_buf()
    } else {
        firmware
            .join(".pio")
            .join("build")
            .join(board)
            .join("program")
    }
}

/// Prepend `dirs` to an existing `LD_LIBRARY_PATH` value.
pub fn prepend_ld_library_path(dirs: &[PathBuf], current: Option<&str>) -> OsString {
    let mut parts: Vec<OsString> = dirs.iter().map(|p| p.as_os_str().to_owned()).collect();
    if let Some(old) = current.filter(|s| !s.is_empty()) {
        parts.push(OsString::from(old));
    }
    let mut value = OsString::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            value.push(":");
        }
        value.push(part);
    }
    value
}

pub fn proxy_bin(root: &Path) -> PathBuf {
    let target = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target"));
    let mut bin = target.join("debug").join("crosspoint-simulator-mcp-proxy");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    bin
}

pub fn proxy_argv(plan: &StartPlan) -> Vec<OsString> {
    let mut argv = vec![
        plan.proxy_bin.as_os_str().to_owned(),
        OsString::from("--listen"),
        OsString::from(&plan.listen),
    ];
    if plan.mode == McpMode::Http {
        argv.push(OsString::from("--mcp-http"));
        argv.push(OsString::from(&plan.mcp_http));
    }
    if let Some(sim) = &plan.simulator {
        argv.push(OsString::from("--simulator"));
        argv.push(sim.as_os_str().to_owned());
    }
    argv.extend(plan.extra.iter().map(OsString::from));
    argv
}

fn log(msg: impl std::fmt::Display) {
    eprintln!("xtask: {msg}");
}

fn run_status(mut cmd: Command, what: &str) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|err| format!("failed to spawn {what}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

fn build_proxy(root: &Path) -> Result<PathBuf, String> {
    log("building csm-proxy");
    run_status(
        {
            let mut cmd = Command::new("cargo");
            cmd.args(["build", "-p", "csm-proxy"])
                .current_dir(root)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            cmd
        },
        "cargo build -p csm-proxy",
    )?;
    let bin = proxy_bin(root);
    if !bin.is_file() {
        return Err(format!(
            "proxy binary missing after build: {}",
            bin.display()
        ));
    }
    Ok(bin)
}

fn build_firmware(firmware: &Path, board: &str, grpc_prefix: Option<&Path>) -> Result<(), String> {
    log(format!(
        "building firmware env {board} in {}",
        firmware.display()
    ));
    let mut cmd = Command::new("pio");
    cmd.args(["run", "-e", board])
        .current_dir(firmware)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(prefix) = grpc_prefix {
        let pc = prefix.join("lib").join("pkgconfig");
        let old = env::var("PKG_CONFIG_PATH").unwrap_or_default();
        let merged = if old.is_empty() {
            pc.display().to_string()
        } else {
            format!("{}:{old}", pc.display())
        };
        cmd.env("PKG_CONFIG_PATH", merged);
    }
    run_status(cmd, "pio run")
}

fn resolve_simulator(
    args: &StartCsmProxyArgs,
    grpc_prefix: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let firmware = match &args.firmware {
        Some(path) => path.clone(),
        None => {
            log("no --firmware / CSM_FIRMWARE; start_instance will be unset");
            return Ok(None);
        }
    };
    if firmware.is_file() {
        return Ok(Some(firmware));
    }
    if !firmware.is_dir() {
        return Err(format!("firmware path not found: {}", firmware.display()));
    }
    let program = program_path(&firmware, &args.board);
    if args.build_firmware || !program.is_file() {
        build_firmware(&firmware, &args.board, grpc_prefix)?;
    }
    if !program.is_file() {
        return Err(format!(
            "firmware program missing: {} (pio env {})",
            program.display(),
            args.board
        ));
    }
    log(format!("simulator {}", program.display()));
    Ok(Some(program))
}

pub fn plan(root: &Path, args: StartCsmProxyArgs) -> Result<StartPlan, String> {
    if let Some(prefix) = &args.grpc_prefix {
        log(format!("grpc prefix {} (pio PKG_CONFIG_PATH only)", prefix.display()));
    }
    let proxy_bin = build_proxy(root)?;
    let simulator = resolve_simulator(&args, args.grpc_prefix.as_deref())?;
    Ok(StartPlan {
        proxy_bin,
        listen: args.listen,
        mode: args.mode,
        mcp_http: args.mcp_http,
        simulator,
        grpc_prefix: args.grpc_prefix,
        display: args.display.filter(|value| !value.is_empty()),
        ld_library_path: args.ld_library_path,
        extra: args.extra,
    })
}

pub fn exec_proxy(plan: &StartPlan) -> Result<(), String> {
    let argv = proxy_argv(plan);
    log(format!(
        "exec {} ({})",
        plan.proxy_bin.display(),
        match plan.mode {
            McpMode::Http => format!("mcp http://{}/mcp", plan.mcp_http),
            McpMode::Stdio => "mcp stdio".into(),
        }
    ));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(display) = &plan.display {
        cmd.env("DISPLAY", display);
    }
    if !plan.ld_library_path.is_empty() {
        let current = env::var("LD_LIBRARY_PATH").ok();
        cmd.env(
            "LD_LIBRARY_PATH",
            prepend_ld_library_path(&plan.ld_library_path, current.as_deref()),
        );
        if cfg!(target_os = "macos") {
            let current = env::var("DYLD_LIBRARY_PATH").ok();
            cmd.env(
                "DYLD_LIBRARY_PATH",
                prepend_ld_library_path(&plan.ld_library_path, current.as_deref()),
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        return Err(format!("exec proxy failed: {err}"));
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .map_err(|err| format!("failed to start proxy: {err}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("proxy exited ({status})"))
        }
    }
}

pub fn run(root: PathBuf, args: StartCsmProxyArgs) -> Result<(), String> {
    let planned = plan(&root, args)?;
    exec_proxy(&planned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_path_uses_pio_layout_for_a_directory() {
        let dir = PathBuf::from("/tmp/fw");
        assert_eq!(
            program_path(&dir, "simulator"),
            PathBuf::from("/tmp/fw/.pio/build/simulator/program")
        );
    }

    #[test]
    fn proxy_argv_http_includes_simulator_and_extra() {
        let plan = StartPlan {
            proxy_bin: PathBuf::from("/opt/csm-proxy"),
            listen: "127.0.0.1:50051".into(),
            mode: McpMode::Http,
            mcp_http: "127.0.0.1:8765".into(),
            simulator: Some(PathBuf::from("/opt/program")),
            grpc_prefix: None,
            display: None,
            ld_library_path: vec![],
            extra: vec!["--auto-sleep".into()],
        };
        let argv: Vec<String> = proxy_argv(&plan)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            argv,
            vec![
                "/opt/csm-proxy",
                "--listen",
                "127.0.0.1:50051",
                "--mcp-http",
                "127.0.0.1:8765",
                "--simulator",
                "/opt/program",
                "--auto-sleep",
            ]
        );
    }

    #[test]
    fn proxy_argv_stdio_omits_mcp_http() {
        let plan = StartPlan {
            proxy_bin: PathBuf::from("csm"),
            listen: "127.0.0.1:9".into(),
            mode: McpMode::Stdio,
            mcp_http: "127.0.0.1:8765".into(),
            simulator: None,
            grpc_prefix: None,
            display: None,
            ld_library_path: vec![],
            extra: vec![],
        };
        let argv: Vec<String> = proxy_argv(&plan)
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, vec!["csm", "--listen", "127.0.0.1:9"]);
        assert!(!argv.iter().any(|a| a == "--mcp-http"));
    }

    #[test]
    fn prepend_ld_library_path_keeps_existing_entries() {
        let dirs = [PathBuf::from("/opt/grpc/lib")];
        assert_eq!(
            prepend_ld_library_path(&dirs, None).to_string_lossy(),
            "/opt/grpc/lib"
        );
        assert_eq!(
            prepend_ld_library_path(&dirs, Some("/usr/lib")).to_string_lossy(),
            "/opt/grpc/lib:/usr/lib"
        );
    }
}
