//! The `lazyagents` binary across process boundaries: argument parsing through to exit
//! codes and output, the daemon it starts, signals, hooks and the status line. The
//! commands themselves are tested in process, in `src/tests.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use protocol::{AgentInfo, AgentKind, CreateAgent, Request, Response, StatePaths};

const BIN: &str = env!("CARGO_BIN_EXE_lazyagents");

/// How long anything a test waits for may take. With the suite running in parallel each
/// spawned process can take seconds, and polling returns as soon as the condition holds.
const WAIT: Duration = Duration::from_secs(30);

/// A command without what a shell's direnv hook sets, so the direnv state of the shell
/// running the tests never reaches it.
fn without_direnv(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for var in ["DIRENV_DIFF", "DIRENV_DIR", "DIRENV_FILE", "DIRENV_WATCHES"] {
        command.env_remove(var);
    }
    command
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// Sends `request` to the daemon of `paths`, as the CLI would without launching it.
fn request(paths: &StatePaths, request: Request) -> Response {
    block_on(async {
        let mut client = client::Client::connect(paths).await.expect("connect");
        client.request(request).await.expect("request")
    })
}

/// Waits until the daemon of `paths` answers; its socket file appears before it listens.
fn wait_for_daemon(paths: &StatePaths) {
    assert!(
        wait_until(WAIT, || block_on(client::Client::connect(paths)).is_ok()),
        "daemon never answered"
    );
}

/// Starts an agent running `command` in a new project at `project`.
fn start_agent(
    paths: &StatePaths,
    project: &Path,
    name: &str,
    kind: AgentKind,
    command: Vec<String>,
) -> AgentInfo {
    let added = match request(
        paths,
        Request::AddProject {
            path: project.to_path_buf(),
            name: None,
        },
    ) {
        Response::Project(added) => added,
        other => panic!("unexpected reply to AddProject: {other:?}"),
    };
    let create = Request::CreateAgent(CreateAgent {
        project: added.id,
        name: name.into(),
        kind,
        command,
        cols: 80,
        rows: 24,
        worktree: None,
        prompt: None,
        setup: None,
    });
    match request(paths, create) {
        Response::Agent(info) => info,
        other => panic!("unexpected reply to CreateAgent: {other:?}"),
    }
}

struct TestEnv {
    state_dir: PathBuf,
    _state: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let state = tempfile::tempdir().expect("state");
        Self {
            state_dir: state.path().to_path_buf(),
            _state: state,
            project: tempfile::tempdir().expect("project"),
        }
    }

    /// A state dir that does not exist yet.
    fn without_state_dir() -> Self {
        let parent = tempfile::tempdir().expect("parent");
        Self {
            state_dir: parent.path().join("state"),
            _state: parent,
            project: tempfile::tempdir().expect("project"),
        }
    }

    fn paths(&self) -> StatePaths {
        StatePaths::new(&self.state_dir)
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        without_direnv(BIN)
            .args(args)
            .envs(env.iter().copied())
            .env("LAZYAGENTS_STATE_DIR", &self.state_dir)
            .current_dir(self.project.path())
            .stdin(Stdio::null())
            .output()
            .expect("run lazyagents")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "lazyagents {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn socket(&self) -> PathBuf {
        self.paths().socket()
    }

    fn daemon_pid(&self) -> i32 {
        let contents = std::fs::read_to_string(self.paths().lock_file()).expect("lock file");
        let holder = protocol::lockfile::parse(&contents).expect("pid in lock file");
        assert!(
            protocol::lockfile::is_current(&holder),
            "{contents:?} is not the running daemon"
        );
        i32::try_from(holder.pid).expect("pid fits i32")
    }
}

/// Shuts down any daemon the test left running, stopping its agents.
impl Drop for TestEnv {
    fn drop(&mut self) {
        let paths = self.paths();
        let asked = block_on(async {
            match client::Client::connect(&paths).await {
                Ok(mut client) => client.request(Request::Shutdown).await.is_ok(),
                Err(_) => false,
            }
        });
        if asked {
            wait_until(WAIT, || !paths.socket().exists());
        }
    }
}

