//! Messages exchanged between the lazyagents daemon and its clients.

mod input;
mod screen;
mod types;

pub use input::{InputEvent, Key, KeyCode, Modifiers};
pub use screen::{Cell, Color, Frame, Row, Screen, Style};
pub use types::{AgentId, AgentInfo, AgentKind, CreateAgent, HookKind, Project, ProjectId, Status};

pub const PROTOCOL_VERSION: u32 = 1;
