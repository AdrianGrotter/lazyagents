use std::path::Path;
use std::process::Command;

/// Runs git isolated from the user's global and system config, so signing or hooks set
/// there never reach the temp repos.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 git output")
}

/// A temp repo on `main` with one commit holding `README` (`hello\n`).
pub fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("repo dir");
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "lazyagents test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@lazyagents.invalid"],
    );
    std::fs::write(dir.path().join("README"), "hello\n").expect("write README");
    git(dir.path(), &["add", "README"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}