fn pid_alive(pid: u32) -> bool {
    rustix::process::Pid::from_raw(pid as i32)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

/// Pids an agent reported; SIGKILLed on drop if the test panicked, so a failure leaks nothing.
struct Leaves(Vec<u32>);

impl Leaves {
    fn read(path: &Path, count: usize) -> Self {
        let mut pids = Vec::new();
        let listed = wait_until(WAIT, || {
            pids = std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect();
            pids.len() >= count
        });
        let leaves = Self(pids);
        assert!(listed, "{} never listed {count} pids", path.display());
        leaves
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

fn sh(script: String) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script]
}

/// Agents that each leave a process a plain stop of the agent would miss: one ignoring
/// SIGHUP, a job-control background job, and a descendant in a session of its own.
fn start_tree_agents(paths: &StatePaths, project: &Path) -> Leaves {
    let pidfile = project.join("pids");
    let pids = pidfile.display();
    for (name, script) in [
        (
            "nohup",
            format!("trap '' HUP; echo $$ >> '{pids}'; sleep 301"),
        ),
        (
            "jobs",
            format!("set -m; sleep 302 & echo $! >> '{pids}'; wait"),
        ),
        (
            "setsid",
            format!(
                "perl -MPOSIX -e 'POSIX::setsid(); exec \"sleep\", \"303\"' & echo $! >> '{pids}'; wait"
            ),
        ),
    ] {
        start_agent(paths, project, name, AgentKind::Generic, sh(script));
    }
    Leaves::read(&pidfile, 3)
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

/// What a test agent wrote to `path`, once it ends with `end`.
fn wait_for_file_ending(path: &Path, end: &str) -> String {
    let mut text = String::new();
    assert!(
        wait_until(WAIT, || {
            text = std::fs::read_to_string(path).unwrap_or_default();
            text.ends_with(end)
        }),
        "{} was never written",
        path.display()
    );
    text
}

/// A line a test agent wrote to `path`, once it is complete.
fn wait_for_line(path: &Path) -> String {
    wait_for_file_ending(path, "\n").trim().to_string()
}

/// A `lazyagents daemon` child, SIGKILLed by its pid on drop if it is still running.
struct DaemonChild(std::process::Child);

impl DaemonChild {
    fn start(env: &TestEnv, vars: &[(&str, &str)]) -> Self {
        let daemon = DaemonChild(
            without_direnv(BIN)
                .arg("daemon")
                .env("LAZYAGENTS_STATE_DIR", &env.state_dir)
                .envs(vars.iter().copied())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start daemon"),
        );
        wait_for_daemon(&env.paths());
        daemon
    }

    fn exited(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(Some(_)))
    }
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn the_cli_starts_a_daemon_that_runs_agents_as_itself_and_stops_it() {
    let env = TestEnv::new();
    drop(std::os::unix::net::UnixListener::bind(env.socket()).expect("stale socket"));
    let dump = env.project.path().join("env");
    let out = env.run_with_env(
        &[
            "new",
            "dump",
            "--kind",
            "claude",
            "--",
            "sh",
            "-c",
            &format!("{{ env; echo EOF; }} > '{}'", dump.display()),
        ],
        &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")],
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "started dump\n");

    let vars = wait_for_file_ending(&dump, "EOF\n");
    let leaked: Vec<&str> = vars
        .lines()
        .filter(|line| line.starts_with("CLAUDECODE=") || line.starts_with("CLAUDE_CODE_"))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    let id: uuid::Uuid = vars
        .lines()
        .find_map(|line| line.strip_prefix("LAZYAGENTS_AGENT_ID="))
        .unwrap_or_else(|| panic!("no agent id in {vars}"))
        .parse()
        .expect("agent id");
    let settings = std::fs::read_to_string(env.paths().hook_settings(protocol::AgentId(id)))
        .expect("hook settings");
    assert!(
        settings.contains(&format!("'{BIN}' hook UserPromptSubmit")),
        "{settings}"
    );

    assert!(env.ok(&["shutdown"]).contains("daemon stopped"));
    assert!(!env.socket().exists());
    assert!(env.ok(&["shutdown"]).contains("daemon is not running"));
}

#[test]
fn an_auto_started_daemon_gets_a_private_state_dir_and_its_idle_exit_from_the_environment() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::without_state_dir();
    let out = env.run_with_env(&["ls"], &[("LAZYAGENTS_IDLE_EXIT_SECS", "1")]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no projects yet"),
        "{out:?}"
    );
    let mode = std::fs::metadata(&env.state_dir).map(|m| m.permissions().mode());
    assert_eq!(mode.expect("state dir") & 0o777, 0o700);
    assert!(
        wait_until(WAIT, || !env.socket().exists()),
        "idle daemon kept running"
    );
}

/// The client notices the daemon exit before its 5 s startup wait runs out; the message
/// says so.
#[test]
fn a_daemon_that_dies_at_startup_fails_fast_and_logs_why() {
    let env = TestEnv::new();
    std::fs::write(env.paths().state_file(), "[[[ not toml").expect("corrupt");
    let out = env.run(&["ls"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        stderr.contains(&format!(
            "daemon exited during startup; see {}",
            env.paths().log_file().display()
        )),
        "{stderr}"
    );
    let log = std::fs::read_to_string(env.paths().log_file()).unwrap_or_default();
    assert!(log.contains("state.toml"), "{log}");
}

#[test]
fn sigterm_stops_the_daemon_and_its_agents() {
    let env = TestEnv::new();
    let mut daemon = DaemonChild::start(&env, &[]);
    let leaves = start_tree_agents(&env.paths(), env.project.path());
    let pid = rustix::process::Pid::from_child(&daemon.0);
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("SIGTERM");
    assert!(
        wait_until(WAIT, || !env.socket().exists()),
        "socket survived SIGTERM"
    );
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived SIGTERM");
    assert!(wait_until(WAIT, || daemon.exited()), "daemon still running");
}

#[test]
fn stop_daemon_stops_a_daemon_started_in_another_locale_and_time_zone() {
    let env = TestEnv::new();
    let mut daemon = DaemonChild::start(&env, &[("TZ", "Asia/Tokyo"), ("LC_ALL", "ja_JP.UTF-8")]);
    client::stop_daemon(&env.paths(), Duration::from_secs(20)).expect("stop");
    assert!(!env.socket().exists(), "socket survived stop_daemon");
    assert!(wait_until(WAIT, || daemon.exited()), "daemon still running");
}

#[test]
fn no_subcommand_without_a_terminal_fails_cleanly() {
    let env = TestEnv::new();
    let started = Instant::now();
    let out = env.run(&[]);
    assert!(started.elapsed() < WAIT, "{out:?}");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("lazyagents: "), "{out:?}");
    assert!(stderr.contains("needs an interactive terminal"), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(!env.socket().exists(), "a daemon started");
    assert!(!env.paths().lock_file().exists(), "a daemon started");
}

#[test]
fn hook_is_silent_and_does_not_wait_on_an_open_stdin() {
    let dir = tempfile::tempdir().expect("dir");
    let mut child = Command::new(BIN)
        .args(["hook", "Stop"])
        .env(
            "LAZYAGENTS_AGENT_ID",
            "00000000-0000-4000-8000-000000000000",
        )
        .env("LAZYAGENTS_SOCKET", dir.path().join("missing.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook");
    let _stdin = child.stdin.take();
    let pid = rustix::process::Pid::from_child(&child);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(child.wait_with_output()));
    let Ok(out) = receiver.recv_timeout(WAIT) else {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        panic!("hook blocked on stdin");
    };
    let out = out.expect("hook output");
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}

/// A daemon on a runtime of the test process, shut down on drop.
struct InProcessDaemon {
    state: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
    task: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl InProcessDaemon {
    fn start() -> Self {
        let state = tempfile::tempdir().expect("state");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let task = runtime.spawn(daemon::run(daemon::Config {
            paths: StatePaths::new(state.path()),
            exe: PathBuf::from(BIN),
            idle_exit: None,
            direnv: None,
            claude_config: None,
            timings: daemon::Timings::default(),
        }));
        wait_for_daemon(&StatePaths::new(state.path()));
        Self {
            state,
            runtime,
            task: Some(task),
        }
    }

    fn paths(&self) -> StatePaths {
        StatePaths::new(self.state.path())
    }

    fn agents(&self) -> Vec<AgentInfo> {
        match request(&self.paths(), Request::ListAgents) {
            Response::Agents(agents) => agents,
            other => panic!("unexpected reply to ListAgents: {other:?}"),
        }
    }
}

impl Drop for InProcessDaemon {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        let paths = self.paths();
        self.runtime.block_on(async {
            if let Ok(mut client) = client::Client::connect(&paths).await {
                let _ = client.request(Request::Shutdown).await;
            }
            let _ = tokio::time::timeout(WAIT, task).await;
        });
    }
}

/// The status line gives up reporting after a short budget, as Claude runs it again
/// soon; under load a report can miss it, so the test runs it a few times too.
#[test]
fn the_status_line_shows_the_users_and_reports_to_the_daemon() {
    let daemon = InProcessDaemon::start();
    let project = tempfile::tempdir().expect("project");
    let agent = start_agent(
        &daemon.paths(),
        project.path(),
        "c",
        AgentKind::Claude,
        sh("read _".into()),
    );
    let claude_config = tempfile::tempdir().expect("claude config");
    std::fs::write(
        claude_config.path().join("settings.json"),
        r#"{"statusLine": {"type": "command", "command": "printf 'mine:'; head -c 11; exit 3"}}"#,
    )
    .expect("settings");
    let payload =
        r#"{"model":{"display_name":"Opus"},"cost":{"total_cost_usd":0.42},"cwd":"/nowhere"}"#;
    let reported = |daemon: &InProcessDaemon| {
        daemon.agents().iter().any(|a| {
            a.id == agent.id
                && a.usage.as_ref().map(|usage| usage.summary()) == Some("Opus · $0.42".into())
        })
    };
    let mut attempts = 0;
    while attempts < 5 && !reported(&daemon) {
        attempts += 1;
        let mut child = Command::new(BIN)
            .arg("statusline")
            .env("CLAUDE_CONFIG_DIR", claude_config.path())
            .env("LAZYAGENTS_AGENT_ID", agent.id.to_string())
            .env("LAZYAGENTS_SOCKET", daemon.paths().socket())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("statusline");
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("wait");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "mine:{\"model\":{\"");
        assert_eq!(out.status.code(), Some(3));
        wait_until(Duration::from_secs(2), || reported(&daemon));
    }
    assert!(reported(&daemon), "{:?}", daemon.agents());
}

/// direnv's home, data, config and cache dirs inside `root`, so the user's allow list and
/// config are never read or written.
fn isolated_direnv_home(root: &Path) -> Vec<(String, String)> {
    ["data", "config", "cache"]
        .iter()
        .map(|name| root.join(name))
        .for_each(|dir| std::fs::create_dir_all(dir).expect("xdg dir"));
    let path = |name: &str| root.join(name).display().to_string();
    vec![
        ("HOME".into(), root.display().to_string()),
        ("XDG_DATA_HOME".into(), path("data")),
        ("XDG_CONFIG_HOME".into(), path("config")),
        ("XDG_CACHE_HOME".into(), path("cache")),
        ("DIRENV_CONFIG".into(), path("config")),
    ]
}

/// The variables a shell's direnv hook sets on entering `dir`, after allowing its `.envrc`.
fn direnv_loaded_env(dir: &Path, base: &[(String, String)]) -> Vec<(String, String)> {
    let direnv = daemon::find_in_path("direnv").expect("direnv in the nix dev shell");
    let direnv_command = || {
        let mut command = without_direnv(&direnv);
        command
            .envs(base.iter().map(|(k, v)| (k, v)))
            .current_dir(dir)
            .stdin(Stdio::null());
        command
    };
    let allowed = direnv_command()
        .arg("allow")
        .output()
        .expect("direnv allow");
    assert!(allowed.status.success(), "{allowed:?}");
    let exported = direnv_command()
        .args(["export", "json"])
        .output()
        .expect("direnv export");
    assert!(exported.status.success(), "{exported:?}");
    let vars: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&exported.stdout).expect("export json sets every variable");
    vars.into_iter().collect()
}

