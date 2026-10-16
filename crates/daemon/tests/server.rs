mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use daemon::{Config, run};
use protocol::{
    AgentId, AgentInfo, AgentKind, ClientMessage, CreateAgent, HookKind, InputEvent, Key, KeyCode,
    Modifiers, PROTOCOL_VERSION, Project, Request, Response, Screen, ServerMessage, StatePaths,
    Status, WorktreeRequest, read_message, write_message,
};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(10);

struct TestDaemon {
    dir: tempfile::TempDir,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

impl TestDaemon {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    async fn start_with(idle_exit: Option<Duration>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = Some(spawn_daemon_with(dir.path(), idle_exit).await);
        Self { dir, task }
    }

    fn socket(&self) -> PathBuf {
        StatePaths::new(self.path()).socket()
    }

    async fn exits_within(&mut self, limit: Duration) {
        let task = self.task.take().expect("running");
        tokio::time::timeout(limit, task)
            .await
            .expect("daemon did not exit in time")
            .expect("join")
            .expect("clean exit");
    }

    async fn restart(&mut self) {
        let mut conn = Conn::open(self.dir.path()).await;
        conn.request(Request::Shutdown).await.expect("shutdown");
        self.task
            .take()
            .expect("running")
            .await
            .expect("join")
            .expect("run ok");
        self.task = Some(spawn_daemon(self.dir.path()).await);
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

fn config(dir: &Path) -> Config {
    Config {
        paths: StatePaths::new(dir),
        exe: PathBuf::from("/usr/bin/true"),
        idle_exit: None,
    }
}

async fn spawn_daemon(dir: &Path) -> JoinHandle<anyhow::Result<()>> {
    spawn_daemon_with(dir, None).await
}

async fn spawn_daemon_with(
    dir: &Path,
    idle_exit: Option<Duration>,
) -> JoinHandle<anyhow::Result<()>> {
    let task = tokio::spawn(run(Config {
        idle_exit,
        ..config(dir)
    }));
    let socket = StatePaths::new(dir).socket();
    let deadline = tokio::time::Instant::now() + WAIT;
    while UnixStream::connect(&socket).await.is_err() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon never listened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task
}

struct Conn {
    read: OwnedReadHalf,
    write: OwnedWriteHalf,
    next_id: u64,
    pending: Vec<ServerMessage>,
    screen: Screen,
}

impl Conn {
    async fn open(dir: &Path) -> Self {
        let stream = UnixStream::connect(StatePaths::new(dir).socket())
            .await
            .expect("connect");
        let (mut read, mut write) = stream.into_split();
        let welcome: ServerMessage = read_message(&mut read)
            .await
            .expect("read")
            .expect("welcome");
        assert!(
            matches!(welcome, ServerMessage::Welcome { protocol_version, .. } if protocol_version == PROTOCOL_VERSION)
        );
        write_message(
            &mut write,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .expect("hello");
        Self {
            read,
            write,
            next_id: 1,
            pending: Vec::new(),
            screen: Screen::blank(0, 0),
        }
    }

    async fn request(&mut self, request: Request) -> Result<Response, String> {
        let id = self.send(request).await;
        self.response(id).await
    }

    async fn send(&mut self, request: Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        write_message(&mut self.write, &ClientMessage::Request { id, request })
            .await
            .expect("send");
        id
    }

    async fn response(&mut self, id: u64) -> Result<Response, String> {
        let message = self
            .wait_for(|m| matches!(m, ServerMessage::Response { id: got, .. } if *got == id))
            .await;
        let ServerMessage::Response { result, .. } = message else {
            unreachable!()
        };
        result
    }

    async fn next(&mut self) -> ServerMessage {
        tokio::time::timeout(WAIT, read_message::<_, ServerMessage>(&mut self.read))
            .await
            .expect("timed out waiting for a message")
            .expect("read")
            .expect("daemon closed the connection")
    }

    async fn wait_for(
        &mut self,
        mut predicate: impl FnMut(&ServerMessage) -> bool,
    ) -> ServerMessage {
        if let Some(index) = self.pending.iter().position(&mut predicate) {
            return self.pending.remove(index);
        }
        loop {
            let message = self.next().await;
            if predicate(&message) {
                return message;
            }
            self.pending.push(message);
        }
    }

    async fn wait_status(&mut self, id: AgentId, status: Status) {
        self.wait_for(|m| matches!(m, ServerMessage::AgentUpdated(info) if info.id == id && info.status == status))
            .await;
    }

    async fn wait_text(&mut self, id: AgentId, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let pending = std::mem::take(&mut self.pending);
            for message in pending {
                match message {
                    ServerMessage::Frame(frame) if frame.agent == id => self.screen.apply(&frame),
                    other => self.pending.push(other),
                }
            }
            if self.screen.text().contains(needle) {
                return self.screen.text();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never saw {needle:?}; screen:\n{}",
                self.screen.text()
            );
            let message = self.next().await;
            self.pending.push(message);
        }
    }
}

async fn add_project(conn: &mut Conn) -> (Project, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("project dir");
    let Ok(Response::Project(project)) = conn
        .request(Request::AddProject {
            path: dir.path().to_path_buf(),
            name: None,
        })
        .await
    else {
        panic!("add project failed");
    };
    (project, dir)
}

async fn create(
    conn: &mut Conn,
    project: &Project,
    name: &str,
    kind: AgentKind,
    command: &[&str],
) -> Result<AgentInfo, String> {
    let request = Request::CreateAgent(CreateAgent {
        project: project.id,
        name: name.into(),
        kind,
        command: command.iter().map(|s| s.to_string()).collect(),
        cols: 80,
        rows: 24,
        worktree: None,
    });
    match conn.request(request).await {
        Ok(Response::Agent(info)) => Ok(info),
        Ok(other) => panic!("unexpected {other:?}"),
        Err(e) => Err(e),
    }
}

fn key(code: KeyCode) -> InputEvent {
    InputEvent::Key(Key::new(code, Modifiers::NONE))
}

fn pid_alive(pid: u32) -> bool {
    rustix::process::Pid::from_raw(pid as i32)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

/// Pids an agent reported; SIGKILLed on drop if the test panicked, so a failure leaks nothing.
struct Leaves(Vec<u32>);

impl Leaves {
    async fn read(path: &Path, count: usize) -> Self {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let pids: Vec<u32> = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect();
            if pids.len() >= count {
                return Self(pids);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{} never listed {count} pids",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn alive(&self) -> Vec<u32> {
        self.0
            .iter()
            .copied()
            .filter(|&pid| pid_alive(pid))
            .collect()
    }
}

impl Drop for Leaves {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for &pid in &self.0 {
                if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
        }
    }
}

fn ignores_sighup(pidfile: &Path) -> Vec<String> {
    sh(&format!(
        "trap '' HUP; echo $$ >> '{}'; sleep 301",
        pidfile.display()
    ))
}

fn job_control_child(pidfile: &Path) -> Vec<String> {
    sh(&format!(
        "set -m; sleep 302 & echo $! >> '{}'; wait",
        pidfile.display()
    ))
}

fn own_session_child(pidfile: &Path) -> Vec<String> {
    sh(&format!(
        "perl -MPOSIX -e 'POSIX::setsid(); exec \"sleep\", \"303\"' & echo $! >> '{}'; wait",
        pidfile.display()
    ))
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

async fn create_owned(
    conn: &mut Conn,
    project: &Project,
    name: &str,
    command: &[String],
) -> AgentInfo {
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    create(conn, project, name, AgentKind::Generic, &command)
        .await
        .expect("create")
}

#[tokio::test(flavor = "multi_thread")]
async fn welcome_then_empty_lists() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    assert_eq!(
        conn.request(Request::ListProjects).await,
        Ok(Response::Projects(vec![]))
    );
    assert_eq!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(vec![]))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_the_same_project_twice_returns_it() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let again = conn
        .request(Request::AddProject {
            path: dir.path().to_path_buf(),
            name: Some("x".into()),
        })
        .await;
    assert_eq!(again, Ok(Response::Project(project.clone())));
    assert_eq!(project.path, dir.path().canonicalize().expect("canonical"));
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_agent_streams_output_and_takes_input() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "echo",
        AgentKind::Generic,
        &[
            "sh",
            "-c",
            "printf ready; read line; printf 'got:%s' \"$line\"; sleep 5",
        ],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "ready").await;
    for ch in "hi".chars() {
        conn.request(Request::Input {
            id: agent.id,
            event: key(KeyCode::Char(ch)),
        })
        .await
        .expect("input");
    }
    conn.request(Request::Input {
        id: agent.id,
        event: key(KeyCode::Enter),
    })
    .await
    .expect("enter");
    conn.wait_text(agent.id, "got:hi").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_status_is_broadcast() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "fail",
        AgentKind::Generic,
        &["sh", "-c", "exit 3"],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::Exited(3)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_status_follows_output() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "burst",
        AgentKind::Generic,
        &["sh", "-c", "echo burst; sleep 30"],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::Working).await;
    let working = tokio::time::Instant::now();
    conn.wait_status(agent.id, Status::Idle).await;
    assert!(
        working.elapsed() >= Duration::from_millis(1500),
        "went idle {:?} after working",
        working.elapsed()
    );
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_and_duplicate_names_are_rejected() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    for bad in ["", "a/b", "-x", ".hidden", &"x".repeat(65)] {
        assert!(
            create(&mut conn, &project, bad, AgentKind::Generic, &["true"])
                .await
                .is_err(),
            "{bad:?}"
        );
    }
    create(
        &mut conn,
        &project,
        "dup",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("first");
    let err = create(
        &mut conn,
        &project,
        "dup",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect_err("dup");
    assert!(err.contains("dup"), "{err}");
    let err = create(&mut conn, &project, "nocmd", AgentKind::Generic, &[])
        .await
        .expect_err("no command");
    assert!(err.contains("command"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_failure_keeps_the_record_as_exited() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let err = create(
        &mut conn,
        &project,
        "ghost",
        AgentKind::Generic,
        &["/nonexistent/lazyagents-cmd"],
    )
    .await
    .expect_err("spawn fails");
    assert!(err.contains("lazyagents-cmd"), "{err}");
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].status, Status::Exited(127));
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_failure_reports_last_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let err = create(
        &mut conn,
        &project,
        "ghost",
        AgentKind::Generic,
        &["/nonexistent/lazyagents-cmd"],
    )
    .await
    .expect_err("spawn fails");
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].last_error.as_deref(), Some(err.as_str()));
    let healthy = create(
        &mut conn,
        &project,
        "fine",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    assert_eq!(healthy.last_error, None);
    assert_eq!(healthy.worktree, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_restart_clears_last_error() {
    use std::os::unix::fs::PermissionsExt;
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let bin = tempfile::tempdir().expect("bin dir");
    let script = bin.path().join("late-agent");
    let command = script.to_str().expect("utf8 path");
    let err = create(&mut conn, &project, "late", AgentKind::Generic, &[command])
        .await
        .expect_err("spawn fails");
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].last_error.as_deref(), Some(err.as_str()));
    let id = agents[0].id;

    std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let Ok(Response::Agent(restarted)) = conn
        .request(Request::RestartAgent {
            id,
            cols: 80,
            rows: 24,
        })
        .await
    else {
        panic!("restart failed")
    };
    assert_eq!(restarted.last_error, None);
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].last_error, None);
    conn.request(Request::KillAgent { id }).await.expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_restart_remove_and_rename() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "life",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    let Ok(Response::Agent(killed)) = conn.request(Request::KillAgent { id: agent.id }).await
    else {
        panic!("kill")
    };
    assert_eq!(killed.status, Status::Stopped);
    let Ok(Response::Agent(restarted)) = conn
        .request(Request::RestartAgent {
            id: agent.id,
            cols: 80,
            rows: 24,
        })
        .await
    else {
        panic!("restart")
    };
    assert!(restarted.status.is_running());
    let Ok(Response::Agent(renamed)) = conn
        .request(Request::RenameAgent {
            id: agent.id,
            name: "life2".into(),
        })
        .await
    else {
        panic!("rename")
    };
    assert_eq!(renamed.name, "life2");
    assert!(
        conn.request(Request::RemoveProject { id: project.id })
            .await
            .is_err(),
        "project still has agents"
    );
    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: false,
        force: false,
    })
    .await
    .expect("remove");
    conn.wait_for(|m| matches!(m, ServerMessage::AgentRemoved(id) if *id == agent.id))
        .await;
    assert_eq!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(vec![]))
    );
    conn.request(Request::RemoveProject { id: project.id })
        .await
        .expect("remove project");
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_persist_across_restarts_as_stopped() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "keep",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    drop(conn);
    daemon.restart().await;
    let mut conn = Conn::open(daemon.path()).await;
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].id, agent.id);
    assert_eq!(agents[0].status, Status::Stopped);
    assert_eq!(
        conn.request(Request::ListProjects).await,
        Ok(Response::Projects(vec![project]))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_agents_get_sessions_hooks_and_resume() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let args_file = dir.path().join("args");
    let fake = dir.path().join("claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec sleep 30\n",
            args_file.display()
        ),
    )
    .expect("fake");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");

    let agent = create(
        &mut conn,
        &project,
        "claude",
        AgentKind::Claude,
        &[fake.to_str().expect("utf8")],
    )
    .await
    .expect("create");
    let args = wait_for_file(&args_file).await;
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines[0], "--session-id");
    let session = lines[1].to_string();
    assert_eq!(lines[2], "--settings");
    let settings = StatePaths::new(daemon.path()).hook_settings(agent.id);
    assert_eq!(lines[3], settings.display().to_string());
    let settings_json = std::fs::read_to_string(&settings).expect("settings written");
    assert!(
        settings_json.contains("UserPromptSubmit"),
        "{settings_json}"
    );

    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::UserPromptSubmit,
        session_id: None,
        source: None,
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::Working).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::Notification,
        session_id: None,
        source: None,
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::NeedsInput).await;

    std::fs::remove_file(&args_file).expect("reset args");
    conn.request(Request::RestartAgent {
        id: agent.id,
        cols: 80,
        rows: 24,
    })
    .await
    .expect("restart");
    let resumed = wait_for_file(&args_file).await;
    let lines: Vec<&str> = resumed.lines().collect();
    assert_eq!(lines[..2], ["--resume", session.as_str()]);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

