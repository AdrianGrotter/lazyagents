use std::path::{Path, PathBuf};
use std::time::Duration;

use daemon::{Config, run};
use protocol::{
    AgentId, AgentInfo, AgentKind, ClientMessage, CreateAgent, HookKind, InputEvent, Key, KeyCode,
    Modifiers, PROTOCOL_VERSION, Project, Request, Response, Screen, ServerMessage, StatePaths,
    Status, read_message, write_message,
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
        let dir = tempfile::tempdir().expect("tempdir");
        let task = Some(spawn_daemon(dir.path()).await);
        Self { dir, task }
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

async fn spawn_daemon(dir: &Path) -> JoinHandle<anyhow::Result<()>> {
    let config = Config {
        paths: StatePaths::new(dir),
        exe: PathBuf::from("/usr/bin/true"),
    };
    let task = tokio::spawn(run(config));
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
        let id = self.next_id;
        self.next_id += 1;
        write_message(&mut self.write, &ClientMessage::Request { id, request })
            .await
            .expect("send");
        loop {
            let message = self.next().await;
            match message {
                ServerMessage::Response { id: got, result } if got == id => return result,
                other => self.pending.push(other),
            }
        }
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

fn alive(marker: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", marker])
        .status()
        .is_ok_and(|s| s.success())
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
    conn.wait_status(agent.id, Status::Idle).await;
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
    conn.request(Request::RemoveAgent { id: agent.id })
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
    })
    .await
    .expect("hook");
    conn.wait_status(agent.id, Status::Working).await;
    conn.request(Request::Hook {
        id: agent.id,
        kind: HookKind::Notification,
        session_id: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn hook_for_unknown_agent_is_an_error() {
    let daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let err = conn
        .request(Request::Hook {
            id: AgentId::random(),
            kind: HookKind::Stop,
            session_id: None,
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
async fn shutdown_stops_agents_and_removes_socket() {
    let mut daemon = TestDaemon::start().await;
    let mut conn = Conn::open(daemon.path()).await;
    let (project, _dir) = add_project(&mut conn).await;
    let marker = format!("lazyagents-shutdown-{}", std::process::id());
    create(
        &mut conn,
        &project,
        "sleeper",
        AgentKind::Generic,
        &["sh", "-c", &format!("sleep 30; : {marker}")],
    )
    .await
    .expect("create");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !alive(&marker) {
        assert!(std::time::Instant::now() < deadline, "agent never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(conn.request(Request::Shutdown).await, Ok(Response::Ok));
    daemon
        .task
        .take()
        .expect("task")
        .await
        .expect("join")
        .expect("clean exit");
    assert!(!StatePaths::new(daemon.path()).socket().exists());
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while alive(&marker) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!alive(&marker), "agent survived shutdown");
}

#[tokio::test(flavor = "multi_thread")]
async fn second_daemon_refuses_to_start() {
    let daemon = TestDaemon::start().await;
    let err = run(Config {
        paths: StatePaths::new(daemon.path()),
        exe: PathBuf::from("/usr/bin/true"),
    })
    .await
    .expect_err("second daemon");
    assert!(format!("{err:#}").contains("already"), "{err:#}");
    let mut conn = Conn::open(daemon.path()).await;
    assert_eq!(
        conn.request(Request::ListAgents).await,
        Ok(Response::Agents(vec![]))
    );
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
    let err = run(Config {
        paths: StatePaths::new(dir.path()),
        exe: PathBuf::from("/usr/bin/true"),
    })
    .await
    .expect_err("corrupt");
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
