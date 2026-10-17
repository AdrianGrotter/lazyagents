//! Pins the encoded bytes of every message to `PROTOCOL_VERSION`.
//!
//! postcard is not self-describing and encodes enum variants by index, so any change to
//! a message type changes the bytes while both ends of a round trip still agree. This
//! test compares a fixed sample against `tests/fixtures/wire-v<PROTOCOL_VERSION>.txt`.
//!
//! When it fails after a deliberate wire change: bump `PROTOCOL_VERSION`, then run
//! `UPDATE_GOLDEN=1 cargo test -p protocol --test wire` to write the fixture for the new
//! version. Regenerating refuses to change the fixture of a version that already has
//! one, so a wire change cannot be recorded without a bump. Adding an enum variant
//! fails to compile until `request_slot`, `response_slot` or `server_slot` names it,
//! and fails the coverage tests until a sample exercises it.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use protocol::{
    AgentId, AgentInfo, AgentKind, Cell, ClientMessage, Color, CreateAgent, DiffScope, Frame,
    GitStat, HookKind, InputEvent, Key, KeyCode, Modifiers, PROTOCOL_VERSION, Project, ProjectId,
    Request, Response, Row, ServerMessage, Status, Style, Usage, WorktreeInfo, WorktreeRequest,
};
use uuid::Uuid;

const REQUEST_VARIANTS: usize = 21;
const RESPONSE_VARIANTS: usize = 8;
const SERVER_VARIANTS: usize = 7;

fn request_slot(request: &Request) -> usize {
    match request {
        Request::ListProjects => 0,
        Request::AddProject { .. } => 1,
        Request::RemoveProject { .. } => 2,
        Request::ListAgents => 3,
        Request::CreateAgent(_) => 4,
        Request::KillAgent { .. } => 5,
        Request::RestartAgent { .. } => 6,
        Request::RemoveAgent { .. } => 7,
        Request::MergeAgent { .. } => 8,
        Request::GitStat { .. } => 9,
        Request::GitDiff { .. } => 10,
        Request::RenameAgent { .. } => 11,
        Request::Subscribe { .. } => 12,
        Request::Unsubscribe => 13,
        Request::Input { .. } => 14,
        Request::Resize { .. } => 15,
        Request::Scroll { .. } => 16,
        Request::Hook { .. } => 17,
        Request::Usage { .. } => 18,
        Request::Shutdown => 19,
        Request::Approve { .. } => 20,
    }
}

fn response_slot(response: &Response) -> usize {
    match response {
        Response::Ok => 0,
        Response::Projects(_) => 1,
        Response::Project(_) => 2,
        Response::Agents(_) => 3,
        Response::Agent(_) => 4,
        Response::GitStat(_) => 5,
        Response::Diff(_) => 6,
        Response::Done(_) => 7,
    }
}

fn server_slot(message: &ServerMessage) -> usize {
    match message {
        ServerMessage::Welcome { .. } => 0,
        ServerMessage::Response { .. } => 1,
        ServerMessage::AgentUpdated(_) => 2,
        ServerMessage::AgentRemoved(_) => 3,
        ServerMessage::AgentBell(_) => 4,
        ServerMessage::ProjectsChanged(_) => 5,
        ServerMessage::Frame(_) => 6,
    }
}

const STATUS_VARIANTS: usize = 6;
const KEY_CODE_VARIANTS: usize = 17;
const COLOR_VARIANTS: usize = 3;
const SCOPE_VARIANTS: usize = 3;
const AGENT_KIND_VARIANTS: usize = 2;
const INPUT_EVENT_VARIANTS: usize = 3;
const HOOK_KIND_VARIANTS: usize = 8;

fn status_slot(status: &Status) -> usize {
    match status {
        Status::Starting => 0,
        Status::Working => 1,
        Status::NeedsInput => 2,
        Status::Idle => 3,
        Status::Exited(_) => 4,
        Status::Stopped => 5,
    }
}

fn key_code_slot(code: &KeyCode) -> usize {
    match code {
        KeyCode::Char(_) => 0,
        KeyCode::Enter => 1,
        KeyCode::Tab => 2,
        KeyCode::BackTab => 3,
        KeyCode::Backspace => 4,
        KeyCode::Esc => 5,
        KeyCode::Up => 6,
        KeyCode::Down => 7,
        KeyCode::Left => 8,
        KeyCode::Right => 9,
        KeyCode::Home => 10,
        KeyCode::End => 11,
        KeyCode::Insert => 12,
        KeyCode::Delete => 13,
        KeyCode::PageUp => 14,
        KeyCode::PageDown => 15,
        KeyCode::F(_) => 16,
    }
}

