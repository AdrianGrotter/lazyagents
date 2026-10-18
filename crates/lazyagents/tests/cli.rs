use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

struct TestEnv {
    state: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        Self {
            state: tempfile::tempdir().expect("state"),
            project: tempfile::tempdir().expect("project"),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        without_direnv(BIN)
            .args(args)
            .envs(env.iter().copied())
            .env("LAZYAGENTS_STATE_DIR", self.state.path())
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
        self.state.path().join("daemon.sock")
    }

    fn daemon_pid(&self) -> i32 {
        let contents =
            std::fs::read_to_string(self.state.path().join("daemon.lock")).expect("lock file");
        let holder = protocol::lockfile::parse(&contents).expect("pid in lock file");
        assert!(
            protocol::lockfile::is_current(&holder),
            "{contents:?} is not the running daemon"
        );
        i32::try_from(holder.pid).expect("pid fits i32")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = self.run(&["shutdown"]);
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

fn start_tree_agents(env: &TestEnv, pidfile: &Path) -> Leaves {
    let pidfile = pidfile.display();
    env.ok(&[
        "new",
        "nohup",
        "--",
        "sh",
        "-c",
        &format!("trap '' HUP; echo $$ >> '{pidfile}'; sleep 301"),
    ]);
    env.ok(&[
        "new",
        "jobs",
        "--",
        "sh",
        "-c",
        &format!("set -m; sleep 302 & echo $! >> '{pidfile}'; wait"),
    ]);
    env.ok(&[
        "new",
        "setsid",
        "--",
        "sh",
        "-c",
        &format!(
            "perl -MPOSIX -e 'POSIX::setsid(); exec \"sleep\", \"303\"' & echo $! >> '{pidfile}'; wait"
        ),
    ]);
    Leaves::read(Path::new(&pidfile.to_string()), 3)
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

fn wait_for_file(path: &Path) -> String {
    let mut text = String::new();
    assert!(
        wait_until(WAIT, || {
            text = std::fs::read_to_string(path).unwrap_or_default();
            !text.is_empty()
        }),
        "{} never appeared",
        path.display()
    );
    text
}

#[test]
fn prints_version() {
    let out = Command::new(BIN)
        .arg("--version")
        .output()
        .expect("binary runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("lazyagents {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn new_types_the_prompt_into_a_generic_agent() {
    let env = TestEnv::new();
    let typed = env.project.path().join("typed");
    let script = format!(
        "printf 'ready> '; read line; echo \"$line\" > '{0}.tmp'; mv '{0}.tmp' '{0}'; sleep 30",
        typed.display()
    );
    env.ok(&[
        "new",
        "asker",
        "--prompt",
        "hello there",
        "--",
        "sh",
        "-c",
        &script,
    ]);
    assert_eq!(wait_for_file(&typed).trim(), "hello there");
    env.ok(&["shutdown"]);
}

#[test]
fn the_status_line_shows_the_users_and_reports_to_the_daemon() {
    let env = TestEnv::new();
    let claude_config = tempfile::tempdir().expect("claude config");
    std::fs::write(
        claude_config.path().join("settings.json"),
        r#"{"statusLine": {"type": "command", "command": "printf 'mine:'; head -c 11; exit 3"}}"#,
    )
    .expect("settings");
    env.ok(&[
        "new",
        "c",
        "--kind",
        "claude",
        "--",
        "sh",
        "-c",
        "exec sleep 30",
    ]);
    let state = std::fs::read_to_string(env.state.path().join("state.toml")).expect("state");
    let id = state
        .lines()
        .skip_while(|line| line.trim() != "[[agents]]")
        .find_map(|line| line.strip_prefix("id = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|id| id.len() == 36)
        .expect("agent id");
    let payload =
        r#"{"model":{"display_name":"Opus"},"cost":{"total_cost_usd":0.42},"cwd":"/nowhere"}"#;
    let mut child = Command::new(BIN)
        .arg("statusline")
        .env("CLAUDE_CONFIG_DIR", claude_config.path())
        .env("LAZYAGENTS_AGENT_ID", id)
        .env("LAZYAGENTS_SOCKET", env.socket())
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
    assert!(
        wait_until(WAIT, || env.ok(&["ls"]).contains("(Opus · $0.42)")),
        "{}",
        env.ok(&["ls"])
    );
}

#[test]
fn send_types_a_line_into_an_agent() {
    let env = TestEnv::new();
    let typed = env.project.path().join("typed");
    let script = format!(
        "read line; echo \"$line\" > '{0}.tmp'; mv '{0}.tmp' '{0}'; sleep 30",
        typed.display()
    );
    env.ok(&["new", "listener", "--", "sh", "-c", &script]);
    assert!(
        env.ok(&["send", "listener", "use pnpm"])
            .contains("sent to listener")
    );
    assert_eq!(wait_for_file(&typed).trim(), "use pnpm");
    env.ok(&["shutdown"]);
}

#[test]
fn lifecycle_through_the_cli() {
    let env = TestEnv::new();
    assert!(env.ok(&["ls"]).contains("no projects yet"));
    assert!(
        env.ok(&["new", "demo", "--", "sh", "-c", "echo hi; sleep 30"])
            .contains("started demo")
    );
    let ls = env.ok(&["ls"]);
    let project_name = env
        .project
        .path()
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    assert!(ls.contains(&project_name), "{ls}");
    assert!(ls.contains("demo"), "{ls}");
    assert!(env.ok(&["kill", "demo"]).contains("stopped demo"));
    assert!(env.ok(&["ls"]).contains("stopped"));
    assert!(env.ok(&["restart", "demo"]).contains("restarted demo"));
    assert!(
        env.ok(&["rename", "demo", "demo2"])
            .contains("renamed demo to demo2")
    );
    assert!(
        env.ok(&["rm", &format!("{project_name}/demo2")])
            .contains("removed demo2")
    );
    assert!(!env.ok(&["ls"]).contains("demo2"));
    assert!(env.ok(&["shutdown"]).contains("daemon stopped"));
    assert!(!env.socket().exists());
    assert!(env.ok(&["shutdown"]).contains("daemon is not running"));
}

#[test]
fn errors_go_to_stderr_with_exit_code_one() {
    let env = TestEnv::new();
    let out = env.run(&["kill", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("lazyagents: "),
        "{out:?}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn shutdown_leaves_no_agent_processes() {
    let env = TestEnv::new();
    let leaves = start_tree_agents(&env, &env.project.path().join("pids"));
    assert!(env.ok(&["shutdown"]).contains("daemon stopped"));
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived shutdown");
}

struct FakeClaude {
    args_file: PathBuf,
    agent_id: String,
    session: String,
}

impl FakeClaude {
    fn start(env: &TestEnv) -> Self {
        let args_file = env.project.path().join("args");
        let fake = env.project.path().join("claude");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexec sleep 30\n",
                args_file.display()
            ),
        )
        .expect("fake claude");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");

        env.ok(&[
            "new",
            "c",
            "--kind",
            "claude",
            "--",
            fake.to_str().expect("utf8"),
        ]);
        let args = wait_for_file(&args_file);
        let lines: Vec<&str> = args.lines().collect();
        assert_eq!(lines[0], "--session-id");
        let session = lines[1].to_string();
        let settings = PathBuf::from(lines[3]);
        let agent_id = settings
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .into_owned();
        let settings_json = std::fs::read_to_string(&settings).expect("settings");
        assert!(
            settings_json.contains(&format!("'{BIN}' hook UserPromptSubmit")),
            "{settings_json}"
        );
        Self {
            args_file,
            agent_id,
            session,
        }
    }

    fn hook(&self, env: &TestEnv, event: &str, payload: &str) {
        let mut child = Command::new(BIN)
            .args(["hook", event])
            .env("LAZYAGENTS_AGENT_ID", &self.agent_id)
            .env("LAZYAGENTS_SOCKET", env.socket())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("hook");
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(payload.as_bytes())
            .expect("payload");
        let out = child.wait_with_output().expect("hook output");
        assert!(
            out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(),
            "{out:?}"
        );
    }
}

#[test]
fn claude_agent_hooks_drive_status_and_restart_resumes() {
    let env = TestEnv::new();
    let claude = FakeClaude::start(&env);
    let args_file = &claude.args_file;
    let session = claude.session.clone();
    let hook = |event: &str, payload: &str| claude.hook(&env, event, payload);
    let payload = format!("{{\"session_id\":\"{session}\"}}");
    hook("UserPromptSubmit", &payload);
    assert!(wait_until(WAIT, || env.ok(&["ls"]).contains("working")));
    hook("Notification", &payload);
    assert!(wait_until(WAIT, || env.ok(&["ls"]).contains("needs input")));

    std::fs::remove_file(args_file).expect("reset");
    env.ok(&["restart", "c"]);
    let resumed = wait_for_file(args_file);
    assert_eq!(
        resumed.lines().take(2).collect::<Vec<_>>(),
        ["--resume", session.as_str()]
    );

    let other = "6f1c2b9e-1d2a-4c3b-9e8f-0a1b2c3d4e5f";
    hook(
        "SessionStart",
        &format!("{{\"session_id\":\"{other}\",\"source\":\"resume\"}}"),
    );
    std::fs::remove_file(args_file).expect("reset");
    env.ok(&["restart", "c"]);
    let resumed = wait_for_file(args_file);
    assert_eq!(
        resumed.lines().take(2).collect::<Vec<_>>(),
        ["--resume", other]
    );
}

#[test]
fn idle_notification_marks_a_claude_agent_idle() {
    let env = TestEnv::new();
    let claude = FakeClaude::start(&env);
    let session = &claude.session;
    claude.hook(
        &env,
        "UserPromptSubmit",
        &format!("{{\"session_id\":\"{session}\"}}"),
    );
    assert!(wait_until(WAIT, || env.ok(&["ls"]).contains("working")));
    claude.hook(
        &env,
        "Notification",
        &format!(
            "{{\"session_id\":\"{session}\",\"notification_type\":\"idle_prompt\",\"message\":\"Claude is waiting for your input\"}}"
        ),
    );
    assert!(wait_until(WAIT, || env.ok(&["ls"]).contains("idle")));
    assert!(!env.ok(&["ls"]).contains("needs input"));
}

#[test]
fn a_claude_agent_stays_working_while_a_background_subagent_runs() {
    let env = TestEnv::new();
    let claude = FakeClaude::start(&env);
    let session = &claude.session;
    let main = format!("{{\"session_id\":\"{session}\"}}");
    let sub = format!(
        "{{\"session_id\":\"{session}\",\"agent_id\":\"a853c9dd5f0c291b7\",\"agent_type\":\"general-purpose\"}}"
    );
    claude.hook(&env, "UserPromptSubmit", &main);
    claude.hook(&env, "SubagentStart", &sub);
    claude.hook(&env, "Stop", &main);
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("working")));
    claude.hook(&env, "SubagentStop", &sub);
    claude.hook(&env, "Stop", &main);
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("idle")));
}

/// Runs the binary once, so a timed run measures it rather than the first exec of a
/// newly linked binary, which macOS holds up for its code-signature assessment.
fn warm_up() {
    let out = Command::new(BIN)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .expect("run lazyagents --version");
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn hook_is_silent_and_returns_without_daemon() {
    warm_up();
    let start = Instant::now();
    let out = Command::new(BIN)
        .args(["hook", "Stop"])
        .env(
            "LAZYAGENTS_AGENT_ID",
            "00000000-0000-4000-8000-000000000000",
        )
        .env("LAZYAGENTS_SOCKET", "/nonexistent/lazyagents.sock")
        .stdin(Stdio::null())
        .output()
        .expect("hook");
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "took {:?}",
        start.elapsed()
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}

#[test]
fn cli_recovers_from_a_stale_socket() {
    let env = TestEnv::new();
    drop(std::os::unix::net::UnixListener::bind(env.socket()).expect("stale socket"));
    assert!(env.ok(&["ls"]).contains("no projects yet"));
}

#[test]
fn project_add_relative_path_uses_the_callers_cwd() {
    let env = TestEnv::new();
    assert!(env.ok(&["ls"]).contains("no projects yet"));
    let other = tempfile::tempdir().expect("other");
    std::fs::create_dir(other.path().join("sub")).expect("sub");
    let out = Command::new(BIN)
        .args(["project", "add", "sub"])
        .env("LAZYAGENTS_STATE_DIR", env.state.path())
        .current_dir(other.path())
        .stdin(Stdio::null())
        .output()
        .expect("run lazyagents");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = other.path().canonicalize().expect("canonical").join("sub");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&expected.display().to_string()), "{stdout}");
}

#[test]
fn daemon_startup_errors_are_logged() {
    let state = tempfile::tempdir().expect("state");
    std::fs::write(state.path().join("state.toml"), "[[[ not toml").expect("corrupt");
    let mut child = Command::new(BIN)
        .arg("daemon")
        .env("LAZYAGENTS_STATE_DIR", state.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("daemon");
    let mut status = None;
    let exited = wait_until(WAIT, || {
        status = child.try_wait().expect("try_wait");
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(exited, "daemon did not exit");
    assert!(!status.expect("status").success());
    let log = std::fs::read_to_string(state.path().join("daemon.log")).unwrap_or_default();
    assert!(log.contains("state.toml"), "{log}");
}

#[test]
fn hook_does_not_wait_on_an_open_stdin() {
    let dir = tempfile::tempdir().expect("dir");
    warm_up();
    let mut child = Command::new(BIN)
        .args(["hook", "Stop"])
        .env(
            "LAZYAGENTS_AGENT_ID",
            "00000000-0000-4000-8000-000000000000",
        )
        .env("LAZYAGENTS_SOCKET", dir.path().join("missing.sock"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("hook");
    let _stdin = child.stdin.take();
    let mut status = None;
    let exited = wait_until(WAIT, || {
        status = child.try_wait().expect("try_wait");
        status.is_some()
    });
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(exited, "hook blocked on stdin");
    assert!(status.expect("status").success());
}

#[test]
fn idle_exit_is_configured_from_the_environment() {
    let env = TestEnv::new();
    let out = env.run_with_env(&["ls"], &[("LAZYAGENTS_IDLE_EXIT_SECS", "1")]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        wait_until(WAIT, || !env.socket().exists()),
        "idle daemon kept running"
    );
}

#[test]
fn auto_started_daemon_creates_a_private_state_dir() {
    use std::os::unix::fs::PermissionsExt;

    let parent = tempfile::tempdir().expect("parent");
    let state = parent.path().join("state");
    let run = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("LAZYAGENTS_STATE_DIR", &state)
            .stdin(Stdio::null())
            .output()
            .expect("run lazyagents")
    };
    let listed = run(&["ls"]);
    let mode = std::fs::metadata(&state).map(|m| m.permissions().mode());
    let stopped = run(&["shutdown"]);
    assert!(listed.status.success(), "{listed:?}");
    assert!(stopped.status.success(), "{stopped:?}");
    assert_eq!(mode.expect("state dir") & 0o777, 0o700);
}

#[test]
fn sigterm_stops_the_daemon_and_its_agents() {
    let env = TestEnv::new();
    let leaves = start_tree_agents(&env, &env.project.path().join("pids"));
    let daemon = rustix::process::Pid::from_raw(env.daemon_pid()).expect("pid");
    rustix::process::kill_process(daemon, rustix::process::Signal::TERM).expect("SIGTERM");
    assert!(
        wait_until(WAIT, || !env.socket().exists()),
        "socket survived SIGTERM"
    );
    assert_eq!(leaves.alive(), Vec::<u32>::new(), "survived SIGTERM");
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
        let lock = env.state.path().join("daemon.lock");
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

#[test]
fn shutdown_stops_a_mismatched_daemon() {
    let env = TestEnv::new();
    let fake = ForeignDaemon::start(&env);
    let out = env.run(&["shutdown"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("daemon stopped"),
        "{out:?}"
    );
    assert!(exited(&fake.0), "shutdown did not stop it");
}

#[test]
fn shutdown_refuses_a_daemon_that_answers_without_the_lock() {
    let env = TestEnv::new();
    serve_foreign_protocol(env.socket());
    let out = env.run(&["shutdown"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("daemon stopped"),
        "{out:?}"
    );
    assert!(stderr.contains("9.9.9"), "{stderr}");
    assert!(stderr.contains("stop it by hand"), "{stderr}");
}

/// A `lazyagents daemon` child, SIGKILLed by its pid on drop if it is still running.
struct DaemonChild(std::process::Child);

impl Drop for DaemonChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn stop_daemon_stops_a_running_daemon() {
    let env = TestEnv::new();
    let mut daemon = DaemonChild(
        Command::new(BIN)
            .arg("daemon")
            .env("LAZYAGENTS_STATE_DIR", env.state.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start daemon"),
    );
    assert!(
        wait_until(WAIT, || env.socket().exists()),
        "daemon never listened"
    );
    let paths = protocol::StatePaths::new(env.state.path());
    client::stop_daemon(&paths, Duration::from_secs(5)).expect("stop");
    assert!(!env.socket().exists(), "socket survived stop_daemon");
    assert!(
        wait_until(WAIT, || matches!(daemon.0.try_wait(), Ok(Some(_)))),
        "daemon still running"
    );
}

#[test]
fn a_daemon_started_in_another_locale_and_time_zone_can_be_stopped() {
    let env = TestEnv::new();
    let mut daemon = DaemonChild(
        Command::new(BIN)
            .arg("daemon")
            .env("LAZYAGENTS_STATE_DIR", env.state.path())
            .env("TZ", "Asia/Tokyo")
            .env("LC_ALL", "ja_JP.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start daemon"),
    );
    assert!(
        wait_until(WAIT, || env.socket().exists()),
        "daemon never listened"
    );
    let paths = protocol::StatePaths::new(env.state.path());
    client::stop_daemon(&paths, Duration::from_secs(20)).expect("stop");
    assert!(
        wait_until(WAIT, || matches!(daemon.0.try_wait(), Ok(Some(_)))),
        "daemon still running"
    );
}

#[test]
fn daemon_that_dies_at_startup_fails_fast() {
    let parent = tempfile::tempdir().expect("parent");
    let state = parent.path().join("s".repeat(110));
    warm_up();
    let started = Instant::now();
    let out = Command::new(BIN)
        .arg("ls")
        .env("LAZYAGENTS_STATE_DIR", &state)
        .stdin(Stdio::null())
        .output()
        .expect("run lazyagents");
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        stderr.contains(&format!(
            "daemon exited during startup; see {}",
            state.join("daemon.log").display()
        )),
        "{stderr}"
    );
    assert!(took < Duration::from_secs(10), "took {took:?}");
}

#[test]
fn daemon_does_not_inherit_claude_code_variables() {
    let env = TestEnv::new();
    let dump = env.project.path().join("env");
    env.run_with_env(
        &[
            "new",
            "dump",
            "--",
            "sh",
            "-c",
            &format!(
                "env > '{}.tmp'; mv '{0}.tmp' '{0}'; sleep 30",
                dump.display()
            ),
        ],
        &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")],
    );
    let vars = wait_for_file(&dump);
    assert!(vars.contains("LAZYAGENTS_AGENT_ID="), "{vars}");
    let leaked: Vec<&str> = vars
        .lines()
        .filter(|line| line.starts_with("CLAUDECODE=") || line.starts_with("CLAUDE_CODE_"))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
}

#[test]
fn no_subcommand_without_a_terminal_fails_cleanly() {
    let env = TestEnv::new();
    let started = Instant::now();
    let out = env.run(&[]);
    assert!(started.elapsed() < WAIT, "{out:?}");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("needs an interactive terminal"),
        "{out:?}"
    );
    assert!(!env.socket().exists(), "a daemon started");
    assert!(
        !env.state.path().join("daemon.lock").exists(),
        "a daemon started"
    );
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
        "echo \"${{LAZYAGENTS_TEST_LEAK-unset}} ${{LAZYAGENTS_TEST_ORIG-unset}}\" > '{0}.tmp'; mv '{0}.tmp' '{0}'",
        seen.display()
    );
    let out = env.run_with_env(&["new", "seer", "--", "sh", "-c", &script], &shell);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(wait_for_file(&seen).trim(), "unset original");
}

#[test]
fn an_unreadable_direnv_diff_still_starts_agents() {
    let env = TestEnv::new();
    let home = tempfile::tempdir().expect("direnv home");
    let base = isolated_direnv_home(home.path());
    let mut shell: Vec<(&str, &str)> = base.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    shell.extend([("DIRENV_DIFF", "garbage"), ("DIRENV_DIR", "-/nowhere")]);
    let started = env.project.path().join("started");
    let script = format!(
        "echo started > '{0}.tmp'; mv '{0}.tmp' '{0}'",
        started.display()
    );
    let out = env.run_with_env(&["new", "starter", "--", "sh", "-c", &script], &shell);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(wait_for_file(&started).trim(), "started");
}

/// The TUI in a detached session of `scripts/tmux-harness.sh`, on a tmux server whose
/// socket lives in a temp dir of its own; the session is stopped on drop.
struct Tui {
    sockets: tempfile::TempDir,
}

impl Tui {
    const SESSION: &str = "tui";

    fn start(env: &TestEnv, config: &str) -> Self {
        let config_file = env.state.path().join("tui-config.toml");
        std::fs::write(&config_file, config).expect("tui config");
        let tui = Tui {
            sockets: tempfile::tempdir().expect("tmux socket dir"),
        };
        let command = format!(
            "cd '{}' && exec env LAZYAGENTS_STATE_DIR='{}' LAZYAGENTS_CONFIG='{}' SHELL=/bin/sh '{BIN}'",
            env.project.path().display(),
            env.state.path().display(),
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
    assert!(env.ok(&["ls"]).contains("no projects yet"));
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
command = '''printf 'answer? '; until [ -e '{typed}' ]; do sleep 0.05; done; sleep 0.5; read line; echo "got:$line" > '{answer}.tmp'; mv '{answer}.tmp' '{answer}'; exit 4'''
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
    assert_eq!(wait_for_file(&answer).trim(), "got:hello");
    tui.wait_for("ask exited with 4");
    tui.keys(&["n"]);
    tui.wait_for(" New agent ");
}
