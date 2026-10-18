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

/// How long anything the daemon is expected to do may take. With the whole suite running in
/// parallel each spawned process (`ps`, every git command) can take seconds, so a request
/// that runs several, such as creating a worktree agent, needs well over 10 s.
const WAIT: Duration = Duration::from_secs(30);

struct TestDaemon {
    dir: tempfile::TempDir,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

impl TestDaemon {
    async fn start() -> Self {
        Self::start_with(None).await
    }

    async fn start_with(idle_exit: Option<Duration>) -> Self {
        Self::start_configured(|config| Config {
            idle_exit,
            ..config
        })
        .await
    }

    async fn start_with_direnv(direnv: PathBuf) -> Self {
        Self::start_configured(|config| Config {
            direnv: Some(direnv),
            ..config
        })
        .await
    }

    async fn start_configured(configure: impl FnOnce(Config) -> Config) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let task = Some(spawn_configured(configure(config(dir.path()))).await);
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
        direnv: None,
        claude_config: None,
    }
}

async fn spawn_daemon(dir: &Path) -> JoinHandle<anyhow::Result<()>> {
    spawn_configured(config(dir)).await
}

async fn spawn_configured(config: Config) -> JoinHandle<anyhow::Result<()>> {
    let socket = config.paths.socket();
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(run(config));
    while UnixStream::connect(&socket).await.is_err() {
        if task.is_finished() {
            panic!("daemon stopped before listening: {:?}", task.await);
        }
        assert!(
            started.elapsed() < WAIT,
            "daemon never listened within {WAIT:?}"
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
        prompt: None,
        setup: None,
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
async fn a_client_of_another_protocol_is_closed_unanswered() {
    let daemon = TestDaemon::start().await;
    let stream = UnixStream::connect(daemon.socket()).await.expect("connect");
    let (mut read, mut write) = stream.into_split();
    let welcome: Option<ServerMessage> = read_message(&mut read).await.expect("welcome");
    assert!(matches!(welcome, Some(ServerMessage::Welcome { .. })));
    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: 1,
        },
    )
    .await
    .expect("hello");
    let _ = write_message(
        &mut write,
        &ClientMessage::Request {
            id: 1,
            request: Request::ListAgents,
        },
    )
    .await;
    let next = tokio::time::timeout(WAIT, read_message::<_, ServerMessage>(&mut read))
        .await
        .expect("the daemon kept the connection open");
    assert!(matches!(next, Ok(None) | Err(_)), "{next:?}");
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
        delete_branch: false,
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
    for event in ["UserPromptSubmit", "PermissionRequest", "StopFailure"] {
        assert!(settings_json.contains(event), "{event}: {settings_json}");
    }

    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::UserPromptSubmit,
        session_id: None,
        source: None,
        detail: None,
        subagent: None,
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::Working).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::Notification,
        session_id: None,
        source: None,
        detail: None,
        subagent: None,
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
        detail: None,
        subagent: None,
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
        detail: None,
        subagent: None,
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

fn titled_claude(dir: &Path) -> PathBuf {
    let fake = dir.join("claude");
    std::fs::write(
        &fake,
        "#!/bin/sh\n\
         printf '\\033]0;\u{2733} Claude Code\\007'\n\
         sleep 0.3\n\
         printf '\\033]0;\u{2810} Fix the parser\\007'\n\
         sleep 0.3\n\
         printf '\\033]0;\u{2733} Fix the parser\\007'\n\
         exec sleep 30\n",
    )
    .expect("fake");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    fake
}

async fn wait_title(conn: &mut Conn, id: AgentId, title: Option<&str>) {
    conn.wait_for(|m| {
        matches!(m, ServerMessage::AgentUpdated(info)
            if info.id == id && info.title.as_deref() == title)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_terminal_title_names_the_session_and_is_kept() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let fake = titled_claude(dir.path());
    let agent = create(
        &mut conn,
        &project,
        "claude",
        AgentKind::Claude,
        &[fake.to_str().expect("utf8")],
    )
    .await
    .expect("create");
    assert_eq!(agent.title, None);
    wait_title(&mut conn, agent.id, Some("Fix the parser")).await;
    assert!(
        !conn.pending.iter().any(|m| matches!(m,
            ServerMessage::AgentUpdated(info) if info.title.as_deref() == Some("Claude Code"))),
        "the default title is not a session title"
    );

    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    drop(conn);
    daemon.restart().await;
    let mut conn = Conn::open(daemon.path()).await;
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].title.as_deref(), Some("Fix the parser"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_session_clears_the_title() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let fake = titled_claude(dir.path());
    let agent = create(
        &mut conn,
        &project,
        "claude",
        AgentKind::Claude,
        &[fake.to_str().expect("utf8")],
    )
    .await
    .expect("create");
    wait_title(&mut conn, agent.id, Some("Fix the parser")).await;
    session_start(&mut conn, agent.id, uuid::Uuid::new_v4(), "clear").await;
    wait_title(&mut conn, agent.id, None).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn generic_agents_ignore_terminal_titles() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "shell",
        AgentKind::Generic,
        &[
            "sh",
            "-c",
            "printf '\\033]0;~/git/x\\007'; echo titled; exec sleep 30",
        ],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "titled").await;
    let Ok(Response::Agents(agents)) = conn.request(Request::ListAgents).await else {
        panic!("list")
    };
    assert_eq!(agents[0].title, None);
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

async fn create_with_prompt(
    conn: &mut Conn,
    project: &Project,
    kind: AgentKind,
    command: &[&str],
    prompt: &str,
) -> AgentInfo {
    let request = Request::CreateAgent(CreateAgent {
        project: project.id,
        name: "prompted".into(),
        kind,
        command: command.iter().map(|s| s.to_string()).collect(),
        cols: 80,
        rows: 24,
        worktree: None,
        prompt: Some(prompt.into()),
        setup: None,
    });
    match conn.request(request).await {
        Ok(Response::Agent(info)) => info,
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_prompt_is_passed_on_the_first_start_only() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let (fake, args_file) = fake_claude(dir.path());
    let agent = create_with_prompt(
        &mut conn,
        &project,
        AgentKind::Claude,
        &[fake.to_str().expect("utf8")],
        "fix the parser",
    )
    .await;
    let args = wait_for_file(&args_file).await;
    let lines: Vec<&str> = args.lines().collect();
    assert_eq!(lines[lines.len() - 2..], ["--", "fix the parser"]);
    let restarted = restart_args(&mut conn, agent.id, &args_file).await;
    assert!(
        !restarted.iter().any(|a| a == "fix the parser"),
        "{restarted:?}"
    );
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_generic_prompt_is_typed_once_the_agent_prints() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create_with_prompt(
        &mut conn,
        &project,
        AgentKind::Generic,
        &[
            "sh",
            "-c",
            "printf 'ready> '; read line; echo \"got:$line\"; exec sleep 30",
        ],
        "hello there",
    )
    .await;
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "got:hello there").await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

async fn hook(conn: &mut Conn, id: AgentId, kind: HookKind, detail: Option<&str>) {
    conn.request(Request::Hook {
        id,
        kind,
        session_id: None,
        source: None,
        detail: detail.map(str::to_string),
        subagent: None,
    })
    .await
    .expect("hook");
}

async fn waiting_for(conn: &mut Conn, id: AgentId) -> Option<String> {
    list_agents(conn)
        .await
        .into_iter()
        .find(|a| a.id == id)
        .and_then(|a| a.waiting_for)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_waiting_agent_says_what_it_waits_for_until_it_moves_on() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "asker",
        AgentKind::Claude,
        &["sh", "-c", "exec sleep 30"],
    )
    .await
    .expect("create");
    hook(
        &mut conn,
        agent.id,
        HookKind::PermissionRequest,
        Some("Bash: npm test"),
    )
    .await;
    conn.wait_status(agent.id, Status::NeedsInput).await;
    assert_eq!(
        waiting_for(&mut conn, agent.id).await.as_deref(),
        Some("Bash: npm test")
    );
    hook(
        &mut conn,
        agent.id,
        HookKind::Notification,
        Some("Claude needs your permission to use Bash"),
    )
    .await;
    assert_eq!(
        waiting_for(&mut conn, agent.id).await.as_deref(),
        Some("Bash: npm test")
    );
    hook(&mut conn, agent.id, HookKind::PostToolUse, None).await;
    conn.wait_status(agent.id, Status::Working).await;
    assert_eq!(waiting_for(&mut conn, agent.id).await, None);
    hook(
        &mut conn,
        agent.id,
        HookKind::Notification,
        Some("Pick a colour"),
    )
    .await;
    conn.wait_status(agent.id, Status::NeedsInput).await;
    assert_eq!(
        waiting_for(&mut conn, agent.id).await.as_deref(),
        Some("Pick a colour")
    );
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_agent_reports_its_usage_and_a_generic_one_does_not() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let claude = create(
        &mut conn,
        &project,
        "c",
        AgentKind::Claude,
        &["sh", "-c", "exec sleep 30"],
    )
    .await
    .expect("create");
    let generic = create(
        &mut conn,
        &project,
        "g",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    let usage = protocol::Usage {
        model: Some("Opus".into()),
        cost_micros: Some(420_000),
        context_percent: Some(23),
        ..protocol::Usage::default()
    };
    for id in [claude.id, generic.id] {
        conn.request(Request::Usage {
            id,
            usage: usage.clone(),
        })
        .await
        .expect("usage");
    }
    conn.wait_for(|m| {
        matches!(m, ServerMessage::AgentUpdated(info) if info.id == claude.id && info.usage.is_some())
    })
    .await;
    let agents = list_agents(&mut conn).await;
    let usage_of = |id| {
        agents
            .iter()
            .find(|a| a.id == id)
            .and_then(|a| a.usage.as_deref().cloned())
    };
    assert_eq!(usage_of(claude.id), Some(usage));
    assert_eq!(usage_of(generic.id), None);
    for id in [claude.id, generic.id] {
        conn.request(Request::KillAgent { id }).await.expect("kill");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_users_status_line_is_taken_over_in_the_generated_settings() {
    let claude_config = tempfile::tempdir().expect("claude config");
    std::fs::write(
        claude_config.path().join("settings.json"),
        r#"{"statusLine": {"type": "command", "command": "mine.sh", "padding": 2}}"#,
    )
    .expect("settings");
    let dir = claude_config.path().to_path_buf();
    let daemon = TestDaemon::start_configured(move |config| Config {
        claude_config: Some(dir),
        ..config
    })
    .await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "c",
        AgentKind::Claude,
        &["sh", "-c", "exec sleep 30"],
    )
    .await
    .expect("create");
    let settings = StatePaths::new(daemon.path()).hook_settings(agent.id);
    let generated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(settings).expect("read")).expect("json");
    assert_eq!(
        generated["statusLine"],
        serde_json::json!({"type": "command", "command": "'/usr/bin/true' statusline", "padding": 2})
    );
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agents_bell_reaches_every_client() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let mut other = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "ringer",
        AgentKind::Generic,
        &["sh", "-c", "sleep 0.5; printf 'ding\\a'; exec sleep 30"],
    )
    .await
    .expect("create");
    for conn in [&mut conn, &mut other] {
        conn.wait_for(|m| matches!(m, ServerMessage::AgentBell(id) if *id == agent.id))
            .await;
    }
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_agent_at_the_trust_dialog_needs_input() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "untrusted",
        AgentKind::Claude,
        &[
            "sh",
            "-c",
            "printf ' Quick safety check\\n   Yes, I trust this folder\\n'; exec sleep 30",
        ],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::NeedsInput).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::SessionStart,
        session_id: None,
        source: Some("startup".into()),
        detail: None,
        subagent: None,
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::Idle).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

/// A shell script that shows `screen`, then prints the bytes it next receives in hex,
/// followed by `|`.
fn records_answer(screen: &str) -> String {
    format!(
        "stty -icanon -echo -icrnl min 8 time 5; printf '{screen}'; \
         dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'; printf '|'; exec sleep 30"
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_the_trust_dialog_moves_to_yes_and_confirms() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let script = records_answer(
        " Quick safety check\\n\\n ❯ No, exit\\n   Yes, I trust this folder\\n\\n Enter to confirm\\n",
    );
    let agent = create(
        &mut conn,
        &project,
        "untrusted",
        AgentKind::Claude,
        &["sh", "-c", &script],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_status(agent.id, Status::NeedsInput).await;
    conn.request(Request::Approve { id: agent.id })
        .await
        .expect("approve");
    conn.wait_text(agent.id, "1b5b420d|").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_the_trust_dialog_twice_never_interleaves_the_keys() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let script = records_answer(" ❯ No, exit\\n   Yes, I trust this folder\\n");
    let agent = create(
        &mut conn,
        &project,
        "untrusted",
        AgentKind::Claude,
        &["sh", "-c", &script],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_status(agent.id, Status::NeedsInput).await;
    for _ in 0..2 {
        conn.request(Request::Approve { id: agent.id })
            .await
            .expect("approve");
    }
    conn.wait_text(agent.id, "1b5b420d1b5b420d|").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_a_claude_prompt_that_quotes_the_trust_dialog_presses_enter() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let script = format!(
        "stty -icanon -echo -icrnl min 1 time 0; printf 'started\\n'; \
         dd bs=1 count=1 >/dev/null 2>&1; {}",
        records_answer(
            "+ Yes, I trust this folder\\n\\n Run cargo test?\\n ❯ 1. Yes\\n   2. Yes, and allow\\n   3. No\\n"
        )
    );
    let agent = create(
        &mut conn,
        &project,
        "trusted",
        AgentKind::Claude,
        &["sh", "-c", &script],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "started").await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::SessionStart,
        session_id: None,
        source: Some("startup".into()),
        detail: None,
        subagent: None,
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::Idle).await;
    conn.request(Request::Input {
        id: agent.id,
        event: key(KeyCode::Char('x')),
    })
    .await
    .expect("input");
    conn.wait_text(agent.id, "3. No").await;
    conn.request(Request::Approve { id: agent.id })
        .await
        .expect("approve");
    conn.wait_text(agent.id, "0d|").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_an_ordinary_prompt_presses_enter() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let script = records_answer("Run npm test? ready\\n");
    let agent = create(
        &mut conn,
        &project,
        "asking",
        AgentKind::Generic,
        &["sh", "-c", &script],
    )
    .await
    .expect("create");
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "ready").await;
    conn.request(Request::Approve { id: agent.id })
        .await
        .expect("approve");
    conn.wait_text(agent.id, "0d|").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_a_trust_dialog_without_a_yes_option_is_an_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let script = records_answer(" Do you trust the files in this folder?\\n");
    let agent = create(
        &mut conn,
        &project,
        "untrusted",
        AgentKind::Claude,
        &["sh", "-c", &script],
    )
    .await
    .expect("create");
    conn.wait_status(agent.id, Status::NeedsInput).await;
    let err = conn
        .request(Request::Approve { id: agent.id })
        .await
        .expect_err("approve without a yes option");
    assert!(err.contains("trust"), "{err}");
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
            detail: None,
            subagent: None,
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

fn wait_for_release(release: &Path) -> String {
    format!("until [ -e '{}' ]; do sleep 0.05; done", release.display())
}

/// Lets an agent's parent shell exit once the daemon has finished two process-table
/// readings after the call, so at least one of them started after the agent's
/// descendants existed and links them, however slow `ps` is.
async fn release(release: &Path) {
    two_more_readings().await;
    std::fs::write(release, "").expect("release");
}

/// Waits until the daemon has finished two process-table readings after the call, so at
/// least one of them started after whatever the test did before it.
async fn two_more_readings() {
    let from = daemon::reap::readings();
    let deadline = tokio::time::Instant::now() + WAIT;
    while daemon::reap::readings() < from + 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the daemon finished no two process-table readings"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
        gone_within(&leaves, WAIT).await,
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
        gone_within(&leaves, WAIT).await,
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
    let parent_gone = dir.path().join("parent-gone");
    let script = format!(
        "(perl -e 'use POSIX; setsid(); exec q(sleep), 312' & echo $! > '{}'; {}); echo gone > '{}'; exec sleep 313",
        pidfile.display(),
        wait_for_release(&released),
        parent_gone.display()
    );
    let agent = create_owned(&mut conn, &project, "reparent", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;
    release(&released).await;
    wait_for_file(&parent_gone).await;
    two_more_readings().await;
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
        delete_branch: false,
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
async fn the_lock_file_records_the_daemons_pid_and_start_time() {
    let daemon = TestDaemon::start().await;
    let contents =
        std::fs::read_to_string(StatePaths::new(daemon.path()).lock_file()).expect("lock file");
    let pid = std::process::id();
    assert_eq!(
        protocol::lockfile::parse(&contents),
        Some(protocol::lockfile::Holder {
            pid,
            start: Some(protocol::lockfile::process_start(pid).expect("our start")),
        }),
        "{contents:?}"
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
    daemon.exits_within(Duration::from_secs(20)).await;
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
    daemon.exits_within(Duration::from_secs(20)).await;
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
        detail: None,
        subagent: None,
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
        prompt: None,
        setup: None,
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

/// A stand-in for direnv: `status` and `allow <dir>` append themselves to `calls` next
/// to the script, `status` then runs the given `status` command, and `exec <dir> <cmd…>`
/// runs the command in `dir`.
fn fake_direnv_with_status(dir: &Path, status: &str) -> PathBuf {
    let fake = dir.join("direnv");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             status) echo status >> \"$(dirname \"$0\")/calls\"; {status} ;;\n\
             allow) echo \"allow $2\" >> \"$(dirname \"$0\")/calls\" ;;\n\
             exec) dir=$2; shift 2; cd \"$dir\" && exec \"$@\" ;;\n\
             esac\n"
        ),
    )
    .expect("fake direnv");
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    fake
}

fn fake_direnv(dir: &Path, allowed: bool) -> PathBuf {
    let allowed = if allowed { 0 } else { 1 };
    fake_direnv_with_status(
        dir,
        &format!(
            r#"printf '{{"state":{{"foundRC":{{"allowed":{allowed},"path":"%s/.envrc"}}}}}}\n' "$PWD""#
        ),
    )
}

fn direnv_calls(fake: &Path) -> String {
    std::fs::read_to_string(fake.with_file_name("calls")).unwrap_or_default()
}

async fn add_envrc_repo_project(conn: &mut Conn) -> (Project, tempfile::TempDir) {
    let repo = common::repo();
    std::fs::write(repo.path().join(".envrc"), "use flake\n").expect("write .envrc");
    common::git(repo.path(), &["add", ".envrc"]);
    common::git(repo.path(), &["commit", "-q", "-m", "envrc"]);
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

/// Creates a worktree agent under `fake` and returns direnv's recorded calls once the
/// agent printed its working directory.
async fn create_worktree_agent_under(fake: PathBuf, name: &str) -> (PathBuf, String) {
    let daemon = TestDaemon::start_with_direnv(fake.clone()).await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_envrc_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, name, &["sh", "-c", "pwd; sleep 30"])
        .await
        .expect("create");
    let path = worktree_path(&daemon, &project, name);
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, &path.display().to_string()).await;
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
    (path, direnv_calls(&fake))
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_allowed_envrc_is_allowed_for_a_new_worktree() {
    let bin = tempfile::tempdir().expect("bin");
    let (path, calls) = create_worktree_agent_under(fake_direnv(bin.path(), true), "same").await;
    assert_eq!(calls, format!("status\nallow {}\n", path.display()));
}

#[tokio::test(flavor = "multi_thread")]
async fn unallowed_project_envrc_is_not_copied_to_the_worktree() {
    let bin = tempfile::tempdir().expect("bin");
    let (_, calls) = create_worktree_agent_under(fake_direnv(bin.path(), false), "blocked").await;
    assert_eq!(calls, "status\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn status_failure_allows_nothing() {
    let bin = tempfile::tempdir().expect("bin");
    let fake = fake_direnv_with_status(bin.path(), "echo broken >&2; exit 1");
    let (_, calls) = create_worktree_agent_under(fake, "failing").await;
    assert_eq!(calls, "status\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn unparsable_status_allows_nothing() {
    let bin = tempfile::tempdir().expect("bin");
    let fake = fake_direnv_with_status(bin.path(), "echo nope");
    let (_, calls) = create_worktree_agent_under(fake, "garbled").await;
    assert_eq!(calls, "status\n");
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
            delete_branch: false,
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
            delete_branch: false,
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
        delete_branch: false,
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
        delete_branch: false,
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
        delete_branch: false,
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
    let diff_id = conn
        .send(Request::GitDiff {
            id: agent.id,
            scope: protocol::DiffScope::All,
        })
        .await;
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
    let Ok(Response::Diff(text)) = conn
        .request(Request::GitDiff {
            id: agent.id,
            scope: protocol::DiffScope::All,
        })
        .await
    else {
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
async fn a_branch_that_existed_before_survives_a_failed_add() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    common::git(repo.path(), &["checkout", "-q", "-b", "lazyagents/x"]);
    std::fs::write(repo.path().join("mine.txt"), "mine\n").expect("write");
    common::git(repo.path(), &["add", "mine.txt"]);
    common::git(repo.path(), &["commit", "-q", "-m", "mine"]);
    common::git(repo.path(), &["checkout", "-q", "main"]);
    let commit = common::git(repo.path(), &["rev-parse", "lazyagents/x"]);

    let err = create_in_worktree(&mut conn, &project, "x", &["sleep", "30"])
        .await
        .expect_err("the branch exists");
    assert!(err.contains("lazyagents/x"), "{err}");
    assert!(list_agents(&mut conn).await.is_empty());
    assert_eq!(
        common::git(repo.path(), &["rev-parse", "lazyagents/x"]),
        commit
    );
    assert!(!worktree_path(&daemon, &project, "x").exists());
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

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_agent_is_merged_then_removed_with_its_branch() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(
        &mut conn,
        &project,
        "feat",
        &["sh", "-c", "printf 'x\\n' > feat.txt && git add feat.txt && git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -qm feat && exec sleep 30"],
    )
    .await
    .expect("create");
    let worktree = agent.worktree.clone().expect("worktree");
    let deadline = tokio::time::Instant::now() + WAIT;
    while common::git(repo.path(), &["log", "--oneline", "lazyagents/feat"])
        .lines()
        .count()
        < 2
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the agent never committed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let refused = conn
        .request(Request::RemoveAgent {
            id: agent.id,
            remove_worktree: true,
            force: false,
            delete_branch: true,
        })
        .await
        .expect_err("unmerged branch kept");
    assert!(refused.contains("not merged into main"), "{refused}");

    let merged = conn
        .request(Request::MergeAgent { id: agent.id })
        .await
        .expect("merge");
    assert_eq!(
        merged,
        Response::Done("merged lazyagents/feat into main".into())
    );
    assert!(repo.path().join("feat.txt").exists());

    conn.request(Request::RemoveAgent {
        id: agent.id,
        remove_worktree: true,
        force: false,
        delete_branch: true,
    })
    .await
    .expect("remove");
    assert!(!worktree.path.exists());
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/feat"]),
        ""
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_worktree_agent_can_be_merged() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_repo_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "plain",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("create");
    let err = conn
        .request(Request::MergeAgent { id: agent.id })
        .await
        .expect_err("no worktree");
    assert!(err.contains("has no worktree branch to merge"), "{err}");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_setup_script_runs_once_in_the_new_worktree_before_the_agent() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _repo) = add_repo_project(&mut conn).await;
    let request = Request::CreateAgent(CreateAgent {
        project: project.id,
        name: "setup".into(),
        kind: AgentKind::Generic,
        command: vec![
            "sh".into(),
            "-c".into(),
            "echo agent-sees:$(cat ready.txt 2>/dev/null || echo nothing); exec sleep 30".into(),
        ],
        cols: 120,
        rows: 24,
        worktree: Some(WorktreeRequest { base: None }),
        prompt: None,
        setup: Some("echo $LAZYAGENTS_PROJECT_PATH > /dev/null; echo prepared > ready.txt".into()),
    });
    let Ok(Response::Agent(agent)) = conn.request(request).await else {
        panic!("create failed")
    };
    conn.request(Request::Subscribe { id: agent.id })
        .await
        .expect("subscribe");
    conn.wait_text(agent.id, "agent-sees:prepared").await;
    let worktree = agent.worktree.clone().expect("worktree").path;
    std::fs::remove_file(worktree.join("ready.txt")).expect("remove marker");
    conn.request(Request::RestartAgent {
        id: agent.id,
        cols: 120,
        rows: 24,
    })
    .await
    .expect("restart");
    conn.wait_text(agent.id, "agent-sees:nothing").await;
    assert!(!worktree.join("ready.txt").exists(), "setup ran again");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

/// A git hook in the repo that, once armed, holds the git command running it until the
/// release file appears. Dropping the gate releases it, and the hook also gives up when
/// its directory is gone or after about a minute, so a failing test leaves nothing waiting.
struct Gate {
    _hooks: tempfile::TempDir,
    armed: PathBuf,
    entered: PathBuf,
    left: PathBuf,
    release: PathBuf,
}

impl Gate {
    /// Holds every `git worktree add` in the repo.
    fn post_checkout(repo: &Path) -> Self {
        let gate = Self::install(repo, "core.hooksPath", "post-checkout", 0, None);
        gate.arm();
        gate
    }

    /// Holds every `git worktree add` until `release` exists.
    fn post_checkout_until(repo: &Path, release: &Path) -> Self {
        let gate = Self::install(repo, "core.hooksPath", "post-checkout", 0, Some(release));
        gate.arm();
        gate
    }

    /// Holds, once armed, every git command that reads the working tree's status.
    fn fsmonitor(repo: &Path) -> Self {
        Self::install(repo, "core.fsmonitor", "fsmonitor", 1, None)
    }

    fn install(repo: &Path, key: &str, name: &str, status: u8, release: Option<&Path>) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let hooks = tempfile::tempdir().expect("hooks dir");
        let armed = hooks.path().join("armed");
        let entered = hooks.path().join("entered");
        let left = hooks.path().join("left");
        let release = release.map_or_else(|| hooks.path().join("release"), Path::to_path_buf);
        let hook = hooks.path().join(name);
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\n[ -e '{armed}' ] || exit {status}\necho in > '{entered}'\ni=0\nuntil [ -e '{release}' ] || [ ! -e '{entered}' ] || [ $i -ge 1200 ]; do i=$((i+1)); sleep 0.05; done\necho out > '{left}'\nexit {status}\n",
                armed = armed.display(),
                entered = entered.display(),
                left = left.display(),
                release = release.display(),
            ),
        )
        .expect("hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let value = if key == "core.hooksPath" {
            hooks.path()
        } else {
            hook.as_path()
        };
        common::git(repo, &["config", key, value.to_str().expect("utf8")]);
        Self {
            _hooks: hooks,
            armed,
            entered,
            left,
            release,
        }
    }

    fn arm(&self) {
        std::fs::write(&self.armed, "").expect("arm");
    }

    /// Polls from the blocking pool, so a daemon that blocks the runtime cannot stall it.
    async fn entered(&self) {
        let entered = self.entered.clone();
        tokio::task::spawn_blocking(move || {
            let deadline = std::time::Instant::now() + WAIT;
            while !entered.exists() {
                assert!(std::time::Instant::now() < deadline, "the hook never ran");
                std::thread::sleep(Duration::from_millis(20));
            }
        })
        .await
        .expect("join");
    }

    /// Whether an armed hook that ran still holds its git command.
    fn holding(&self) -> bool {
        !self.left.exists()
    }

    fn open(&self) {
        std::fs::write(&self.release, "").expect("release");
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.open();
    }
}

fn create_in_worktree_request(project: &Project, name: &str, command: &[&str]) -> Request {
    Request::CreateAgent(CreateAgent {
        project: project.id,
        name: name.into(),
        kind: AgentKind::Generic,
        command: command.iter().map(|s| s.to_string()).collect(),
        cols: 120,
        rows: 24,
        worktree: Some(WorktreeRequest { base: None }),
        prompt: None,
        setup: None,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_client_is_greeted_while_a_worktree_is_created() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let gate = Gate::post_checkout(repo.path());
    let create = conn
        .send(create_in_worktree_request(
            &project,
            "slow",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let welcomed = greeted(daemon.path()).await;
    gate.open();
    assert!(welcomed, "no Welcome while a worktree is created");
    let Ok(Response::Agent(agent)) = conn.response(create).await else {
        panic!("create failed")
    };
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

/// Whether a new connection gets its Welcome within `WAIT`, timed on a runtime of its own,
/// so a daemon that blocks the test's runtime cannot stall the timeout.
async fn greeted(dir: &Path) -> bool {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(async { tokio::time::timeout(WAIT, Conn::open(&dir)).await.is_ok() })
    })
    .await
    .expect("join")
}

#[tokio::test(flavor = "multi_thread")]
async fn hooks_and_panes_keep_flowing_while_a_worktree_is_created() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let signals = tempfile::tempdir().expect("signals");
    let go = signals.path().join("go");
    let script = format!(
        "until [ -e '{}' ]; do sleep 0.05; done; echo streaming-ok; exec sleep 300",
        go.display()
    );
    let watcher = create(
        &mut conn,
        &project,
        "watcher",
        AgentKind::Claude,
        &["sh", "-c", &script],
    )
    .await
    .expect("create watcher");
    let mut viewer = Conn::open(daemon.path()).await;
    viewer
        .request(Request::Subscribe { id: watcher.id })
        .await
        .expect("subscribe");
    let gate = Gate::post_checkout(repo.path());
    let create = conn
        .send(create_in_worktree_request(
            &project,
            "slow",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let mut hook_conn = Conn::open(daemon.path()).await;
    hook(
        &mut hook_conn,
        watcher.id,
        HookKind::Notification,
        Some("needs you"),
    )
    .await;
    assert_eq!(
        waiting_for(&mut hook_conn, watcher.id).await.as_deref(),
        Some("needs you")
    );
    std::fs::write(&go, "").expect("go");
    viewer.wait_text(watcher.id, "streaming-ok").await;
    assert!(
        gate.holding(),
        "the hook and the output waited for the worktree"
    );

    gate.open();
    let Ok(Response::Agent(slow)) = conn.response(create).await else {
        panic!("create failed")
    };
    for id in [slow.id, watcher.id] {
        conn.request(Request::KillAgent { id }).await.expect("kill");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_is_reserved_while_its_worktree_is_created() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let gate = Gate::post_checkout(repo.path());
    let first = conn
        .send(create_in_worktree_request(
            &project,
            "twin",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let mut other = Conn::open(daemon.path()).await;
    let err = other
        .request(Request::RemoveProject { id: project.id })
        .await
        .expect_err("project in use");
    assert!(err.contains("remove the project's agents first"), "{err}");
    let err = create_in_worktree(&mut other, &project, "Twin", &["sleep", "30"])
        .await
        .expect_err("the name in another case, in a worktree");
    assert!(err.contains("Twin is being created"), "{err}");
    let err = create(
        &mut other,
        &project,
        "twin",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect_err("same name in the checkout");
    assert!(err.contains("twin is being created"), "{err}");
    let plain = create(
        &mut other,
        &project,
        "plain",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect("another name");
    let err = other
        .request(Request::RenameAgent {
            id: plain.id,
            name: "twin".into(),
        })
        .await
        .expect_err("rename to the reserved name");
    assert!(err.contains("twin is being created"), "{err}");

    gate.open();
    let Ok(Response::Agent(twin)) = conn.response(first).await else {
        panic!("create failed")
    };
    assert_eq!(twin.name, "twin");
    let err = create(
        &mut other,
        &project,
        "TWIN",
        AgentKind::Generic,
        &["sleep", "30"],
    )
    .await
    .expect_err("the name in another case");
    assert!(err.contains("TWIN already exists"), "{err}");
    for id in [twin.id, plain.id] {
        conn.request(Request::KillAgent { id }).await.expect("kill");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_being_removed_refuses_other_lifecycle_requests() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "doomed", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    let gate = Gate::fsmonitor(repo.path());
    gate.arm();
    let remove = conn
        .send(Request::RemoveAgent {
            id: agent.id,
            remove_worktree: true,
            force: false,
            delete_branch: false,
        })
        .await;
    gate.entered().await;

    let mut other = Conn::open(daemon.path()).await;
    for request in [
        Request::KillAgent { id: agent.id },
        Request::RestartAgent {
            id: agent.id,
            cols: 80,
            rows: 24,
        },
        Request::RemoveAgent {
            id: agent.id,
            remove_worktree: false,
            force: false,
            delete_branch: false,
        },
        Request::MergeAgent { id: agent.id },
        Request::RenameAgent {
            id: agent.id,
            name: "renamed".into(),
        },
    ] {
        let err = other.request(request).await.expect_err("busy");
        assert!(err.contains("doomed is being removed"), "{err}");
    }
    assert_eq!(list_agents(&mut other).await.len(), 1);

    gate.open();
    assert_eq!(conn.response(remove).await, Ok(Response::Ok));
    assert!(list_agents(&mut conn).await.is_empty());
    assert!(!path.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_during_a_worktree_create_leaves_no_worktree() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let signals = tempfile::tempdir().expect("signals");
    let stopped = signals.path().join("stopped");
    let script = format!(
        "trap \"echo stopped > '{}'; exit 0\" HUP TERM; while :; do sleep 0.05; done",
        stopped.display()
    );
    create(
        &mut conn,
        &project,
        "sentinel",
        AgentKind::Generic,
        &["sh", "-c", &script],
    )
    .await
    .expect("create sentinel");
    let gate = Gate::post_checkout_until(repo.path(), &stopped);
    let create = conn
        .send(create_in_worktree_request(
            &project,
            "late",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let mut other = Conn::open(daemon.path()).await;
    let shutdown = other.send(Request::Shutdown).await;
    let err = conn
        .response(create)
        .await
        .expect_err("shut down before it started");
    assert!(err.contains("shut down before late started"), "{err}");
    assert_eq!(other.response(shutdown).await, Ok(Response::Ok));
    daemon.exits_within(WAIT).await;

    assert!(!worktree_path(&daemon, &project, "late").exists());
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    let worktrees = common::git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("late"), "{worktrees}");
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_are_answered_while_an_agent_is_killed() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, dir) = add_project(&mut conn).await;
    let pidfile = dir.path().join("pid");
    let script = format!(
        "trap '' TERM HUP; echo $$ > '{}'; exec sleep 318",
        pidfile.display()
    );
    let agent = create_owned(&mut conn, &project, "stubborn", &sh(&script)).await;
    let leaves = Leaves::read(&pidfile, 1).await;

    let kill = conn.send(Request::KillAgent { id: agent.id }).await;
    let list = conn.send(Request::ListAgents).await;
    let first = conn
        .wait_for(|m| matches!(m, ServerMessage::Response { .. }))
        .await;
    assert!(
        matches!(first, ServerMessage::Response { id, .. } if id == list),
        "the kill was answered first: {first:?}"
    );
    assert!(matches!(
        conn.response(kill).await,
        Ok(Response::Agent(info)) if info.status == Status::Stopped
    ));
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_being_merged_refuses_other_lifecycle_requests() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "merging", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    let gate = Gate::fsmonitor(repo.path());
    gate.arm();
    let merge = conn.send(Request::MergeAgent { id: agent.id }).await;
    gate.entered().await;

    let mut other = Conn::open(daemon.path()).await;
    for request in [
        Request::RemoveAgent {
            id: agent.id,
            remove_worktree: true,
            force: true,
            delete_branch: false,
        },
        Request::MergeAgent { id: agent.id },
        Request::KillAgent { id: agent.id },
    ] {
        let err = other.request(request).await.expect_err("busy");
        assert!(err.contains("merging is being merged"), "{err}");
    }
    assert!(path.exists());

    gate.open();
    assert!(
        matches!(conn.response(merge).await, Ok(Response::Done(_))),
        "the merge failed"
    );
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill once merged");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worktree_being_created_is_recorded_until_its_agent_is() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let state = StatePaths::new(daemon.path()).state_file();
    let gate = Gate::post_checkout(repo.path());
    let create = conn
        .send(create_in_worktree_request(
            &project,
            "slow",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let during = std::fs::read_to_string(&state).expect("state");
    assert!(during.contains("[[pending_worktrees]]"), "{during}");
    assert!(during.contains("lazyagents/slow"), "{during}");
    gate.open();
    let Ok(Response::Agent(agent)) = conn.response(create).await else {
        panic!("create failed")
    };
    let after = std::fs::read_to_string(&state).expect("state");
    assert!(!after.contains("pending_worktrees"), "{after}");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_leftovers_of_an_unfinished_create_are_removed_at_the_next_start() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let leftover = worktree_path(&daemon, &project, "ghost");
    common::git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lazyagents/ghost",
            leftover.to_str().expect("utf8"),
            "main",
        ],
    );
    conn.request(Request::Shutdown).await.expect("shutdown");
    daemon.exits_within(WAIT).await;
    let state = StatePaths::new(daemon.path()).state_file();
    let mut text = std::fs::read_to_string(&state).expect("state");
    text.push_str(&format!(
        "\n[[pending_worktrees]]\nproject = \"{}\"\nname = \"ghost\"\npath = \"{}\"\nbranch = \"lazyagents/ghost\"\n",
        project.id,
        leftover.display()
    ));
    std::fs::write(&state, text).expect("write state");

    daemon.task = Some(spawn_daemon(daemon.path()).await);
    let deadline = tokio::time::Instant::now() + WAIT;
    while leftover.exists()
        || std::fs::read_to_string(&state)
            .expect("state")
            .contains("ghost")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leftover worktree was never removed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    let mut conn = Conn::open(daemon.path()).await;
    let agent = create_in_worktree(&mut conn, &project, "ghost", &["sleep", "30"])
        .await
        .expect("the name is free again");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_waiting_for_a_hung_create_and_the_next_start_cleans_up() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let gate = Gate::post_checkout(repo.path());
    let create = conn
        .send(create_in_worktree_request(
            &project,
            "hung",
            &["sleep", "30"],
        ))
        .await;
    gate.entered().await;

    let mut other = Conn::open(daemon.path()).await;
    let shutdown = other.send(Request::Shutdown).await;
    let answered = tokio::time::timeout(protocol::SHUTDOWN_WAIT, other.response(shutdown))
        .await
        .expect("shutdown waited for the hung create");
    assert_eq!(answered, Ok(Response::Ok));
    let err = conn.response(create).await.expect_err("never started");
    assert!(err.contains("shut down before hung started"), "{err}");
    daemon.exits_within(WAIT).await;

    gate.open();
    let left = gate.left.clone();
    tokio::task::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + WAIT;
        while !left.exists() {
            assert!(std::time::Instant::now() < deadline, "the hook never left");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(500));
    })
    .await
    .expect("join");
    daemon.task = Some(spawn_daemon(daemon.path()).await);
    let leftover = worktree_path(&daemon, &project, "hung");
    let deadline = tokio::time::Instant::now() + WAIT;
    while leftover.exists()
        || !common::git(repo.path(), &["branch", "--list", "lazyagents/*"]).is_empty()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the hung create's worktree was never removed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_gets_a_full_frame_when_it_reads_again() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let chatty = create(
        &mut conn,
        &project,
        "chatty",
        AgentKind::Generic,
        &[
            "sh",
            "-c",
            "i=0; while :; do i=$((i+1)); echo line-$i; done",
        ],
    )
    .await
    .expect("create");
    conn.request(Request::Resize {
        id: chatty.id,
        cols: 250,
        rows: 100,
    })
    .await
    .expect("resize");
    let mut stalled = Conn::open(daemon.path()).await;
    stalled
        .request(Request::Subscribe { id: chatty.id })
        .await
        .expect("subscribe");
    stalled
        .wait_for(|m| matches!(m, ServerMessage::Frame(frame) if frame.full))
        .await;
    stalled.pending.clear();

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        matches!(
            conn.request(Request::ListAgents).await,
            Ok(Response::Agents(_))
        ),
        "the daemon stopped answering"
    );
    stalled
        .wait_for(|m| matches!(m, ServerMessage::Frame(frame) if frame.full))
        .await;
    conn.request(Request::KillAgent { id: chatty.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_while_events_pile_up_is_disconnected() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let agent = create(
        &mut conn,
        &project,
        "busy",
        AgentKind::Claude,
        &["sh", "-c", "exec sleep 300"],
    )
    .await
    .expect("create");
    let mut stalled = Conn::open(daemon.path()).await;
    assert!(matches!(
        stalled.request(Request::ListAgents).await,
        Ok(Response::Agents(_))
    ));
    drop(conn);

    let hooks: u64 = 20_000;
    let hooker = UnixStream::connect(StatePaths::new(daemon.path()).socket())
        .await
        .expect("connect");
    let (mut read, mut write) = hooker.into_split();
    let answered = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counted = std::sync::Arc::clone(&answered);
    let reader = tokio::spawn(async move {
        loop {
            match read_message::<_, ServerMessage>(&mut read).await {
                Ok(Some(ServerMessage::Response { result, .. })) => {
                    result.expect("hook");
                    let done = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if done == hooks {
                        return;
                    }
                }
                Ok(Some(_)) => {}
                other => panic!("the hooking client lost its connection: {other:?}"),
            }
        }
    });
    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .expect("hello");
    let deadline = tokio::time::Instant::now() + WAIT * 2;
    for id in 0..hooks {
        while id >= answered.load(std::sync::atomic::Ordering::SeqCst) + 200 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the hooks were never all answered"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let kind = if id % 2 == 0 {
            HookKind::PreToolUse
        } else {
            HookKind::Stop
        };
        let request = Request::Hook {
            id: agent.id,
            kind,
            session_id: None,
            source: None,
            detail: None,
            subagent: None,
        };
        write_message(&mut write, &ClientMessage::Request { id, request })
            .await
            .expect("send hook");
    }
    tokio::time::timeout(WAIT * 2, reader)
        .await
        .expect("the hooks were never all answered")
        .expect("reader");

    let closed = tokio::time::timeout(WAIT, async {
        loop {
            match read_message::<_, ServerMessage>(&mut stalled.read).await {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => return,
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the stalled client was never disconnected");
    let mut conn = Conn::open(daemon.path()).await;
    assert!(matches!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(_))
    ));
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

fn record_pending_worktree(daemon: &TestDaemon, project: &Project, name: &str, path: &Path) {
    let state = StatePaths::new(daemon.path()).state_file();
    let mut text = std::fs::read_to_string(&state).expect("state");
    text.push_str(&format!(
        "\n[[pending_worktrees]]\nproject = \"{}\"\nname = \"{name}\"\npath = \"{}\"\nbranch = \"lazyagents/{name}\"\n",
        project.id,
        path.display()
    ));
    std::fs::write(&state, text).expect("write state");
}

async fn stop_daemon(daemon: &mut TestDaemon) {
    let mut conn = Conn::open(daemon.path()).await;
    conn.request(Request::Shutdown).await.expect("shutdown");
    daemon.exits_within(WAIT).await;
}

async fn wait_until_no_pending_worktree(daemon: &TestDaemon) {
    let state = StatePaths::new(daemon.path()).state_file();
    let deadline = tokio::time::Instant::now() + WAIT;
    while std::fs::read_to_string(&state)
        .expect("state")
        .contains("pending_worktrees")
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the pending worktree was never dropped"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leftover_git_will_not_let_go_of_is_retried_at_the_next_start() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let leftover = worktree_path(&daemon, &project, "ghost");
    let leftover_arg = leftover.to_str().expect("utf8");
    common::git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lazyagents/ghost",
            leftover_arg,
            "main",
        ],
    );
    common::git(repo.path(), &["worktree", "lock", leftover_arg]);
    drop(conn);
    stop_daemon(&mut daemon).await;
    record_pending_worktree(&daemon, &project, "ghost", &leftover);

    daemon.task = Some(spawn_daemon(daemon.path()).await);
    let mut conn = Conn::open(daemon.path()).await;
    let err = create_in_worktree(&mut conn, &project, "ghost", &["sleep", "30"])
        .await
        .expect_err("the leftover keeps its name");
    let leftover_text = leftover.display().to_string();
    assert!(err.contains(&leftover_text), "{err}");
    assert!(err.contains("removed at the next start"), "{err}");
    let err = conn
        .request(Request::RemoveProject { id: project.id })
        .await
        .expect_err("the leftover keeps its project");
    assert!(err.to_string().contains(&leftover_text), "{err}");
    assert!(
        err.to_string().contains("removed at the next start"),
        "{err}"
    );
    drop(conn);
    stop_daemon(&mut daemon).await;
    let state = StatePaths::new(daemon.path()).state_file();
    let kept = std::fs::read_to_string(&state).expect("state");
    assert!(kept.contains("lazyagents/ghost"), "{kept}");
    assert_ne!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/ghost"]),
        ""
    );

    common::git(repo.path(), &["worktree", "unlock", leftover_arg]);
    daemon.task = Some(spawn_daemon(daemon.path()).await);
    wait_until_no_pending_worktree(&daemon).await;
    assert_eq!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/*"]),
        ""
    );
    let mut conn = Conn::open(daemon.path()).await;
    let agent = create_in_worktree(&mut conn, &project, "ghost", &["sleep", "30"])
        .await
        .expect("the name is free again");
    conn.request(Request::KillAgent { id: agent.id })
        .await
        .expect("kill");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_worktree_an_agent_records_is_kept_at_the_next_start() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let agent = create_in_worktree(&mut conn, &project, "kept", &["sleep", "30"])
        .await
        .expect("create");
    let path = agent.worktree.as_ref().expect("worktree").path.clone();
    drop(conn);
    stop_daemon(&mut daemon).await;
    record_pending_worktree(&daemon, &project, "kept", &path);

    daemon.task = Some(spawn_daemon(daemon.path()).await);
    wait_until_no_pending_worktree(&daemon).await;
    assert!(path.exists());
    assert_ne!(
        common::git(repo.path(), &["branch", "--list", "lazyagents/kept"]),
        ""
    );
    let mut conn = Conn::open(daemon.path()).await;
    assert_eq!(list_agents(&mut conn).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_worktree_whose_repo_is_gone_is_dropped_at_the_next_start() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let leftover = worktree_path(&daemon, &project, "ghost");
    std::fs::create_dir_all(&leftover).expect("leftover directory");
    std::fs::write(leftover.join("file"), "x").expect("leftover file");
    drop(conn);
    stop_daemon(&mut daemon).await;
    record_pending_worktree(&daemon, &project, "ghost", &leftover);
    std::fs::remove_dir_all(repo.path()).expect("remove the repo");

    daemon.task = Some(spawn_daemon(daemon.path()).await);
    wait_until_no_pending_worktree(&daemon).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while leftover.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the leftover directory was never removed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut conn = Conn::open(daemon.path()).await;
    conn.request(Request::RemoveProject { id: project.id })
        .await
        .expect("the project can be removed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_leftover_outside_the_worktrees_dir_survives_its_dropped_entry() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, repo) = add_repo_project(&mut conn).await;
    let outside = tempfile::tempdir().expect("outside");
    let precious = outside.path().join("precious");
    std::fs::create_dir_all(&precious).expect("precious");
    drop(conn);
    stop_daemon(&mut daemon).await;
    record_pending_worktree(&daemon, &project, "ghost", &precious);
    std::fs::remove_dir_all(repo.path()).expect("remove the repo");

    daemon.task = Some(spawn_daemon(daemon.path()).await);
    wait_until_no_pending_worktree(&daemon).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(precious.exists());
}
