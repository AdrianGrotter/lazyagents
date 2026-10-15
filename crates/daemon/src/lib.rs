//! The lazyagents daemon: owns agent processes, their terminals, and persisted state.

pub mod encode;
pub mod reap;
mod server;
pub mod spawn;
pub mod status;
pub mod store;
pub mod terminal;

pub use server::{Config, run};
