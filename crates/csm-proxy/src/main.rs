use std::process::ExitCode;

use clap::Parser;
use csm_proxy::Args;

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    match csm_proxy::run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}