fn color_slot(color: &Color) -> usize {
    match color {
        Color::Default => 0,
        Color::Indexed(_) => 1,
        Color::Rgb(..) => 2,
    }
}

fn scope_slot(scope: &DiffScope) -> usize {
    match scope {
        DiffScope::All => 0,
        DiffScope::Uncommitted => 1,
        DiffScope::Committed => 2,
    }
}

fn agent_kind_slot(kind: &AgentKind) -> usize {
    match kind {
        AgentKind::Claude => 0,
        AgentKind::Generic => 1,
    }
}

fn input_event_slot(event: &InputEvent) -> usize {
    match event {
        InputEvent::Key(_) => 0,
        InputEvent::Paste(_) => 1,
        InputEvent::Wheel { .. } => 2,
    }
}

fn hook_kind_slot(kind: &HookKind) -> usize {
    match kind {
        HookKind::SessionStart => 0,
        HookKind::UserPromptSubmit => 1,
        HookKind::PreToolUse => 2,
        HookKind::PostToolUse => 3,
        HookKind::Notification => 4,
        HookKind::Stop => 5,
        HookKind::PermissionRequest => 6,
        HookKind::StopFailure => 7,
    }
}

fn all_statuses() -> Vec<Status> {
    vec![
        Status::Starting,
        Status::Working,
        Status::NeedsInput,
        Status::Idle,
        Status::Exited(-1),
        Status::Exited(i32::MIN),
        Status::Stopped,
    ]
}

fn all_key_codes() -> Vec<KeyCode> {
    vec![
        KeyCode::Char('é'),
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Esc,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Insert,
        KeyCode::Delete,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::F(200),
    ]
}

fn all_colors() -> Vec<Color> {
    vec![
        Color::Default,
        Color::Indexed(200),
        Color::Rgb(255, 128, 200),
    ]
}

fn all_scopes() -> Vec<DiffScope> {
    vec![DiffScope::All, DiffScope::Uncommitted, DiffScope::Committed]
}

fn all_agent_kinds() -> Vec<AgentKind> {
    vec![AgentKind::Claude, AgentKind::Generic]
}

fn all_hook_kinds() -> Vec<HookKind> {
    vec![
        HookKind::SessionStart,
        HookKind::UserPromptSubmit,
        HookKind::PreToolUse,
        HookKind::PostToolUse,
        HookKind::Notification,
        HookKind::Stop,
        HookKind::PermissionRequest,
        HookKind::StopFailure,
    ]
}

fn agent(n: u128) -> AgentId {
    AgentId(Uuid::from_u128(n))
}

fn project_id(n: u128) -> ProjectId {
    ProjectId(Uuid::from_u128(n))
}

fn full_info() -> AgentInfo {
    AgentInfo {
        id: agent(1),
        project: project_id(2),
        name: "api".into(),
        kind: AgentKind::Claude,
        command: vec!["claude".into(), "--resume".into()],
        cwd: "/tmp/p".into(),
        status: Status::Exited(i32::MIN),
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
            cost_micros: Some(5_000_000_000),
            context_percent: Some(200),
            five_hour_percent: Some(201),
            seven_day_percent: Some(202),
            pr_number: Some(70_000),
            pr_review: Some("approved".into()),
        })),
    }
}

fn bare_info() -> AgentInfo {
    AgentInfo {
        id: agent(3),
        project: project_id(2),
        name: "sh".into(),
        kind: AgentKind::Generic,
        command: Vec::new(),
        cwd: "/tmp/p".into(),
        status: Status::Starting,
        worktree: None,
        last_error: None,
        title: None,
        waiting_for: None,
        usage: None,
    }
}

fn create_agent(worktree: Option<WorktreeRequest>, with_options: bool) -> CreateAgent {
    CreateAgent {
        project: project_id(2),
        name: "a".into(),
        kind: AgentKind::Claude,
        command: vec!["sh".into()],
        cols: 300,
        rows: 200,
        worktree,
        prompt: with_options.then(|| "fix the parser".into()),
        setup: with_options.then(|| "just build".into()),
    }
}

