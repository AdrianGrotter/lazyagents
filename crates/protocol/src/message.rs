use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    AgentId, AgentInfo, CreateAgent, DiffScope, Frame, GitStat, HookKind, InputEvent, Project,
    ProjectId, Usage,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    Hello { protocol_version: u32 },
    Request { id: u64, request: Request },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    ListProjects,
    AddProject {
        path: PathBuf,
        name: Option<String>,
    },
    RemoveProject {
        id: ProjectId,
    },
    ListAgents,
    CreateAgent(CreateAgent),
    KillAgent {
        id: AgentId,
    },
    RestartAgent {
        id: AgentId,
        cols: u16,
        rows: u16,
    },
    RemoveAgent {
        id: AgentId,
        remove_worktree: bool,
        force: bool,
        /// Also delete the worktree's branch, which is refused unless it is merged into its
        /// base; needs `remove_worktree`.
        delete_branch: bool,
    },
    /// Merge a worktree agent's branch into its base, in the project's own checkout.
    MergeAgent {
        id: AgentId,
    },
    GitStat {
        id: AgentId,
    },
    GitDiff {
        id: AgentId,
        scope: DiffScope,
    },
    RenameAgent {
        id: AgentId,
        name: String,
    },
    Subscribe {
        id: AgentId,
    },
    Unsubscribe,
    Input {
        id: AgentId,
        event: InputEvent,
    },
    Resize {
        id: AgentId,
        cols: u16,
        rows: u16,
    },
    Scroll {
        id: AgentId,
        lines: i32,
    },
    Hook {
        id: AgentId,
        kind: HookKind,
        session_id: Option<String>,
        /// `SessionStart`'s `source`: `startup`, `resume`, `clear` or `compact`.
        source: Option<String>,
        /// A summary of what the hook is about: the tool call of a `PermissionRequest`, the
        /// message of a `Notification`.
        detail: Option<String>,
    },
    /// From `lazyagents statusline`, each time Claude refreshes the agent's status line.
    Usage {
        id: AgentId,
        usage: Usage,
    },
    Shutdown,
    /// Answer yes to what the agent waits for, with the keys its screen calls for.
    Approve {
        id: AgentId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Projects(Vec<Project>),
    Project(Project),
    Agents(Vec<AgentInfo>),
    Agent(AgentInfo),
    GitStat(GitStat),
    Diff(String),
    /// A finished operation, described for the user.
    Done(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Welcome {
        protocol_version: u32,
        daemon_version: String,
    },
    Response {
        id: u64,
        result: Result<Response, String>,
    },
    AgentUpdated(AgentInfo),
    AgentRemoved(AgentId),
    /// The agent's program rang the terminal bell.
    AgentBell(AgentId),
    ProjectsChanged(Vec<Project>),
    Frame(Frame),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentKind, Cell, Key, KeyCode, Modifiers, PROTOCOL_VERSION, ProjectId, Row, Status,
        WorktreeInfo, WorktreeRequest,
    };

    fn round_trip<T>(value: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let bytes = postcard::to_stdvec(value).expect("serialize");
        let back: T = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(&back, value);
    }

    fn info() -> AgentInfo {
        AgentInfo {
            id: AgentId::random(),
            project: ProjectId::random(),
            name: "api".into(),
            kind: AgentKind::Claude,
            command: vec!["claude".into()],
            cwd: "/tmp/p".into(),
            status: Status::Exited(2),
            worktree: Some(Box::new(WorktreeInfo {
                path: "/tmp/p/.worktrees/api".into(),
                branch: "agent/api".into(),
                base: "main".into(),
            })),
            last_error: Some("no such file".into()),
            title: Some("Fix the parser".into()),
            waiting_for: Some("Bash: ls".into()),
            usage: Some(Box::new(Usage {
                model: Some("Opus".into()),
                cost_micros: Some(420_000),
                context_percent: Some(23),
                five_hour_percent: Some(24),
                seven_day_percent: None,
                pr_number: Some(7),
                pr_review: Some("approved".into()),
            })),
        }
    }

    #[test]
    fn every_request_round_trips() {
        let id = AgentId::random();
        let requests = vec![
            Request::ListProjects,
            Request::AddProject {
                path: "/tmp/p".into(),
                name: Some("p".into()),
            },
            Request::RemoveProject {
                id: ProjectId::random(),
            },
            Request::ListAgents,
            Request::CreateAgent(CreateAgent {
                project: ProjectId::random(),
                name: "a".into(),
                kind: AgentKind::Generic,
                command: vec!["sh".into()],
                cols: 80,
                rows: 24,
                worktree: None,
                prompt: Some("fix the parser".into()),
                setup: None,
            }),
            Request::CreateAgent(CreateAgent {
                project: ProjectId::random(),
                name: "w".into(),
                kind: AgentKind::Claude,
                command: Vec::new(),
                cols: 80,
                rows: 24,
                worktree: Some(WorktreeRequest {
                    base: Some("main".into()),
                }),
                prompt: None,
                setup: None,
            }),
            Request::CreateAgent(CreateAgent {
                project: ProjectId::random(),
                name: "w".into(),
                kind: AgentKind::Claude,
                command: Vec::new(),
                cols: 80,
                rows: 24,
                worktree: Some(WorktreeRequest { base: None }),
                prompt: None,
                setup: None,
            }),
            Request::KillAgent { id },
            Request::RestartAgent {
                id,
                cols: 80,
                rows: 24,
            },
            Request::RemoveAgent {
                id,
                remove_worktree: true,
                force: false,
                delete_branch: true,
            },
            Request::RemoveAgent {
                id,
                remove_worktree: false,
                force: true,
                delete_branch: false,
            },
            Request::MergeAgent { id },
            Request::GitStat { id },
            Request::GitDiff {
                id,
                scope: DiffScope::Committed,
            },
            Request::RenameAgent {
                id,
                name: "b".into(),
            },
            Request::Subscribe { id },
            Request::Unsubscribe,
            Request::Input {
                id,
                event: InputEvent::Key(Key::new(KeyCode::Char('é'), Modifiers::ALT)),
            },
            Request::Input {
                id,
                event: InputEvent::Paste("multi\nline".into()),
            },
            Request::Input {
                id,
                event: InputEvent::Wheel {
                    up: true,
                    col: 3,
                    row: 4,
                },
            },
            Request::Input {
                id,
                event: InputEvent::Click { col: 5, row: 6 },
            },
            Request::Resize {
                id,
                cols: 100,
                rows: 30,
            },
            Request::Scroll { id, lines: -3 },
            Request::Hook {
                id,
                kind: HookKind::Notification,
                session_id: Some("s".into()),
                source: Some("resume".into()),
                detail: Some("Bash: ls".into()),
            },
            Request::Usage {
                id,
                usage: Usage::default(),
            },
            Request::Shutdown,
            Request::Approve { id },
        ];
        for request in requests {
            round_trip(&ClientMessage::Request { id: 7, request });
        }
        round_trip(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        });
    }

    #[test]
    fn every_server_message_round_trips() {
        let info = info();
        let messages = vec![
            ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "0.1.0".into(),
            },
            ServerMessage::Response {
                id: 1,
                result: Ok(Response::Ok),
            },
            ServerMessage::Response {
                id: 2,
                result: Err("nope".into()),
            },
            ServerMessage::Response {
                id: 3,
                result: Ok(Response::Agents(vec![info.clone()])),
            },
            ServerMessage::Response {
                id: 4,
                result: Ok(Response::GitStat(GitStat {
                    insertions: 3,
                    deletions: 2,
                    files: 1,
                    uncommitted: 4,
                })),
            },
            ServerMessage::Response {
                id: 5,
                result: Ok(Response::Diff("diff --git a b\n".into())),
            },
            ServerMessage::Response {
                id: 6,
                result: Ok(Response::Done("merged".into())),
            },
            ServerMessage::AgentUpdated(info.clone()),
            ServerMessage::AgentRemoved(info.id),
            ServerMessage::AgentBell(info.id),
            ServerMessage::ProjectsChanged(vec![Project {
                id: ProjectId::random(),
                name: "p".into(),
                path: "/p".into(),
            }]),
            ServerMessage::Frame(Frame {
                agent: info.id,
                cols: 2,
                rows: 1,
                full: true,
                lines: vec![Row {
                    index: 0,
                    cells: vec![Cell::default(), Cell::default()],
                }],
                cursor: Some((1, 0)),
            }),
        ];
        for message in messages {
            round_trip(&message);
        }
    }
}