fn fake_claude(dir: &Path) -> (PathBuf, PathBuf) {
    let args_file = dir.join("args");
    let fake = dir.join("claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec sleep 30\n",
            args_file.display()
        ),
    )
    .expect("fake");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    (fake, args_file)
}

async fn restart_args(conn: &mut Conn, id: AgentId, args_file: &Path) -> Vec<String> {
    let _ = std::fs::remove_file(args_file);
    conn.request(Request::RestartAgent {
        id,
        cols: 80,
        rows: 24,
    })
    .await
    .expect("restart");
    wait_for_file(args_file)
        .await
        .lines()
        .map(str::to_owned)
        .collect()
}

async fn session_start(conn: &mut Conn, id: AgentId, session: uuid::Uuid, source: &str) {
    conn.request(Request::Hook {
        id,
        kind: HookKind::SessionStart,
        session_id: Some(session.to_string()),
        source: Some(source.into()),
    })
    .await
    .expect("hook");
}

#[tokio::test(flavor = "multi_thread")]
async fn session_start_source_decides_whether_the_session_stays_resumable() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let (fake, args_file) = fake_claude(dir.path());
    let agent = create(
        &mut conn,
        &project,
        "claude",
        AgentKind::Claude,
        &[fake.to_str().expect("utf8")],
    )
    .await
    .expect("create");
    wait_for_file(&args_file).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::UserPromptSubmit,
        session_id: None,
        source: None,
    })
    .await
    .expect("prompt");

    let resumed = uuid::Uuid::new_v4();
    session_start(&mut conn, agent.id, resumed, "resume").await;
    let args = restart_args(&mut conn, agent.id, &args_file).await;
    assert_eq!(args[..2], ["--resume".to_string(), resumed.to_string()]);

    drop(conn);
    daemon.restart().await;
    let mut conn = Conn::open(daemon.path()).await;
    let args = restart_args(&mut conn, agent.id, &args_file).await;
    assert_eq!(args[..2], ["--resume".to_string(), resumed.to_string()]);

    let cleared = uuid::Uuid::new_v4();
    session_start(&mut conn, agent.id, cleared, "clear").await;
    let state = daemon::store::load(&StatePaths::new(daemon.path()).state_file()).expect("state");
    assert_eq!(state.agents[0].session_id, Some(cleared));
    assert!(!state.agents[0].resumable);
    let args = restart_args(&mut conn, agent.id, &args_file).await;
    assert_eq!(args[0], "--session-id");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn hook_for_unknown_agent_is_an_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let err = conn
        .request(Request::Hook {
            id: AgentId::random(),
            kind: HookKind::Stop,
            session_id: None,
            source: None,
        })
        .await
        .expect_err("unknown");
    assert!(err.contains("unknown agent"), "{err}");
    assert_eq!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(vec![]))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_an_agent_that_ignores_sighup() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "nohup", &ignores_sighup(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_a_job_control_background_child() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "jobs", &job_control_child(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_a_descendant_in_its_own_session() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "setsid", &own_session_child(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

/// How long an agent's parent shell stays alive after it reported its pids: several
/// snapshot intervals, so its detached children are tracked before the parent exits.
const RELEASE_AFTER: Duration = Duration::from_secs(3);

fn wait_for_release(release: &Path) -> String {
    format!("until [ -e '{}' ]; do sleep 0.05; done", release.display())
}

async fn release(release: &Path) {
    tokio::time::sleep(RELEASE_AFTER).await;
    std::fs::write(release, "").expect("release");
}

async fn gone_within(leaves: &Leaves, limit: Duration) -> Vec<u32> {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let alive = leaves.alive();
        if alive.is_empty() || tokio::time::Instant::now() >= deadline {
            return alive;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_a_nohup_orphan_in_the_agent_group() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let script = format!(
        "(nohup sleep 305 >/dev/null 2>&1 & echo $! > '{}'); exec sleep 306",
        pidfile.display()
    );
    let agent = create_owned(&mut conn, &project, "orphan", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    assert_eq!(leaves.alive(), leaves.0, "orphan never ran");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(
        gone_within(&leaves, Duration::from_secs(3)).await,
        Vec::<u32>::new(),
        "survived kill"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exited_agent_leaves_no_detached_descendants() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let released = dir.path().join("release");
    let script = format!(
        "perl -e 'use POSIX; setsid(); exec q(sleep), 307' & echo $! > '{}'; {}; exit 0",
        pidfile.display(),
        wait_for_release(&released)
    );
    let agent = create_owned(&mut conn, &project, "detach", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    release(&released).await;
    conn.wait_status(agent.id, Status::Exited(0)).await;
    assert_eq!(
        gone_within(&leaves, Duration::from_secs(5)).await,
        Vec::<u32>::new(),
        "survived the agent's exit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exited_agent_leaves_no_group_members() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let script = format!(
        "(nohup sleep 311 >/dev/null 2>&1 & echo $! > '{}'); sleep 2; exit 0",
        pidfile.display()
    );
    let agent = create_owned(&mut conn, &project, "member", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    conn.wait_status(agent.id, Status::Exited(0)).await;
    assert_eq!(
        gone_within(&leaves, Duration::from_secs(5)).await,
        Vec::<u32>::new(),
        "survived the agent's exit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_reaping_does_not_block_requests() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let script = format!(
        "(nohup sh -c \"trap '' TERM HUP; echo \\$\\$ > '{}'; exec sleep 315\" </dev/null >/dev/null 2>&1 &); sleep 1; exit 0",
        pidfile.display()
    );
    let agent = create_owned(&mut conn, &project, "stubborn", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    let mut slowest = Duration::ZERO;
    loop {
        let asked = tokio::time::Instant::now();
        let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
            panic!("list")
        };
        slowest = slowest.max(asked.elapsed());
        let exited = agents
            .iter()
            .any(|a| a.id == agent.id && a.status == Status::Exited(0));
        if exited && leaves.alive().is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "agent never exited or {:?} survived",
            leaves.alive()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        slowest < Duration::from_millis(300),
        "a request took {slowest:?}"
    );
}

fn exits_leaving_a_stubborn_session(pidfile: &Path, released: &Path, marker: u32) -> Vec<String> {
    sh(&format!(
        "perl -e 'use POSIX; setsid(); $SIG{{TERM}}=q(IGNORE); $SIG{{HUP}}=q(IGNORE); exec q(sleep), {marker}' & echo $! > '{}'; {}; exit 0",
        pidfile.display(),
        wait_for_release(released)
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_during_an_exit_reap_leaves_nothing() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let released = dir.path().join("release");
    let agent = create_owned(
        &mut conn,
        &project,
        "stubborn",
        &exits_leaving_a_stubborn_session(&pidfile, &released, 316),
    )
    .await;
    let leaves = Leaves::read(&pidfile, 1).await;
    release(&released).await;
    conn.wait_status(agent.id, Status::Exited(0)).await;
    assert_eq!(leaves.alive(), leaves.0, "descendant never ran");
    conn.request(Request::Shutdown).await.expect("shutdown");
    daemon.exits_within(WAIT).await;
    assert_eq!(
        gone_within(&leaves, Duration::from_millis(200)).await,
        Vec::<u32>::new(),
        "survived shutdown"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_during_an_exit_reap_covers_its_descendants() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let released = dir.path().join("release");
    let agent = create_owned(
        &mut conn,
        &project,
        "stubborn",
        &exits_leaving_a_stubborn_session(&pidfile, &released, 317),
    )
    .await;
    let leaves = Leaves::read(&pidfile, 1).await;
    release(&released).await;
    conn.wait_status(agent.id, Status::Exited(0)).await;
    assert_eq!(leaves.alive(), leaves.0, "descendant never ran");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(
        gone_within(&leaves, Duration::from_millis(200)).await,
        Vec::<u32>::new(),
        "survived kill"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_stops_a_detached_descendant_whose_parent_exited() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let released = dir.path().join("release");
    let script = format!(
        "(perl -e 'use POSIX; setsid(); exec q(sleep), 312' & echo $! > '{}'; {}); exec sleep 313",
        pidfile.display(),
        wait_for_release(&released)
    );
    let agent = create_owned(&mut conn, &project, "reparent", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    release(&released).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(leaves.alive(), leaves.0, "descendant never ran");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_stops_the_agent_tree() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "rm", &job_control_child(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: false,
        force: false,
    })
    .await
    .expect("remove");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived remove");
}

#[tokio::test(flavor = "multi_thread")]
async fn restart_leaves_exactly_one_live_copy() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "again", &ignores_sighup(&pidfile)).await;
    Leaves::read(&pidfile, 1).await;
    conn.request(Request::RestartAgent {
        id: agent.id,
        cols: 80,
        rows: 24,
    })
    .await
    .expect("restart");
    let leaves = Leaves::read(&pidfile, 2).await;
    assert_eq!(leaves.alive(), vec![leaves.0[1]], "pids {:?}", leaves.0);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_agent_trees_and_removes_socket() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    create_owned(&mut conn, &project, "nohup", &ignores_sighup(&pidfile)).await;
    create_owned(&mut conn, &project, "jobs", &job_control_child(&pidfile)).await;
    create_owned(&mut conn, &project, "setsid", &own_session_child(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 3).await;
    assert_eq!(conn.request(Request::Shutdown).await, Ok(Response::Ok));
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived shutdown");
    daemon
        .task
        .take()
        .expect("task")
        .await
        .expect("join")
        .expect("clean exit");
    assert!(!StatePaths::new(daemon.path()).socket().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn second_daemon_refuses_to_start() {
    let daemon = TestDaemon::start().await;
    let err = run(config(daemon.path())).await.expect_err("second daemon");
    let message = format!("{err:#}");
    assert!(
        message.contains(&format!(
            "a daemon is already running (pid {})",
            std::process::id()
        )),
        "{message}"
    );
    assert!(daemon.socket().exists());
    let mut conn = Conn::open(daemon.path()).await;
    assert_eq!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(vec![]))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_socket_stops_the_daemon_and_its_agents() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    create_owned(&mut conn, &project, "nohup", &ignores_sighup(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    std::fs::remove_file(daemon.socket()).expect("remove socket");
    daemon.exits_within(Duration::from_secs(3)).await;
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived the daemon");
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_daemon_exits() {
    let mut daemon = TestDaemon::start_with(Some(Duration::from_secs(1))).await;
    daemon.exits_within(Duration::from_secs(4)).await;
    assert!(!daemon.socket().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_exit_waits_for_running_agents() {
    let mut daemon = TestDaemon::start_with(Some(Duration::from_secs(1))).await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pids");
    let agent = create_owned(&mut conn, &project, "busy", &ignores_sighup(&pidfile)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    drop(conn);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !daemon.task.as_ref().expect("task").is_finished(),
        "exited while an agent ran"
    );
    let mut conn = Conn::open(daemon.path()).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    drop(conn);
    daemon.exits_within(Duration::from_secs(4)).await;
    assert_eq!(leaves.alive(), Vec::<u32>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn state_dir_is_created_private() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().expect("tempdir");
    let dir = parent.path().join("state");
    let task = spawn_daemon(&dir).await;
    let mode = std::fs::metadata(&dir)
        .expect("state dir")
        .permissions()
        .mode();
    let mut conn = Conn::open(&dir).await;
    assert_eq!(conn.request(Request::Shutdown).await, Ok(Response::Ok));
    task.await.expect("join").expect("clean exit");
    assert_eq!(mode & 0o777, 0o700, "mode {mode:o}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_socket_file_is_replaced() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = StatePaths::new(dir.path()).socket();
    drop(std::os::unix::net::UnixListener::bind(&socket).expect("bind stale"));
    assert!(socket.exists());
    let task = spawn_daemon(dir.path()).await;
    let mut conn = Conn::open(dir.path()).await;
    assert_eq!(conn.request(Request::Shutdown).await, Ok(Response::Ok));
    task.await.expect("join").expect("clean exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_state_prevents_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = StatePaths::new(dir.path()).state_file();
    std::fs::write(&state, "[[[ not toml").expect("write");
    let err = run(config(dir.path())).await.expect_err("corrupt");
    assert!(format!("{err:#}").contains("state.toml"), "{err:#}");
    assert_eq!(
        std::fs::read_to_string(&state).expect("untouched"),
        "[[[ not toml"
    );
}

async fn wait_for_file(path: &Path) -> String {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn input_to_exited_agent_is_an_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "done",
        AgentKind::Generic,
        &["sh", "-c", "exit 0"],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::Exited(0)).await;
    let err = conn
        .request(Request::Input {
            id: agent.id,
            event: key(KeyCode::Enter),
        })
        .await
        .expect_err("input to exited agent");
    assert!(err.contains("not running"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn late_hook_does_not_revive_an_exited_agent() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "gone",
        AgentKind::Claude,
        &["sh", "-c", "exit 0"],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::Exited(0)).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::UserPromptSubmit,
        session_id: None,
        source: None,
    })
    .await
    .expect("late hook");
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].status, Status::Exited(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_generic_agent_becomes_idle() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "quiet",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    let idle = tokio::time::timeout(
        Duration::from_secs(5),
        conn.wait_status(agent.id, Status::Idle),
    )
    .await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    idle.expect("silent agent never became idle");
}

#[tokio::test(flavor = "multi_thread")]
async fn exited_agent_can_be_scrolled_and_resized() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "done",
        AgentKind::Generic,
        &["sh", "-c", "seq 1 200; exit 0"],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::Exited(0)).await;
    assert_eq!(
        conn.request(Request::Subscribe { id: agent.id }).await,
        Ok(Response::Ok)
    );
    let ServerMessage::Frame(frame) = conn
        .wait_for(|m| matches!(m, ServerMessage::Frame(f) if f.agent == agent.id))
        .await
    else {
        unreachable!()
    };
    assert!(frame.full, "first frame after subscribe is not full");
    let mut screen = Screen::blank(0, 0);
    screen.apply(&frame);
    assert!(screen.text().contains("200"), "{}", screen.text());
    assert_eq!(
        conn.request(Request::Scroll {
            id: agent.id,
            lines: 20
        })
        .await,
        Ok(Response::Ok)
    );
    assert_eq!(
        conn.request(Request::Resize {
            id: agent.id,
            cols: 60,
            rows: 10
        })
        .await,
        Ok(Response::Ok)
    );
    let err = conn
        .request(Request::Input {
            id: agent.id,
            event: key(KeyCode::Enter),
        })
        .await
        .expect_err("input to exited agent");
    assert!(err.contains("not running"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_to_a_stopped_agent_then_restart_gets_frames() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "paused",
        AgentKind::Generic,
        &["sh", "-c", "echo back; exec sleep 30"],
    )
    .await
    .expect("create");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    assert_eq!(
        conn.request(Request::Subscribe { id: agent.id }).await,
        Ok(Response::Ok)
    );
    conn.request(Request::RestartAgent {
        id: agent.id,
        cols: 80,
        rows: 24,
    })
    .await
    .expect("restart");
    let frame = tokio::time::timeout(
        Duration::from_secs(5),
        conn.wait_for(|m| matches!(m, ServerMessage::Frame(f) if f.agent == agent.id)),
    )
    .await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    frame.expect("no frame after restart");
}

#[tokio::test(flavor = "multi_thread")]
async fn scroll_and_resize_need_a_terminal() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "halted",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    let scroll = conn
        .request(Request::Scroll {
            id: agent.id,
            lines: 5,
        })
        .await
        .expect_err("scroll without a terminal");
    assert!(scroll.contains("has no terminal"), "{scroll}");
    let resize = conn
        .request(Request::Resize {
            id: agent.id,
            cols: 60,
            rows: 10,
        })
        .await
        .expect_err("resize without a terminal");
    assert!(resize.contains("has no terminal"), "{resize}");
}

async fn add_repo_project(conn: &mut Conn) -> (Project, tempfile::TempDir) {
    let repo = common::repo();
    let Ok(Response::Project(project)) = conn
        .request(Request::AddProject {
            path: repo.path().to_path_buf(),
            name: None,
        })
        .await
    else {
        panic!("add project failed");
    };
    (project, repo)
}

async fn create_in_worktree(
    conn: &mut Conn,
    project: &Project,
    name: &str,
    command: &[&str],
) -> Result<AgentInfo, String> {
    let request = Request::CreateAgent(CreateAgent {
        project: project.id,
        name: name.into(),
        kind: AgentKind::Generic,
        command: command.iter().map(|s| s.to_string()).collect(),
        cols: 250,
        rows: 24,
        worktree: Some(WorktreeRequest { base: None }),
    });
    match conn.request(request).await {
        Ok(Response::Agent(info)) => Ok(info),
        Ok(other) => panic!("unexpected {other:?}"),
        Err(e) => Err(e),
    }
}

fn worktree_path(daemon: &TestDaemon, project: &Project, name: &str) -> PathBuf {
    daemon
        .path()
        .canonicalize()
        .expect("canonical state dir")
        .join("worktrees")
        .join(format!("{}-{}", project.name, &project.id.to_string()[..8]))
        .join(name)
}

async fn list_agents(conn: &mut Conn) -> Vec<AgentInfo> {
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    agents
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_agent_runs_in_its_worktree() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "wt", &["sh", "-c", "pwd; sleep 30"])
        .await
        .expect("create");
    let path = worktree_path(&daemon, &project, "wt");
    let worktree = agent.worktree.clone().expect("worktree info");
    assert_eq!(worktree.branch, "lazyagents/wt");
    assert_eq!(worktree.base, "main");
    assert_eq!(worktree.path, path);
    assert_eq!(agent.cwd, path);
    assert!(path.join("README").is_file());

    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, &path.display().to_string()).await;

    let Ok(Response::Agent(renamed)) = conn
        .request(Request::RenameAgent {
            id: agent.id,
            name: "wt2".into(),
        })
        .await
    else {
        panic!("rename")
    };
    assert_eq!(renamed.worktree, Some(worktree));
    assert_eq!(renamed.cwd, path);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_agent_stopped_by_a_blocked_envrc_says_how_to_allow_it() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_repo_project(&mut conn).await;
    let blocked = "echo \"direnv: error $PWD/.envrc is blocked. Run \\`direnv allow\\` to approve its content\"; exit 1";
    let agent = create_in_worktree(&mut conn, &project, "envrc", &["sh", "-c", blocked])
        .await
        .expect("create");
    let path = worktree_path(&daemon, &project, "envrc");
    let hint = format!("run: direnv allow {}", path.display());
    let message = conn
        .wait_for(|message| {
            matches!(message, ServerMessage::AgentUpdated(info)
                if info.id == agent.id
                    && info.last_error.as_ref().is_some_and(|e| e.contains(&hint)))
        })
        .await;
    let ServerMessage::AgentUpdated(info) = message else {
        unreachable!()
    };
    assert_eq!(info.status, Status::Exited(1));
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_branch_names_create_nothing() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let err = create_in_worktree(&mut conn, &project, "foo.lock", &["sleep", "30"])
        .await
        .expect_err("invalid branch");
    assert!(err.contains("foo.lock"), "{err}");
    assert!(list_agents(&mut conn).await.is_empty());
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    assert!(!worktree_path(&daemon, &project, "foo.lock").exists());
    assert!(!daemon.path().join("worktrees").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn worktree_in_non_git_project_is_an_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let err = create_in_worktree(&mut conn, &project, "nogit", &["sleep", "30"])
        .await
        .expect_err("not a repo");
    assert!(err.contains("not a git repository"), "{err}");
    assert!(list_agents(&mut conn).await.is_empty());
    assert!(!daemon.path().join("worktrees").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_refuses_a_dirty_worktree_and_lists_files() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "dirty", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    std::fs::write(path.join("README"), "changed\n").expect("edit");
    std::fs::write(path.join("new.txt"), "new\n").expect("add");

    let err = conn
        .request(Request::RemoveAgent {
            id: agent.id,
            remove_worktree: true,
            force: false,
        })
        .await
        .expect_err("dirty worktree");
    assert_eq!(err, "worktree has uncommitted changes:\nREADME\nnew.txt");
    let agents = list_agents(&mut conn).await;
    assert_eq!(agents.len(), 1);
    assert!(agents[0].status.is_running(), "{:?}", agents[0].status);
    assert!(path.join("new.txt").is_file());
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_refuses_untracked_files_hidden_by_the_repo_config() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    common::git(repo.path(), &["config", "status.showUntrackedFiles", "no"]);
    let agent = create_in_worktree(&mut conn, &project, "hidden", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    std::fs::write(path.join("untracked.txt"), "u\n").expect("untracked");

    let err = conn
        .request(Request::RemoveAgent {
            id: agent.id,
            remove_worktree: true,
            force: false,
        })
        .await
        .expect_err("untracked file");
    assert_eq!(err, "worktree has uncommitted changes:\nuntracked.txt");
    assert!(path.join("untracked.txt").is_file());
    assert_eq!(list_agents(&mut conn).await.len(), 1);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_with_force_removes_the_worktree() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "forced", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    std::fs::write(path.join("README"), "changed\n").expect("edit");

    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: true,
        force: true,
    })
    .await
    .expect("forced remove");
    assert!(!path.exists());
    assert!(list_agents(&mut conn).await.is_empty());
    let worktrees = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("forced"), "{worktrees}");
}

#[tokio::test(flavor = "multi_thread")]
async fn branches_survive_worktree_removal() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "keep", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();

    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: true,
        force: false,
    })
    .await
    .expect("clean remove");
    assert!(!path.exists());
    assert!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/keep"])
            .contains("lazyagents/keep")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_without_remove_worktree_keeps_it() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "stay", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    std::fs::write(path.join("README"), "changed\n").expect("edit");

    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: false,
        force: false,
    })
    .await
    .expect("remove agent only");
    assert!(list_agents(&mut conn).await.is_empty());
    assert_eq!(
        std::fs::read_to_string(path.join("README")).expect("kept"),
        "changed\n"
    );
    let worktrees = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(worktrees.contains("stay"), "{worktrees}");
}

#[tokio::test(flavor = "multi_thread")]
async fn git_queries_answer_while_the_loop_keeps_serving() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let lines = 400_000;
    let numbered = |order: &dyn Fn(usize) -> usize| -> String {
        (0..lines)
            .map(|i| format!("line {:07}\n", order(i)))
            .collect()
    };
    std::fs::write(repo.path().join("big.txt"), numbered(&|i| i)).expect("write big file");
    common::git(repo.path(), &["add", "big.txt"]);
    common::git(repo.path(), &["commit", "-q", "-m", "big"]);
    let agent = create_in_worktree(&mut conn, &project, "big", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    let shuffled = numbered(&|i| i * 7919 % lines);
    assert!(shuffled.len() > 5_000_000);
    std::fs::write(path.join("big.txt"), shuffled).expect("shuffle big file");

    let sent = tokio::time::Instant::now();
    let diff_id = conn.send(Request::GitDiff { id: agent.id }).await;
    let list_id = conn.send(Request::ListAgents).await;
    let mut list_after = None;
    let mut diff = None;
    while list_after.is_none() || diff.is_none() {
        match conn.next().await {
            ServerMessage::Response { id, result } if id == list_id => {
                assert!(matches!(result, Ok(Response::Agents(_))), "{result:?}");
                list_after = Some((sent.elapsed(), diff.is_none()));
            }
            ServerMessage::Response { id, result } if id == diff_id => diff = Some(result),
            _ => {}
        }
    }
    let (elapsed, list_first) = list_after.expect("list answered");
    assert!(
        list_first || elapsed <= Duration::from_millis(300),
        "ListAgents took {elapsed:?} behind the diff"
    );
    let Some(Ok(Response::Diff(text))) = diff else {
        panic!("diff failed: {diff:?}")
    };
    assert!(text.len() > 5_000_000, "{} bytes", text.len());
    assert!(text.contains("+line 0007919\n"));
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn git_stat_and_diff_for_plain_agents_show_uncommitted_changes() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "plain",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    assert_eq!(agent.worktree, None);
    std::fs::write(repo.path().join("README"), "hello\nmore\n").expect("edit");

    assert_eq!(
        conn.request(Request::GitStat { id: agent.id }).await,
        Ok(Response::GitStat(protocol::GitStat {
            insertions: 1,
            deletions: 0,
            files: 1,
            uncommitted: 1,
        }))
    );
    let Ok(Response::Diff(text)) = conn.request(Request::GitDiff { id: agent.id }).await else {
        panic!("diff")
    };
    assert!(text.contains("+more"), "{text}");
    let unknown = conn
        .request(Request::GitStat {
            id: AgentId::random(),
        })
        .await
        .expect_err("unknown agent");
    assert!(unknown.contains("unknown agent"), "{unknown}");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

fn failing_post_checkout(repo: &Path) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let hooks = tempfile::tempdir().expect("hooks dir");
    let hook = hooks.path().join("post-checkout");
    std::fs::write(&hook, "#!/bin/sh\necho post-checkout refused >&2\nexit 1\n").expect("hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    common::git(
        repo,
        &[
            "config",
            "core.hooksPath",
            hooks.path().to_str().expect("utf8"),
        ],
    );
    hooks
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_worktree_add_leaves_nothing_and_the_name_stays_usable() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let hooks = failing_post_checkout(repo.path());
    let path = worktree_path(&daemon, &project, "hooked");

    let err = create_in_worktree(&mut conn, &project, "hooked", &["sleep", "30"])
        .await
        .expect_err("post-checkout fails");
    assert!(err.contains("post-checkout refused"), "{err}");
    assert!(list_agents(&mut conn).await.is_empty());
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    assert!(!path.exists());
    let worktrees = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("hooked"), "{worktrees}");

    std::fs::remove_file(hooks.path().join("post-checkout")).expect("drop hook");
    let agent = create_in_worktree(&mut conn, &project, "hooked", &["sleep", "30"])
        .await
        .expect("same name after the hook is gone");
    assert_eq!(agent.worktree.expect("worktree").path, path);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_worktree_path_is_left_alone() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let path = worktree_path(&daemon, &project, "taken");
    std::fs::create_dir_all(&path).expect("occupy path");
    std::fs::write(path.join("mine.txt"), "keep me\n").expect("occupant");

    let err = create_in_worktree(&mut conn, &project, "taken", &["sleep", "30"])
        .await
        .expect_err("path exists");
    assert!(err.contains("already exists"), "{err}");
    assert!(list_agents(&mut conn).await.is_empty());
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    assert_eq!(
        std::fs::read_to_string(path.join("mine.txt")).expect("untouched"),
        "keep me\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_project_bases_worktrees_on_its_commit() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    common::git(repo.path(), &["checkout", "-q", "--detach"]);
    let head = common::git(repo.path(), &["rev-parse", "HEAD"]);

    let agent = create_in_worktree(&mut conn, &project, "detached", &["sleep", "30"])
        .await
        .expect("create");
    assert_eq!(agent.worktree.expect("worktree").base, head.trim());
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}
