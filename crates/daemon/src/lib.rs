//! The lazyagents daemon: owns agent processes, their terminals, and persisted state.

pub mod claude_settings;
pub mod direnv;
pub mod encode;
pub mod git;
mod lock;
pub mod reap;
mod server;
pub mod spawn;
pub mod status;
pub mod store;
pub mod terminal;

pub use server::{Config, run};
pub use spawn::find_in_path;
