//! The CLI commands against a daemon running in the test process: parsed as the binary
//! parses them, with what they print captured.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clap::Parser;
use client::Client;
use protocol::{HookKind, Request, StatePaths};
use tokio::task::JoinHandle;

use crate::{Cli, Command, commands, hook};

const WAIT: Duration = Duration::from_secs(30);

/// The `exe` the daemon writes into Claude agents' hook settings.
const EXE: &str = "/usr/bin/true";

struct Daemon {
    state: tempfile::TempDir,
    project: tempfile::TempDir,
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

impl Daemon {
    async fn start() -> Self {
        let state = tempfile::tempdir().expect("state");
        let project = tempfile::tempdir().expect("project");
        let task = tokio::spawn(daemon::run(daemon::Config {
            paths: StatePaths::new(state.path()),
            exe: PathBuf::from(EXE),
            idle_exit: None,
            direnv: None,
            claude_config: None,
            timings: daemon::Timings::default(),
        }));
        let paths = StatePaths::new(state.path());
        let started = tokio::time::Instant::now();
        while Client::connect(&paths).await.is_err() {
            assert!(!task.is_finished(), "daemon stopped before listening");
            assert!(started.elapsed() < WAIT, "daemon never listened");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            state,
            project,
            task: Some(task),
        }
    }

    fn paths(&self) -> StatePaths {
        StatePaths::new(self.state.path())
    }

    fn project_name(&self) -> String {
        file_name(self.project.path())
    }

    async fn run(&self, args: &[&str]) -> (anyhow::Result<()>, String) {
        run(&self.paths(), self.project.path(), args).await
    }

    async fn ok(&self, args: &[&str]) -> String {
        ok(&self.paths(), self.project.path(), args).await
    }

    async fn fail(&self, args: &[&str]) -> String {
        let (result, stdout) = self.run(args).await;
        let Err(error) = result else {
            panic!("lazyagents {args:?} succeeded: {stdout}");
        };
        assert_eq!(stdout, "", "lazyagents {args:?} printed before failing");
        format!("{error:#}")
    }

    async fn ls_until(&self, condition: impl Fn(&str) -> bool) -> String {
        let started = tokio::time::Instant::now();
        loop {
            let ls = self.ok(&["ls"]).await;
            if condition(&ls) {
                return ls;
            }
            assert!(started.elapsed() < WAIT, "ls never matched: {ls}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn socket(&self) -> PathBuf {
        self.paths().socket()
    }
}

/// Shuts the daemon down, stopping its agents, as `lazyagents shutdown` would.
impl Drop for Daemon {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        let paths = self.paths();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                if let Ok(mut client) = Client::connect(&paths).await {
                    let _ = client.request(Request::Shutdown).await;
                }
                let _ = tokio::time::timeout(WAIT, task).await;
            });
        });
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned()
}

/// Runs `lazyagents <args>` from `cwd` against the daemon of `paths`, returning its
/// result and what it printed.
async fn run(paths: &StatePaths, cwd: &Path, args: &[&str]) -> (anyhow::Result<()>, String) {
    let cli = Cli::try_parse_from(std::iter::once("lazyagents").chain(args.iter().copied()))
        .unwrap_or_else(|e| panic!("lazyagents {args:?} does not parse: {e}"));
    let command = cli.command.expect("a subcommand");
    let cwd = cwd.canonicalize().expect("canonical cwd");
    let mut stdout = Vec::new();
    let result = match command {
        Command::Shutdown => commands::shutdown(paths, &mut stdout).await,
        command => match Client::connect(paths).await {
            Ok(mut client) => {
                commands::execute(&mut client, command, &|| Ok(cwd.clone()), &mut stdout).await
            }
            Err(e) => Err(e),
        },
    };
    (result, String::from_utf8(stdout).expect("utf8 stdout"))
}

