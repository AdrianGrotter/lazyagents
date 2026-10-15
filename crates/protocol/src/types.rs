use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(pub Uuid);

impl AgentId {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(pub Uuid);

impl ProjectId {
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Claude,
    Generic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Starting,
    Working,
    NeedsInput,
    Idle,
    Exited(i32),
    Stopped,
}

impl Status {
    pub fn is_running(self) -> bool {
        !matches!(self, Status::Exited(_) | Status::Stopped)
    }

    pub fn label(self) -> String {
        match self {
            Status::Starting => "starting".into(),
            Status::Working => "working".into(),
            Status::NeedsInput => "needs input".into(),
            Status::Idle => "idle".into(),
            Status::Exited(code) => format!("exited ({code})"),
            Status::Stopped => "stopped".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub project: ProjectId,
    pub name: String,
    pub kind: AgentKind,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgent {
    pub project: ProjectId,
    pub name: String,
    pub kind: AgentKind,
    pub command: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Notification,
    Stop,
}

impl HookKind {
    pub const ALL: [HookKind; 6] = [
        HookKind::SessionStart,
        HookKind::UserPromptSubmit,
        HookKind::PreToolUse,
        HookKind::PostToolUse,
        HookKind::Notification,
        HookKind::Stop,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HookKind::SessionStart => "SessionStart",
            HookKind::UserPromptSubmit => "UserPromptSubmit",
            HookKind::PreToolUse => "PreToolUse",
            HookKind::PostToolUse => "PostToolUse",
            HookKind::Notification => "Notification",
            HookKind::Stop => "Stop",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_names_round_trip() {
        for kind in HookKind::ALL {
            assert_eq!(HookKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(HookKind::from_name("SubagentStop"), None);
    }

    #[test]
    fn status_labels_and_running() {
        assert_eq!(Status::NeedsInput.label(), "needs input");
        assert_eq!(Status::Exited(3).label(), "exited (3)");
        assert!(Status::Idle.is_running());
        assert!(!Status::Exited(0).is_running());
        assert!(!Status::Stopped.is_running());
    }

    #[test]
    fn ids_display_as_uuids() {
        let id = AgentId::random();
        assert_eq!(id.to_string(), id.0.to_string());
        assert_ne!(AgentId::random(), id);
    }
}