fn requests() -> Vec<(String, Request)> {
    let id = agent(1);
    let mut out: Vec<(String, Request)> = vec![
        ("ListProjects", Request::ListProjects),
        (
            "AddProject",
            Request::AddProject {
                path: "/tmp/p".into(),
                name: Some("p".into()),
            },
        ),
        (
            "AddProject unnamed",
            Request::AddProject {
                path: "/tmp/p".into(),
                name: None,
            },
        ),
        (
            "RemoveProject",
            Request::RemoveProject { id: project_id(2) },
        ),
        ("ListAgents", Request::ListAgents),
        (
            "CreateAgent options",
            Request::CreateAgent(create_agent(None, true)),
        ),
        (
            "CreateAgent worktree base",
            Request::CreateAgent(create_agent(
                Some(WorktreeRequest {
                    base: Some("main".into()),
                }),
                false,
            )),
        ),
        (
            "CreateAgent worktree head",
            Request::CreateAgent(create_agent(Some(WorktreeRequest { base: None }), false)),
        ),
        ("KillAgent", Request::KillAgent { id }),
        (
            "RestartAgent",
            Request::RestartAgent {
                id,
                cols: 300,
                rows: 200,
            },
        ),
        (
            "RemoveAgent all",
            Request::RemoveAgent {
                id,
                remove_worktree: true,
                force: true,
                delete_branch: true,
            },
        ),
        (
            "RemoveAgent none",
            Request::RemoveAgent {
                id,
                remove_worktree: false,
                force: false,
                delete_branch: false,
            },
        ),
        ("MergeAgent", Request::MergeAgent { id }),
        ("GitStat", Request::GitStat { id }),
        (
            "RenameAgent",
            Request::RenameAgent {
                id,
                name: "b".into(),
            },
        ),
        ("Subscribe", Request::Subscribe { id }),
        ("Unsubscribe", Request::Unsubscribe),
        (
            "Input wheel",
            Request::Input {
                id,
                event: InputEvent::Wheel {
                    up: true,
                    col: 300,
                    row: 400,
                },
            },
        ),
        (
            "Input paste",
            Request::Input {
                id,
                event: InputEvent::Paste("multi\nline".into()),
            },
        ),
        (
            "Resize",
            Request::Resize {
                id,
                cols: 301,
                rows: 201,
            },
        ),
        (
            "Scroll",
            Request::Scroll {
                id,
                lines: i32::MIN,
            },
        ),
        (
            "Hook",
            Request::Hook {
                id,
                kind: HookKind::Notification,
                session_id: Some("s".into()),
                source: Some("resume".into()),
                detail: Some("Bash: ls".into()),
            },
        ),
        (
            "Hook bare",
            Request::Hook {
                id,
                kind: HookKind::Stop,
                session_id: None,
                source: None,
                detail: None,
            },
        ),
        (
            "Usage",
            Request::Usage {
                id,
                usage: full_info().usage.map(|usage| *usage).unwrap(),
            },
        ),
        (
            "Usage empty",
            Request::Usage {
                id,
                usage: Usage::default(),
            },
        ),
        ("Shutdown", Request::Shutdown),
        ("Approve", Request::Approve { id }),
    ]
    .into_iter()
    .map(|(label, request)| (label.to_string(), request))
    .collect();
    for scope in all_scopes() {
        out.push((format!("GitDiff {scope:?}"), Request::GitDiff { id, scope }));
    }
    let mods = [
        Modifiers::NONE,
        Modifiers::SHIFT,
        Modifiers::ALT,
        Modifiers::CTRL,
    ];
    for (i, code) in all_key_codes().into_iter().enumerate() {
        out.push((
            format!("Input key {code:?}"),
            Request::Input {
                id,
                event: InputEvent::Key(Key::new(code, mods[i % mods.len()])),
            },
        ));
    }
    for kind in all_hook_kinds() {
        out.push((
            format!("Hook {kind:?}"),
            Request::Hook {
                id,
                kind,
                session_id: None,
                source: None,
                detail: None,
            },
        ));
    }
    out
}

fn responses() -> Vec<Response> {
    let project = Project {
        id: project_id(2),
        name: "p".into(),
        path: "/p".into(),
    };
    vec![
        Response::Ok,
        Response::Projects(vec![project.clone()]),
        Response::Project(project),
        Response::Agents(vec![full_info(), bare_info()]),
        Response::Agent(full_info()),
        Response::GitStat(GitStat {
            insertions: 70_000,
            deletions: 70_001,
            files: 70_002,
            uncommitted: 70_003,
        }),
        Response::Diff("diff --git a b\n".into()),
        Response::Done("merged".into()),
    ]
}