#[test]
fn agents_do_not_inherit_the_direnv_environment_the_daemon_started_in() {
    let env = TestEnv::new();
    let home = tempfile::tempdir().expect("direnv home");
    let mut base = isolated_direnv_home(home.path());
    base.push(("LAZYAGENTS_TEST_ORIG".into(), "original".into()));
    let other = tempfile::tempdir().expect("other project");
    std::fs::write(
        other.path().join(".envrc"),
        "export LAZYAGENTS_TEST_LEAK=leaked\nexport LAZYAGENTS_TEST_ORIG=changed\n",
    )
    .expect("write .envrc");
    let loaded = direnv_loaded_env(other.path(), &base);
    assert!(loaded.iter().any(|(k, _)| k == "DIRENV_DIFF"), "{loaded:?}");
    let shell: Vec<(&str, &str)> = base
        .iter()
        .chain(&loaded)
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let seen = env.project.path().join("seen");
    let script = format!(
        "echo \"${{LAZYAGENTS_TEST_LEAK-unset}} ${{LAZYAGENTS_TEST_ORIG-unset}}\" > '{}'",
        seen.display()
    );
    let out = env.run_with_env(&["new", "seer", "--", "sh", "-c", &script], &shell);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(wait_for_line(&seen), "unset original");
}

