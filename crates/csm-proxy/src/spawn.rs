//! Start and reap a known prebuilt simulator binary.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use tokio::process::{Child, Command};

use crate::cli::Args;
use crate::is_valid_instance_id;

/// How long `start_instance` waits for `Register` by default.
pub const SPAWN_WAIT: Duration = Duration::from_secs(15);

/// Operator-configured binary and Session listen address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnConfig {
    /// Prebuilt firmware `program`. None means spawn is not configured.
    pub binary: Option<PathBuf>,
    /// Extra argv from `--simulator-arg`. Never supplied by the MCP client.
    pub extra_args: Vec<String>,
    /// `--listen` address passed as `--sim-grpc-addr`.
    pub listen: SocketAddr,
    /// How long to wait for the instance to register.
    pub wait: Duration,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            binary: None,
            extra_args: Vec::new(),
            listen: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 50051)),
            wait: SPAWN_WAIT,
        }
    }
}

impl SpawnConfig {
    /// Build from clap flags. Does not start a process.
    pub fn from_args(args: &Args) -> Self {
        Self {
            binary: args.simulator.clone(),
            extra_args: args.simulator_arg.clone(),
            listen: args.listen,
            wait: SPAWN_WAIT,
        }
    }

    /// True when `--simulator` / `CSM_SIMULATOR` named a binary.
    pub fn is_configured(&self) -> bool {
        self.binary.is_some()
    }
}

/// Why starting or tracking a child failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// `--simulator` was not set.
    NotConfigured,
    /// `instance_id` is empty or longer than 64 bytes.
    InvalidId,
    /// That id is already connected.
    AlreadyConnected,
    /// That id was already started by this process and is still alive.
    AlreadySpawned,
    /// `std::process::Command` failed.
    SpawnFailed(String),
    /// Child exited before `Register`.
    ExitedBeforeRegister,
    /// `Register` did not arrive in time.
    RegisterTimeout,
}

impl SpawnError {
    /// MCP tool error text.
    pub fn as_str(&self) -> String {
        match self {
            Self::NotConfigured => {
                "spawn is not configured; set --simulator / CSM_SIMULATOR".into()
            }
            Self::InvalidId => "instance_id is required (1-64 bytes)".into(),
            Self::AlreadyConnected => "instance is already connected".into(),
            Self::AlreadySpawned => "instance is already spawned".into(),
            Self::SpawnFailed(message) => format!("failed to start simulator: {message}"),
            Self::ExitedBeforeRegister => "simulator exited before register".into(),
            Self::RegisterTimeout => "timed out waiting for register".into(),
        }
    }
}

/// Session argv this server always controls.
pub fn spawn_argv(
    binary: &Path,
    listen: SocketAddr,
    instance_id: &str,
    headless: bool,
    extra_args: &[String],
) -> Vec<String> {
    let mut argv = vec![binary.to_string_lossy().into_owned()];
    argv.extend(extra_args.iter().cloned());
    argv.push("--sim-grpc".into());
    argv.push("--sim-grpc-addr".into());
    argv.push(listen.to_string());
    argv.push("--sim-instance-id".into());
    argv.push(instance_id.to_string());
    if headless {
        argv.push("--sim-headless".into());
    }
    argv
}

/// Per-instance working directory under `$TMPDIR` when the tool omits `cwd`.
pub fn default_cwd(instance_id: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push("csm-spawn");
    dir.push(safe_dir_name(instance_id));
    dir
}

fn safe_dir_name(id: &str) -> String {
    if !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return id.to_string();
    }
    id.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

struct Inner {
    children: HashMap<String, Child>,
    starting: HashSet<String>,
}

/// Tracks children started by `start_instance`.
pub struct SpawnSupervisor {
    config: SpawnConfig,
    inner: Mutex<Inner>,
}

