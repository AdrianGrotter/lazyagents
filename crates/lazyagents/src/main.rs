use clap::Parser;

/// A lazygit-style terminal UI for managing coding agents.
#[derive(Parser)]
#[command(version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