#[test]
fn an_unreadable_direnv_diff_still_starts_agents() {
    let env = TestEnv::new();
    let home = tempfile::tempdir().expect("direnv home");
    let base = isolated_direnv_home(home.path());
    let mut shell: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    shell.extend([("DIRENV_DIFF", "garbage"), ("DIRENV_DIR", "-/nowhere")]);
    let started = env.project.path().join("started");
    let script = format!("echo started > '{}'", started.display());
    let out = env.run_with_env(&["new", "starter", "--", "sh", "-c", &script], &shell);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(wait_for_line(&started), "started");
}

fn serve_foreign_protocol(socket: PathBuf) {
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).expect("listener");
            while let Ok((mut stream, _)) = listener.accept().await {
                let welcome = protocol::ServerMessage::Welcome {
                    protocol_version: 999,
                    daemon_version: "9.9.9".into(),
                };
                let _ = protocol::write_message(&mut stream, &welcome).await;
            }
        });
    });
}

/// A stand-in for a daemon of another protocol: `sleep` holds its pid and the test holds
/// the lock for it; once `sleep` exits the socket goes and the lock is released, as a real
/// daemon's would. SIGKILLed on drop if it is still running, so a failing test leaks nothing.
struct ForeignDaemon(Arc<Mutex<std::process::Child>>);

