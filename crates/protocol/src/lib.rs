//! Messages exchanged between the lazyagents daemon and its clients.

mod framing;
mod input;
pub mod lockfile;
mod message;
mod paths;
mod screen;
mod types;

pub use framing::{MAX_MESSAGE_LEN, read_message, write_message};
pub use input::{InputEvent, Key, KeyCode, Modifiers};
pub use message::{ClientMessage, Request, Response, ServerMessage};
pub use paths::StatePaths;
pub use screen::{Cell, Color, Frame, Row, Screen, Style};
pub use types::{
    AgentId, AgentInfo, AgentKind, CreateAgent, DiffScope, GitStat, HookKind, Project, ProjectId,
    Status, Usage, WorktreeInfo, WorktreeRequest,
};

pub const PROTOCOL_VERSION: u32 = 5;

/// How long a client waits for the daemon to stop. It covers the daemon's slowest
/// clean shutdown: waiting out a running process snapshot, then the agents' grace
/// period, then the `ps` readings that verify what is left.
pub const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(20);
