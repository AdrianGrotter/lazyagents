//! The lazyagents terminal UI: a client of the daemon.

pub mod app;
pub mod connection;
pub mod keys;
mod run;
pub mod ui;

pub use run::run;