fn frame() -> Frame {
    let styled = Style {
        bold: true,
        dim: true,
        italic: true,
        underline: true,
        reverse: true,
        strikethrough: true,
    };
    Frame {
        agent: agent(1),
        cols: 301,
        rows: 201,
        full: true,
        lines: vec![Row {
            index: 300,
            cells: std::iter::once(Cell::default())
                .chain(
                    all_colors()
                        .into_iter()
                        .zip(all_colors().into_iter().rev())
                        .map(|(fg, bg)| Cell {
                            text: "é".into(),
                            fg,
                            bg,
                            style: styled,
                        }),
                )
                .collect(),
        }],
        cursor: Some((300, 200)),
    }
}

fn server_messages() -> Vec<ServerMessage> {
    let project = Project {
        id: project_id(2),
        name: "p".into(),
        path: "/p".into(),
    };
    let mut out = vec![
        ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: "0.1.0".into(),
        },
        ServerMessage::Response {
            id: 5_000_000_001,
            result: Err("nope".into()),
        },
        ServerMessage::AgentUpdated(full_info()),
        ServerMessage::AgentUpdated(bare_info()),
        ServerMessage::AgentRemoved(agent(1)),
        ServerMessage::AgentBell(agent(1)),
        ServerMessage::ProjectsChanged(vec![project]),
        ServerMessage::Frame(frame()),
        ServerMessage::Frame(Frame {
            full: false,
            lines: Vec::new(),
            cursor: None,
            ..frame()
        }),
    ];
    for (i, response) in responses().into_iter().enumerate() {
        out.push(ServerMessage::Response {
            id: 5_000_000_000 + i as u64,
            result: Ok(response),
        });
    }
    for status in all_statuses() {
        out.push(ServerMessage::AgentUpdated(AgentInfo {
            status,
            ..bare_info()
        }));
    }
    for kind in all_agent_kinds() {
        out.push(ServerMessage::AgentUpdated(AgentInfo {
            kind,
            ..bare_info()
        }));
    }
    out.push(ServerMessage::Response {
        id: 99,
        result: Ok(Response::Ok),
    });
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").unwrap();
        out
    })
}

fn encode<T: serde::Serialize>(value: &T) -> String {
    hex(&postcard::to_stdvec(value).expect("serialize"))
}

fn render() -> String {
    let mut out = format!("# protocol version {PROTOCOL_VERSION}\n");
    let mut line = |label: &str, bytes: String| writeln!(out, "{label}: {bytes}").unwrap();
    line(
        "Hello",
        encode(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        }),
    );
    for (label, request) in requests() {
        line(
            &format!("Request {label}"),
            encode(&ClientMessage::Request {
                id: 5_000_000_000,
                request,
            }),
        );
    }
    for message in server_messages() {
        line("Server", encode(&message));
    }
    out
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("wire-v{PROTOCOL_VERSION}.txt"))
}

#[test]
fn wire_bytes_match_the_fixture_for_this_protocol_version() {
    let path = fixture_path();
    let actual = render();
    let existing = std::fs::read_to_string(&path).ok();

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        match existing {
            Some(expected) if expected == actual => {}
            Some(_) => panic!(
                "{} already records protocol version {PROTOCOL_VERSION} and the wire format \
                 no longer matches it. Bump PROTOCOL_VERSION in crates/protocol/src/lib.rs, \
                 then regenerate; never rewrite the fixture of a released version.",
                path.display()
            ),
            None => std::fs::write(&path, &actual).expect("write fixture"),
        }
        return;
    }

    let Some(expected) = existing else {
        panic!(
            "no fixture for protocol version {PROTOCOL_VERSION} at {}. If the wire format \
             changed, PROTOCOL_VERSION must be bumped; then run \
             `UPDATE_GOLDEN=1 cargo test -p protocol --test wire` to record it.",
            path.display()
        );
    };
    assert!(
        expected == actual,
        "the encoded messages changed while PROTOCOL_VERSION is still {PROTOCOL_VERSION}. \
         Bump PROTOCOL_VERSION in crates/protocol/src/lib.rs, then run \
         `UPDATE_GOLDEN=1 cargo test -p protocol --test wire` to record the new fixture.\n\
         --- expected ({})\n{expected}\n--- actual\n{actual}",
        path.display()
    );
}

fn assert_covers(name: &str, expected: usize, slots: impl Iterator<Item = usize>) {
    let seen: BTreeSet<usize> = slots.collect();
    let all: BTreeSet<usize> = (0..expected).collect();
    assert_eq!(
        seen, all,
        "{name}: the samples must exercise every variant; update the sample builders and the \
         *_VARIANTS constant"
    );
}