impl SpawnSupervisor {
    /// Supervisor for `config`. Does not start a process.
    pub fn new(config: SpawnConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(Inner {
                children: HashMap::new(),
                starting: HashSet::new(),
            }),
        }
    }

    /// Spawn is available when a binary was configured.
    pub fn configured(&self) -> bool {
        self.config.is_configured()
    }

    /// Configured wait for `Register`.
    pub fn wait(&self) -> Duration {
        self.config.wait
    }

    /// True when this process started `instance_id` and the child is still up.
    pub fn is_alive(&self, instance_id: &str) -> bool {
        let mut inner = self.inner.lock().expect("spawn lock");
        child_alive(&mut inner, instance_id)
    }

    /// Reserve `instance_id`, start the binary, and store the child.
    pub async fn start(
        &self,
        instance_id: &str,
        headless: bool,
        cwd: Option<&Path>,
        already_connected: bool,
    ) -> Result<u32, SpawnError> {
        if !is_valid_instance_id(instance_id) {
            return Err(SpawnError::InvalidId);
        }
        let binary = self
            .config
            .binary
            .as_ref()
            .ok_or(SpawnError::NotConfigured)?;
        if already_connected {
            return Err(SpawnError::AlreadyConnected);
        }
        {
            let mut inner = self.inner.lock().expect("spawn lock");
            if inner.starting.contains(instance_id) || child_alive(&mut inner, instance_id) {
                return Err(SpawnError::AlreadySpawned);
            }
            inner.starting.insert(instance_id.to_string());
        }

        let workdir = match cwd {
            Some(path) => path.to_path_buf(),
            None => default_cwd(instance_id),
        };
        if let Err(err) = std::fs::create_dir_all(&workdir) {
            self.clear_starting(instance_id);
            return Err(SpawnError::SpawnFailed(err.to_string()));
        }

        let argv = spawn_argv(
            binary,
            self.config.listen,
            instance_id,
            headless,
            &self.config.extra_args,
        );
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                self.clear_starting(instance_id);
                return Err(SpawnError::SpawnFailed(err.to_string()));
            }
        };
        let pid = child.id().unwrap_or(0);
        {
            let mut inner = self.inner.lock().expect("spawn lock");
            inner.starting.remove(instance_id);
            inner.children.insert(instance_id.to_string(), child);
        }
        Ok(pid)
    }

    /// SIGTERM (then kill) a child this process started.
    pub async fn reap(&self, instance_id: &str) {
        let mut child = {
            let mut inner = self.inner.lock().expect("spawn lock");
            inner.starting.remove(instance_id);
            inner.children.remove(instance_id)
        };
        let Some(child) = child.as_mut() else {
            return;
        };
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }

    /// Reap every child started by this process.
    pub async fn reap_all(&self) {
        let ids: Vec<String> = {
            let inner = self.inner.lock().expect("spawn lock");
            inner.children.keys().cloned().collect()
        };
        for id in ids {
            self.reap(&id).await;
        }
    }

    fn clear_starting(&self, instance_id: &str) {
        self.inner
            .lock()
            .expect("spawn lock")
            .starting
            .remove(instance_id);
    }
}

fn child_alive(inner: &mut Inner, instance_id: &str) -> bool {
    let Some(child) = inner.children.get_mut(instance_id) else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => {
            inner.children.remove(instance_id);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn argv_puts_session_flags_after_operator_extra() {
        let argv = spawn_argv(
            Path::new("/opt/sim"),
            "127.0.0.1:50051".parse().unwrap(),
            "e2e-a",
            true,
            &["--foo".into()],
        );
        assert_eq!(
            argv,
            vec![
                "/opt/sim",
                "--foo",
                "--sim-grpc",
                "--sim-grpc-addr",
                "127.0.0.1:50051",
                "--sim-instance-id",
                "e2e-a",
                "--sim-headless",
            ]
        );
        let shown = spawn_argv(
            Path::new("/opt/sim"),
            "10.0.0.1:9".parse().unwrap(),
            "win",
            false,
            &[],
        );
        assert!(!shown.iter().any(|a| a == "--sim-headless"));
        assert_eq!(shown[3], "10.0.0.1:9");
    }

    #[test]
    fn default_cwd_sanitizes_unsafe_ids() {
        let safe = default_cwd("e2e-spawn");
        assert!(safe.ends_with("e2e-spawn"));
        let encoded = default_cwd("../x");
        assert!(encoded.ends_with("2e2e2f78"));
    }

    #[test]
    fn from_args_copies_operator_hints() {
        let args = Args::try_parse_from([
            "crosspoint-simulator-mcp-proxy",
            "--listen",
            "127.0.0.1:9",
            "--simulator",
            "/opt/sim",
            "--simulator-arg=--foo",
        ])
        .unwrap();
        let cfg = SpawnConfig::from_args(&args);
        assert_eq!(cfg.binary.as_deref(), Some(Path::new("/opt/sim")));
        assert_eq!(cfg.extra_args, vec!["--foo".to_string()]);
        assert_eq!(cfg.listen, "127.0.0.1:9".parse().unwrap());
        assert!(cfg.is_configured());
        assert!(!SpawnConfig::default().is_configured());
    }

    #[tokio::test]
    async fn start_rejects_unset_binary_and_bad_id() {
        let supervisor = SpawnSupervisor::new(SpawnConfig::default());
        assert_eq!(
            supervisor.start("ok", true, None, false).await.unwrap_err(),
            SpawnError::NotConfigured
        );
        let supervisor = SpawnSupervisor::new(SpawnConfig {
            binary: Some(PathBuf::from("/opt/sim")),
            ..SpawnConfig::default()
        });
        assert_eq!(
            supervisor.start("", true, None, false).await.unwrap_err(),
            SpawnError::InvalidId
        );
        assert_eq!(
            supervisor.start("ok", true, None, true).await.unwrap_err(),
            SpawnError::AlreadyConnected
        );
    }

    #[tokio::test]
    async fn start_times_out_path_reaps_a_child_that_never_registers() {
        let supervisor = SpawnSupervisor::new(SpawnConfig {
            binary: Some(PathBuf::from("/bin/sh")),
            extra_args: vec!["-c".into(), "sleep 30".into()],
            wait: Duration::from_millis(50),
            ..SpawnConfig::default()
        });
        let pid = supervisor
            .start("sleepy", true, None, false)
            .await
            .expect("sleep starts");
        assert!(pid > 0);
        assert!(supervisor.is_alive("sleepy"));
        assert_eq!(
            supervisor
                .start("sleepy", true, None, false)
                .await
                .unwrap_err(),
            SpawnError::AlreadySpawned
        );
        supervisor.reap("sleepy").await;
        assert!(!supervisor.is_alive("sleepy"));
    }
}
