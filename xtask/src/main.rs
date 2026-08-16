//! Workspace maintenance tasks. Run as `cargo xtask <command>`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use clap::{Parser, Subcommand};

/// Line coverage must be strictly greater than this percent.
const MIN_LINE_COVERAGE_PERCENT: f64 = 90.0;

const GENERATED_PATH_REGEX: &str = r"src[/\\]gen(/|\\)|src[/\\]gen_connect(/|\\)";

const INSTALL_HINT: &str = "\
Missing tools to generate a coverage report.

Install:

  cargo install cargo-llvm-cov --locked
  rustup component add llvm-tools-preview
";

#[derive(Parser, Debug)]
#[command(name = "xtask", about = "Workspace maintenance tasks")]
struct Args {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand, Debug)]
enum Task {
    /// Run tests with LLVM coverage and require more than 90% line cover.
    Coverage(CoverageArgs),
    /// Generate C++ protobuf and grpc++ stubs into the simulator submodule.
    GenerateSimCpp,
}

#[derive(clap::Args, Debug)]
struct CoverageArgs {
    /// Also write an HTML report under target/llvm-cov/html.
    #[arg(long)]
    html: bool,
    /// Open the HTML report in the default browser (also writes HTML).
    #[arg(long)]
    open: bool,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is a workspace member")
        .to_path_buf()
}

fn tool_missing() -> bool {
    let llvm_cov = Command::new("cargo")
        .args(["llvm-cov", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true);
    if llvm_cov {
        return true;
    }

    match Command::new("rustup")
        .args(["component", "list", "--installed"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let listed = String::from_utf8_lossy(&out.stdout);
            !listed.lines().any(|line| line.starts_with("llvm-tools"))
        }
        // rustup is the supported install path; if it is absent, llvm-cov
        // may still work from a custom toolchain — try the report anyway.
        _ => false,
    }
}

fn open_default_browser(path: &Path) -> Result<(), String> {
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(&path).status()
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&path)
            .status()
    } else {
        Command::new("xdg-open").arg(&path).status()
    }
    .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("opener exited with {status}"))
    }
}

fn line_percent(json: &serde_json::Value) -> Option<f64> {
    json.get("data")?
        .as_array()?
        .first()?
        .get("totals")?
        .get("lines")?
        .get("percent")?
        .as_f64()
}

fn llvm_cov(root: &Path, report: bool, extra: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).arg("llvm-cov");
    if report {
        cmd.arg("report");
    } else {
        cmd.args(["--workspace", "--exclude", "xtask"]);
    }
    cmd.args(["--ignore-filename-regex", GENERATED_PATH_REGEX]);
    cmd.args(extra);
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run cargo llvm-cov: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo llvm-cov failed ({status})"))
    }
}

fn html_index(out_dir: &Path) -> PathBuf {
    let nested = out_dir.join("html/index.html");
    if nested.is_file() {
        nested
    } else {
        out_dir.join("index.html")
    }
}

fn run_generate_sim_cpp() -> Result<(), String> {
    let root = workspace_root();
    let protos = root.join("protos");
    let template = protos.join("buf.gen.sim-cpp.yaml");
    let out = root.join("crosspoint-simulator/src/sim_grpc/gen");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;

    if Command::new("sh")
        .args(["-c", "command -v grpc_cpp_plugin"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        return Err(
            "grpc_cpp_plugin not found on PATH. Install protobuf-compiler-grpc \
             (and libgrpc++-dev / libprotobuf-dev) or add a local gRPC prefix \
             to PATH, then re-run cargo xtask generate-sim-cpp."
                .into(),
        );
    }

    let buf = buf_tools::buf_bin_path();
    let status = Command::new(&buf)
        .arg("generate")
        .arg("--template")
        .arg(&template)
        .current_dir(&protos)
        .status()
        .map_err(|e| format!("failed to spawn buf generate: {e}"))?;
    if !status.success() {
        return Err(format!("buf generate failed ({status})"));
    }
    println!("wrote C++ Session stubs under {}", out.display());
    Ok(())
}

fn run_coverage(args: CoverageArgs) -> Result<(), String> {
    if tool_missing() {
        return Err(INSTALL_HINT.trim_end().to_string());
    }

    let root = workspace_root();
    let out_dir = root.join("target/llvm-cov");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let json_path = out_dir.join("coverage.json");
    let write_html = args.html || args.open;

    llvm_cov(&root, false, &[])?;
    llvm_cov(
        &root,
        true,
        &[
            "--json",
            "--output-path",
            json_path.to_str().expect("utf-8 coverage path"),
        ],
    )?;
    if write_html {
        llvm_cov(
            &root,
            true,
            &[
                "--html",
                "--output-dir",
                out_dir.to_str().expect("utf-8 html dir"),
            ],
        )?;
    }

    let json_text = fs::read_to_string(&json_path).map_err(|e| e.to_string())?;
    let json: serde_json::Value =
        serde_json::from_str(&json_text).map_err(|e| format!("coverage json: {e}"))?;
    let percent = line_percent(&json)
        .ok_or_else(|| "coverage json missing data[0].totals.lines.percent".to_string())?;

    println!("line coverage: {percent:.2}% (required: > {MIN_LINE_COVERAGE_PERCENT}%)");

    if write_html {
        let html = html_index(&out_dir);
        if !html.is_file() {
            return Err(format!(
                "HTML report was not written under {}",
                out_dir.display()
            ));
        }
        println!("HTML report: {}", html.display());
        if args.open {
            open_default_browser(&html).map_err(|e| {
                format!(
                    "could not open the default browser ({e}). Open {} manually.",
                    html.display()
                )
            })?;
        }
    }

    if percent <= MIN_LINE_COVERAGE_PERCENT {
        return Err(format!(
            "line coverage {percent:.2}% is not over {MIN_LINE_COVERAGE_PERCENT}%"
        ));
    }

    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = match args.command {
        Task::Coverage(coverage) => run_coverage(coverage),
        Task::GenerateSimCpp => run_generate_sim_cpp(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
