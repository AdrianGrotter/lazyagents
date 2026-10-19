//! The lazyagents terminal UI: a client of the daemon.

pub mod app;
pub mod approved;
pub mod clipboard;
pub mod commands;
pub mod config;
pub mod connection;
pub mod current;
pub mod fixed_keys;
pub mod help;
pub mod keymap;
pub mod keys;
pub mod known;
pub mod layout;
pub mod notify;
pub mod project_config;
pub mod review;
mod run;
pub mod ui;

pub use run::run;
