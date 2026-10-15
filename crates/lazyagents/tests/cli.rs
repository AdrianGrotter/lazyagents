use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
        Command::new(BIN)
            .args(args)
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
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = self.run(&["shutdown"]);
    }
}

fn alive(marker: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", marker])
        .status()
        .is_ok_and(|s| s.success())
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
    let marker = format!("lazyagents-cli-marker-{}", std::process::id());
    env.ok(&[
        "new",
        "sleeper",
        "--",
        "sh",
        "-c",
        &format!("sleep 30; : {marker}"),
    ]);
    assert!(
        wait_until(Duration::from_secs(5), || alive(&marker)),
        "agent never started"
    );
    env.ok(&["shutdown"]);
    assert!(
        wait_until(Duration::from_secs(3), || !alive(&marker)),
        "agent survived shutdown"
    );
}

#[test]
fn claude_agent_hooks_drive_status_and_restart_resumes() {
    let env = TestEnv::new();
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

    let hook = |event: &str| {
        let mut child = Command::new(BIN)
            .args(["hook", event])
            .env("LAZYAGENTS_AGENT_ID", &agent_id)
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
            .write_all(format!("{{\"session_id\":\"{session}\"}}").as_bytes())
            .expect("payload");
        let out = child.wait_with_output().expect("hook output");
        assert!(
            out.status.success() && out.stdout.is_empty() && out.stderr.is_empty(),
            "{out:?}"
        );
    };
    hook("UserPromptSubmit");
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("working")));
    hook("Notification");
    assert!(wait_until(Duration::from_secs(5), || env
        .ok(&["ls"])
        .contains("needs input")));

    std::fs::remove_file(&args_file).expect("reset");
    env.ok(&["restart", "c"]);
    let resumed = wait_for_file(&args_file);
    assert_eq!(
        resumed.lines().take(2).collect::<Vec<_>>(),
        ["--resume", session.as_str()]
    );
}

#[test]
fn hook_is_silent_and_fast_without_daemon() {
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
        start.elapsed() < Duration::from_secs(1),
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
