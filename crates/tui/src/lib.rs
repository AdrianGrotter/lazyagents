//! The lazyagents terminal UI: a client of the daemon.

pub mod app;
pub mod clipboard;
pub mod config;
pub mod connection;
pub mod keymap;
pub mod keys;
pub mod known;
pub mod notify;
mod run;
pub mod ui;

pub use run::run;
