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
    pub worktree: Option<Box<WorktreeInfo>>,
    pub last_error: Option<String>,
    /// The Claude session's title, from the terminal title Claude Code sets.
    pub title: Option<String>,
    /// What a Claude agent that needs input is waiting for, such as the tool call it
    /// asks permission for; `None` once it no longer needs input.
    pub waiting_for: Option<String>,
    /// What a Claude agent's status line last reported; `None` until it reports.
    pub usage: Option<Box<Usage>>,
}

/// A Claude session's model, cost and limits, from the JSON Claude Code gives its status line.
/// Each field is `None` when Claude left it out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub model: Option<String>,
    /// The session's estimated cost in millionths of a US dollar.
    pub cost_micros: Option<u64>,
    pub context_percent: Option<u8>,
    /// How much of the plan's 5-hour and 7-day limits is used, in percent.
    pub five_hour_percent: Option<u8>,
    pub seven_day_percent: Option<u8>,
    pub pr_number: Option<u32>,
    /// `approved`, `pending`, `changes_requested` or `draft`.
    pub pr_review: Option<String>,
}

impl Usage {
    /// `Opus · 23% context · $0.42`, leaving out what is unknown.
    pub fn summary(&self) -> String {
        let cost = self
            .cost_micros
            .map(|micros| format!("${:.2}", micros as f64 / 1_000_000.0));
        [
            self.model.clone(),
            self.context_percent.map(|p| format!("{p}% context")),
            cost,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub base: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRequest {
    pub base: Option<String>,
}

/// Which changes a `GitDiff` shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiffScope {
    /// Committed and uncommitted changes since the comparison point.
    #[default]
    All,
    /// Changes not committed yet: the working tree against `HEAD`.
    Uncommitted,
    /// Commits since the comparison point, without uncommitted changes.
    Committed,
}

impl DiffScope {
    pub fn label(self) -> &'static str {
        match self {
            DiffScope::All => "all changes",
            DiffScope::Uncommitted => "uncommitted",
            DiffScope::Committed => "committed",
        }
    }

    pub fn next(self) -> Self {
        match self {
            DiffScope::All => DiffScope::Uncommitted,
            DiffScope::Uncommitted => DiffScope::Committed,
            DiffScope::Committed => DiffScope::All,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStat {
    pub insertions: u32,
    pub deletions: u32,
    pub files: u32,
    pub uncommitted: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAgent {
    pub project: ProjectId,
    pub name: String,
    pub kind: AgentKind,
    pub command: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub worktree: Option<WorktreeRequest>,
    /// Sent to the agent on its first start only.
    pub prompt: Option<String>,
    /// A worktree setup script the user approved, run in the agent's terminal before the agent
    /// on its first start only.
    pub setup: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookKind {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Notification,
    Stop,
    PermissionRequest,
    StopFailure,
    SubagentStart,
    SubagentStop,
}

impl HookKind {
    pub const ALL: [HookKind; 10] = [
        HookKind::SessionStart,
        HookKind::UserPromptSubmit,
        HookKind::PreToolUse,
        HookKind::PostToolUse,
        HookKind::Notification,
        HookKind::Stop,
        HookKind::PermissionRequest,
        HookKind::StopFailure,
        HookKind::SubagentStart,
        HookKind::SubagentStop,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HookKind::SessionStart => "SessionStart",
            HookKind::UserPromptSubmit => "UserPromptSubmit",
            HookKind::PreToolUse => "PreToolUse",
            HookKind::PostToolUse => "PostToolUse",
            HookKind::Notification => "Notification",
            HookKind::Stop => "Stop",
            HookKind::PermissionRequest => "PermissionRequest",
            HookKind::StopFailure => "StopFailure",
            HookKind::SubagentStart => "SubagentStart",
            HookKind::SubagentStop => "SubagentStop",
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
        assert_eq!(
            HookKind::from_name("PermissionRequest"),
            Some(HookKind::PermissionRequest)
        );
        assert_eq!(
            HookKind::from_name("StopFailure"),
            Some(HookKind::StopFailure)
        );
        assert_eq!(
            HookKind::from_name("SubagentStart"),
            Some(HookKind::SubagentStart)
        );
        assert_eq!(
            HookKind::from_name("SubagentStop"),
            Some(HookKind::SubagentStop)
        );
        assert_eq!(HookKind::from_name("PreCompact"), None);
    }

    #[test]
    fn usage_reads_as_one_line() {
        let usage = Usage {
            model: Some("Opus".into()),
            cost_micros: Some(423_700),
            context_percent: Some(23),
            five_hour_percent: Some(24),
            ..Usage::default()
        };
        assert_eq!(usage.summary(), "Opus · 23% context · $0.42");
        assert_eq!(Usage::default().summary(), "");
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
