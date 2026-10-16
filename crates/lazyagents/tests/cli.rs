use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_lazyagents");

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
        Command::new(BIN)
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
        std::fs::read_to_string(self.state.path().join("daemon.lock"))
            .expect("lock file")
            .trim()
            .parse()
            .expect("pid in lock file")
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
        let listed = wait_until(Duration::from_secs(10), || {
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
        wait_until(Duration::from_secs(10), || {
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
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("working")));
    hook("Notification", &payload);
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("needs input")));

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
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("working")));
    claude.hook(
        &env,
        "Notification",
        &format!(
            "{{\"session_id\":\"{session}\",\"notification_type\":\"idle_prompt\",\"message\":\"Claude is waiting for your input\"}}"
        ),
    );
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("idle")));
    assert!(!env.ok(&["ls"]).contains("needs input"));
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
fn hook_is_silent_and_fast_without_daemon() {
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
        start.elapsed() < Duration::from_secs(3),
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
    let exited = wait_until(Duration::from_secs(10), || {
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
    let exited = wait_until(Duration::from_secs(10), || {
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
        wait_until(Duration::from_secs(4), || !env.socket().exists()),
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
        wait_until(Duration::from_secs(5), || !env.socket().exists()),
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
        wait_until(Duration::from_secs(5), || env.socket().exists()),
        "daemon never listened"
    );
    let paths = protocol::StatePaths::new(env.state.path());
    client::stop_daemon(&paths, Duration::from_secs(5)).expect("stop");
    assert!(!env.socket().exists(), "socket survived stop_daemon");
    assert!(
        wait_until(Duration::from_secs(5), || matches!(
            daemon.0.try_wait(),
            Ok(Some(_))
        )),
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
    assert!(took < Duration::from_secs(4), "took {took:?}");
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
    warm_up();
    let started = Instant::now();
    let out = env.run(&[]);
    assert!(started.elapsed() < Duration::from_secs(5), "{out:?}");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("needs an interactive terminal"),
        "{out:?}"
    );
    assert!(!env.socket().exists());
}
