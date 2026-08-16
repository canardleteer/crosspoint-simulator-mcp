use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "crosspoint-simulator-mcp-proxy", version, about)]
struct Args {}

fn main() {
    let _args = Args::parse();
}