async fn ok(paths: &StatePaths, cwd: &Path, args: &[&str]) -> String {
    let (result, stdout) = run(paths, cwd, args).await;
    if let Err(e) = result {
        panic!("lazyagents {args:?} failed: {e:#}");
    }
    stdout
}

/// What a test agent wrote to `path`, once its last line is complete.
async fn written(path: &Path) -> String {
    let started = tokio::time::Instant::now();
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.ends_with('\n') {
            return text;
        }
        assert!(
            started.elapsed() < WAIT,
            "{} was never written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn help_of(args: &[&str]) -> String {
    let Err(e) = Cli::try_parse_from(std::iter::once("lazyagents").chain(args.iter().copied()))
    else {
        panic!("{args:?} parsed");
    };
    assert_eq!(e.kind(), clap::error::ErrorKind::DisplayHelp, "{e}");
    e.to_string()
}

#[test]
fn prints_version() {
    let Err(e) = Cli::try_parse_from(["lazyagents", "--version"]) else {
        panic!("--version parsed");
    };
    assert_eq!(e.kind(), clap::error::ErrorKind::DisplayVersion);
    assert_eq!(
        e.to_string().trim(),
        format!("lazyagents {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_says_how_agents_are_named_and_what_bare_lazyagents_does() {
    let top = help_of(&["--help"]);
    assert!(top.contains("no command opens the TUI"), "{top}");
    for command in ["kill", "restart", "rm", "rename", "send"] {
        let help = help_of(&[command, "--help"]);
        assert!(help.contains("<project>/<name>"), "{command}: {help}");
        assert!(help.contains("4 hex"), "{command}: {help}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_through_the_cli() {
    let daemon = Daemon::start().await;
    assert!(daemon.ok(&["ls"]).await.contains("no projects yet"));
    assert!(
        daemon
            .ok(&["new", "demo", "--", "cat"])
            .await
            .contains("started demo")
    );
    let ls = daemon.ok(&["ls"]).await;
    let project_name = daemon.project_name();
    assert!(ls.contains(&project_name), "{ls}");
    assert!(ls.contains("demo"), "{ls}");
    assert!(daemon.ok(&["kill", "demo"]).await.contains("stopped demo"));
    assert!(daemon.ok(&["ls"]).await.contains("stopped"));
    assert!(
        daemon
            .ok(&["restart", "demo"])
            .await
            .contains("restarted demo")
    );
    assert!(
        daemon
            .ok(&["rename", "demo", "demo2"])
            .await
            .contains("renamed demo to demo2")
    );
    assert!(
        daemon
            .ok(&["rm", &format!("{project_name}/demo2")])
            .await
            .contains("removed demo2")
    );
    assert!(!daemon.ok(&["ls"]).await.contains("demo2"));
    assert!(daemon.ok(&["shutdown"]).await.contains("daemon stopped"));
    assert!(!daemon.socket().exists());
    assert!(
        daemon
            .ok(&["shutdown"])
            .await
            .contains("daemon is not running")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_agent_is_an_error_that_prints_nothing() {
    let daemon = Daemon::start().await;
    let error = daemon.fail(&["kill", "nope"]).await;
    assert!(error.contains("no agent matches nope"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn new_types_the_prompt_into_a_generic_agent() {
    let daemon = Daemon::start().await;
    let typed = daemon.project.path().join("typed");
    let script = format!(
        "printf 'ready> '; read line; echo \"$line\" > '{}'",
        typed.display()
    );
    daemon
        .ok(&[
            "new",
            "asker",
            "--prompt",
            "hello there",
            "--",
            "sh",
            "-c",
            &script,
        ])
        .await;
    assert_eq!(written(&typed).await.trim(), "hello there");
}

#[tokio::test(flavor = "multi_thread")]
async fn send_types_a_line_into_an_agent() {
    let daemon = Daemon::start().await;
    let typed = daemon.project.path().join("typed");
    let script = format!("read line; echo \"$line\" > '{}'", typed.display());
    daemon
        .ok(&["new", "listener", "--", "sh", "-c", &script])
        .await;
    assert!(
        daemon
            .ok(&["send", "listener", "use pnpm"])
            .await
            .contains("sent to listener")
    );
    assert_eq!(written(&typed).await.trim(), "use pnpm");
}

#[tokio::test(flavor = "multi_thread")]
async fn project_add_relative_path_uses_the_callers_cwd() {
    let daemon = Daemon::start().await;
    let other = tempfile::tempdir().expect("other");
    std::fs::create_dir(other.path().join("sub")).expect("sub");
    let stdout = ok(&daemon.paths(), other.path(), &["project", "add", "sub"]).await;
    let expected = other.path().canonicalize().expect("canonical").join("sub");
    assert!(stdout.contains(&expected.display().to_string()), "{stdout}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_that_cannot_start_leaves_a_kept_agent_that_ls_explains() {
    let daemon = Daemon::start().await;
    let error = daemon
        .fail(&["new", "ghost", "--", "/nonexistent/lazyagents-cmd"])
        .await;
    assert!(error.contains("failed to start"), "{error}");
    assert!(error.contains("agent ghost was kept"), "{error}");
    assert!(error.contains("lazyagents restart"), "{error}");
    assert!(error.contains("lazyagents rm"), "{error}");
    let ls = daemon.ok(&["ls"]).await;
    assert!(
        ls.contains("last error: failed to start `/nonexistent/lazyagents-cmd`"),
        "{ls}"
    );
}

struct FakeClaude {
    args_file: PathBuf,
    agent: uuid::Uuid,
    session: String,
}

impl FakeClaude {
    /// A Claude agent whose command writes its arguments to `args`, ending with `EOF`, and
    /// then waits.
    async fn start(daemon: &Daemon) -> Self {
        let args_file = daemon.project.path().join("args");
        let fake = daemon.project.path().join("claude");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" EOF > '{}'\nread _\n",
                args_file.display()
            ),
        )
        .expect("fake claude");
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        daemon
            .ok(&[
                "new",
                "c",
                "--kind",
                "claude",
                "--",
                fake.to_str().expect("utf8"),
            ])
            .await;
        let args = Self::args(&args_file).await;
        assert_eq!(args[0], "--session-id");
        let session = args[1].clone();
        let settings = PathBuf::from(&args[3]);
        let agent = settings
            .file_stem()
            .expect("stem")
            .to_string_lossy()
            .parse()
            .expect("agent id");
        let settings_json = std::fs::read_to_string(&settings).expect("settings");
        assert!(
            settings_json.contains(&format!("'{EXE}' hook UserPromptSubmit")),
            "{settings_json}"
        );
        Self {
            args_file,
            agent,
            session,
        }
    }

    async fn args(path: &Path) -> Vec<String> {
        let started = tokio::time::Instant::now();
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if text.ends_with("EOF\n") {
                return text.lines().map(str::to_string).collect();
            }
            assert!(
                started.elapsed() < WAIT,
                "{} never complete",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn restarted_args(&self, daemon: &Daemon) -> Vec<String> {
        std::fs::remove_file(&self.args_file).expect("reset");
        daemon.ok(&["restart", "c"]).await;
        Self::args(&self.args_file).await
    }

    /// Forwards a hook as `lazyagents hook <event>` does with `payload` on stdin.
    async fn hook(&self, daemon: &Daemon, event: &str, payload: &str) {
        let kind = HookKind::from_name(event).expect("hook event");
        hook::relay(kind, self.agent, daemon.socket(), payload.as_bytes())
            .await
            .expect("hook delivered");
    }

    fn payload(&self) -> String {
        format!("{{\"session_id\":\"{}\"}}", self.session)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_agent_hooks_drive_status_and_restart_resumes() {
    let daemon = Daemon::start().await;
    let claude = FakeClaude::start(&daemon).await;
    let payload = claude.payload();
    claude.hook(&daemon, "UserPromptSubmit", &payload).await;
    daemon.ls_until(|ls| ls.contains("working")).await;
    claude.hook(&daemon, "Notification", &payload).await;
    daemon.ls_until(|ls| ls.contains("needs input")).await;

    let resumed = claude.restarted_args(&daemon).await;
    assert_eq!(resumed[..2], ["--resume", claude.session.as_str()]);

    let other = "6f1c2b9e-1d2a-4c3b-9e8f-0a1b2c3d4e5f";
    claude
        .hook(
            &daemon,
            "SessionStart",
            &format!("{{\"session_id\":\"{other}\",\"source\":\"resume\"}}"),
        )
        .await;
    let resumed = claude.restarted_args(&daemon).await;
    assert_eq!(resumed[..2], ["--resume", other]);
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_notification_marks_a_claude_agent_idle() {
    let daemon = Daemon::start().await;
    let claude = FakeClaude::start(&daemon).await;
    let session = &claude.session;
    claude
        .hook(&daemon, "UserPromptSubmit", &claude.payload())
        .await;
    daemon.ls_until(|ls| ls.contains("working")).await;
    claude
        .hook(
            &daemon,
            "Notification",
            &format!(
                "{{\"session_id\":\"{session}\",\"notification_type\":\"idle_prompt\",\"message\":\"Claude is waiting for your input\"}}"
            ),
        )
        .await;
    let ls = daemon.ls_until(|ls| ls.contains("idle")).await;
    assert!(!ls.contains("needs input"), "{ls}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claude_agent_stays_working_while_a_background_subagent_runs() {
    let daemon = Daemon::start().await;
    let claude = FakeClaude::start(&daemon).await;
    let session = &claude.session;
    let main = claude.payload();
    let sub = format!(
        "{{\"session_id\":\"{session}\",\"agent_id\":\"a853c9dd5f0c291b7\",\"agent_type\":\"general-purpose\"}}"
    );
    claude.hook(&daemon, "UserPromptSubmit", &main).await;
    claude.hook(&daemon, "SubagentStart", &sub).await;
    claude.hook(&daemon, "Stop", &main).await;
    let ls = daemon.ok(&["ls"]).await;
    assert!(ls.contains("working"), "{ls}");
    claude.hook(&daemon, "SubagentStop", &sub).await;
    claude.hook(&daemon, "Stop", &main).await;
    daemon.ls_until(|ls| ls.contains("idle")).await;
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
    fn start(paths: &StatePaths) -> Self {
        serve_foreign_protocol(paths.socket());
        let sleeper = std::process::Command::new("sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sleep");
        let lock = paths.lock_file();
        std::fs::write(&lock, sleeper.id().to_string()).expect("lock file");
        let held = std::fs::File::open(&lock).expect("open lock file");
        held.lock().expect("hold the lock");
        let fake = ForeignDaemon(Arc::new(Mutex::new(sleeper)));
        let watched = Arc::clone(&fake.0);
        let socket = paths.socket();
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

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_a_mismatched_daemon() {
    let state = tempfile::tempdir().expect("state");
    let paths = StatePaths::new(state.path());
    let fake = ForeignDaemon::start(&paths);
    let stdout = ok(&paths, state.path(), &["shutdown"]).await;
    assert!(stdout.contains("daemon stopped"), "{stdout}");
    assert!(exited(&fake.0), "shutdown did not stop it");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_refuses_a_daemon_that_answers_without_the_lock() {
    let state = tempfile::tempdir().expect("state");
    let paths = StatePaths::new(state.path());
    serve_foreign_protocol(paths.socket());
    let (result, stdout) = run(&paths, state.path(), &["shutdown"]).await;
    let error = format!("{:#}", result.expect_err("refused"));
    assert!(!stdout.contains("daemon stopped"), "{stdout}");
    assert!(error.contains("9.9.9"), "{error}");
    assert!(error.contains("stop it by hand"), "{error}");
}
