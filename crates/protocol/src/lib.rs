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
    AgentId, AgentInfo, AgentKind, CreateAgent, GitStat, HookKind, Project, ProjectId, Status,
    WorktreeInfo, WorktreeRequest,
};

pub const PROTOCOL_VERSION: u32 = 2;
