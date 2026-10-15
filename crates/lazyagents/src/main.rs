mod commands;
mod daemon_cmd;
mod hook;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use protocol::StatePaths;

/// A lazygit-style terminal UI for managing coding agents.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the daemon in the foreground
    Daemon,
    /// Forward a Claude Code hook event to the daemon
    #[command(hide = true)]
    Hook { event: String },
    /// List projects and agents
    Ls,
    /// Manage projects
    #[command(subcommand)]
    Project(ProjectCommand),
    /// Start a new agent
    New {
        name: String,
        /// Project name or path; defaults to the project containing the current directory
        #[arg(long)]
        project: Option<String>,
        /// Agent kind; defaults to claude without a command, generic with one
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        /// Command to run
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Stop an agent
    Kill { agent: String },
    /// Restart an agent, resuming its Claude session when it has one
    Restart { agent: String },
    /// Remove an agent
    Rm { agent: String },
    /// Rename an agent
    Rename { agent: String, name: String },
    /// Stop the daemon and every agent
    Shutdown,
}

#[derive(Subcommand)]
pub enum ProjectCommand {
    /// Register a project directory
    Add {
        path: Option<PathBuf>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Unregister a project that has no agents
    Rm { project: String },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    Claude,
    Generic,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| {
                runtime.block_on(async {
                    tui::run(StatePaths::from_env()?, std::env::current_exe()?).await
                })
            }),
        Some(Command::Hook { event }) => {
            hook::run(&event);
            Ok(())
        }
        Some(Command::Daemon) => daemon_cmd::run(),
        Some(command) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(commands::run(command))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "lazyagents: {e:#}");
            ExitCode::FAILURE
        }
    }
}