impl ForeignDaemon {
    fn start(env: &TestEnv) -> Self {
        serve_foreign_protocol(env.socket());
        let sleeper = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        let lock = env.paths().lock_file();
        std::fs::write(&lock, sleeper.id().to_string()).expect("lock file");
        let held = std::fs::File::open(&lock).expect("open lock file");
        held.lock().expect("hold the lock");
        let fake = ForeignDaemon(Arc::new(Mutex::new(sleeper)));
        let watched = Arc::clone(&fake.0);
        let socket = env.socket();
        std::thread::spawn(move || {
            while !exited(&watched) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = std::fs::remove_file(socket);
            drop(held);
        });
        fake
    }
}

fn exited(child: &Mutex<std::process::Child>) -> bool {
    let mut child = child.lock().unwrap_or_else(PoisonError::into_inner);
    !matches!(child.try_wait(), Ok(None))
}

impl Drop for ForeignDaemon {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The TUI in a detached session of `scripts/tmux-harness.sh`, on a tmux server whose
/// socket lives in a temp dir of its own; the session is stopped on drop.
struct Tui {
    sockets: tempfile::TempDir,
}

impl Tui {
    const SESSION: &str = "tui";

    fn start(env: &TestEnv, config: &str) -> Self {
        let config_file = env.state_dir.join("tui-config.toml");
        std::fs::write(&config_file, config).expect("tui config");
        let tui = Tui {
            sockets: tempfile::tempdir().expect("tmux socket dir"),
        };
        let command = format!(
            "cd '{}' && exec env LAZYAGENTS_STATE_DIR='{}' LAZYAGENTS_CONFIG='{}' SHELL=/bin/sh '{BIN}'",
            env.project.path().display(),
            env.state_dir.display(),
            config_file.display(),
        );
        tui.harness(&[
            "start",
            Self::SESSION,
            "120",
            "32",
            "--",
            "sh",
            "-c",
            &command,
        ]);
        tui
    }

