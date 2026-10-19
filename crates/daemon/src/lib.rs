//! The lazyagents daemon: owns agent processes, their terminals, and persisted state.

pub mod claude_settings;
pub mod direnv;
pub mod encode;
pub mod git;
mod lock;
mod outbox;
pub mod reap;
mod server;
pub mod spawn;
pub mod status;
pub mod store;
pub mod terminal;

pub use server::{Config, Timings, run, run_with_readings};
pub use spawn::find_in_path;