#[test]
fn samples_cover_every_request_variant() {
    assert_covers(
        "Request",
        REQUEST_VARIANTS,
        requests().iter().map(|(_, request)| request_slot(request)),
    );
}

#[test]
fn samples_cover_every_response_variant() {
    assert_covers(
        "Response",
        RESPONSE_VARIANTS,
        responses().iter().map(response_slot),
    );
}

#[test]
fn samples_cover_every_server_message_variant() {
    assert_covers(
        "ServerMessage",
        SERVER_VARIANTS,
        server_messages().iter().map(server_slot),
    );
}

#[test]
fn samples_cover_every_nested_enum_variant() {
    let messages = server_messages();
    let requests = requests();
    let frames: Vec<&Frame> = messages
        .iter()
        .filter_map(|message| match message {
            ServerMessage::Frame(frame) => Some(frame),
            _ => None,
        })
        .collect();
    let cells: Vec<&Cell> = frames
        .iter()
        .flat_map(|frame| frame.lines.iter().flat_map(|row| row.cells.iter()))
        .collect();
    let events: Vec<&InputEvent> = requests
        .iter()
        .filter_map(|(_, request)| match request {
            Request::Input { event, .. } => Some(event),
            _ => None,
        })
        .collect();
    let key_codes = events.iter().filter_map(|event| match event {
        InputEvent::Key(key) => Some(&key.code),
        _ => None,
    });
    let infos: Vec<&AgentInfo> = messages
        .iter()
        .filter_map(|message| match message {
            ServerMessage::AgentUpdated(info) => Some(info),
            _ => None,
        })
        .collect();
    let scopes = requests.iter().filter_map(|(_, request)| match request {
        Request::GitDiff { scope, .. } => Some(scope),
        _ => None,
    });
    let hooks = requests.iter().filter_map(|(_, request)| match request {
        Request::Hook { kind, .. } => Some(kind),
        _ => None,
    });

    assert_covers(
        "Status",
        STATUS_VARIANTS,
        infos.iter().map(|info| status_slot(&info.status)),
    );
    assert_covers(
        "AgentKind",
        AGENT_KIND_VARIANTS,
        infos.iter().map(|info| agent_kind_slot(&info.kind)),
    );
    assert_covers("KeyCode", KEY_CODE_VARIANTS, key_codes.map(key_code_slot));
    assert_covers(
        "Color",
        COLOR_VARIANTS,
        cells
            .iter()
            .flat_map(|cell| [color_slot(&cell.fg), color_slot(&cell.bg)]),
    );
    assert_covers("DiffScope", SCOPE_VARIANTS, scopes.map(scope_slot));
    assert_covers(
        "InputEvent",
        INPUT_EVENT_VARIANTS,
        events.iter().map(|event| input_event_slot(event)),
    );
    assert_covers("HookKind", HOOK_KIND_VARIANTS, hooks.map(hook_kind_slot));
}

#[test]
fn hook_kind_all_lists_every_variant() {
    assert_covers(
        "HookKind::ALL",
        HOOK_KIND_VARIANTS,
        HookKind::ALL.iter().map(hook_kind_slot),
    );
    assert_eq!(HookKind::ALL.len(), HOOK_KIND_VARIANTS);
}

/// Version-mismatch detection (and the restart prompt after an upgrade) reads the
/// handshake of a daemon or client from any version, so its start must never change:
/// `Hello` and `Welcome` are variant 0 and begin with `protocol_version` as a u32
/// varint. Fields after that prefix (`Welcome::daemon_version`) may evolve, so only the
/// prefix is pinned.
#[test]
fn handshake_prefix_is_independent_of_the_protocol_version() {
    const MESSAGE: &str = "the handshake layout must never change: version-mismatch \
                           detection across releases depends on it";
    for (version, prefix) in [
        (5u32, vec![0x00, 0x05]),
        (6, vec![0x00, 0x06]),
        (300, vec![0x00, 0xac, 0x02]),
    ] {
        let hello = postcard::to_stdvec(&ClientMessage::Hello {
            protocol_version: version,
        })
        .expect("serialize");
        assert_eq!(hello, prefix, "Hello for version {version}: {MESSAGE}");

        let welcome = postcard::to_stdvec(&ServerMessage::Welcome {
            protocol_version: version,
            daemon_version: "x".into(),
        })
        .expect("serialize");
        assert!(
            welcome.starts_with(&prefix),
            "Welcome for version {version}: {MESSAGE}"
        );
    }
}