    fn command(&self) -> Command {
        let mut command = without_direnv(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/tmux-harness.sh"
        ));
        command
            .env("TMUX_TMPDIR", self.sockets.path())
            .env("LAZYAGENTS_HARNESS_SERVER", "lazyagents-cli-test")
            .stdin(Stdio::null());
        command
    }

    fn harness(&self, args: &[&str]) {
        let out = self
            .command()
            .args(args)
            .output()
            .expect("run tmux-harness.sh");
        assert!(
            out.status.success(),
            "tmux-harness.sh {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn keys(&self, keys: &[&str]) {
        self.harness(&[&["keys", Self::SESSION], keys].concat());
    }

    fn type_text(&self, text: &str) {
        self.harness(&["type", Self::SESSION, text]);
    }

    fn wait_for(&self, text: &str) {
        self.harness(&["wait-for", Self::SESSION, text, &WAIT.as_secs().to_string()]);
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.command().args(["stop", Self::SESSION]).output();
    }
}

#[test]
fn the_tui_restarts_a_daemon_of_another_protocol_when_asked() {
    let env = TestEnv::new();
    let fake = ForeignDaemon::start(&env);
    let tui = Tui::start(&env, "");
    tui.wait_for("Restart the daemon? This stops every running agent.");
    tui.wait_for("y restart · n quit");
    tui.keys(&["y"]);
    tui.wait_for("press a to add a project");
    assert!(exited(&fake.0), "the old daemon still runs");
    env.daemon_pid();
    assert_eq!(
        request(&env.paths(), Request::ListProjects),
        Response::Projects(Vec::new())
    );
}

#[test]
fn a_terminal_command_gets_the_keyboard_until_it_exits() {
    let env = TestEnv::new();
    let answer = env.project.path().join("answer");
    let typed = env.project.path().join("typed");
    let config = format!(
        r#"[[commands]]
key = "T"
description = "ask"
context = "project"
command = '''printf 'answer? '; until [ -e '{typed}' ]; do sleep 0.05; done; sleep 0.5; read line; echo "got:$line" > '{answer}'; exit 4'''
"#,
        typed = typed.display(),
        answer = answer.display()
    );
    let tui = Tui::start(&env, &config);
    tui.wait_for("press a to add a project");
    tui.keys(&["a"]);
    tui.wait_for(" Add project ");
    tui.keys(&["Enter"]);
    tui.wait_for("no agents — press n");
    tui.keys(&["T"]);
    tui.wait_for("answer?");
    tui.type_text("hello");
    tui.keys(&["Enter"]);
    std::fs::write(&typed, "").expect("typed marker");
    assert_eq!(wait_for_line(&answer), "got:hello");
    tui.wait_for("ask exited with 4");
    tui.keys(&["n"]);
    tui.wait_for(" New agent ");
}
