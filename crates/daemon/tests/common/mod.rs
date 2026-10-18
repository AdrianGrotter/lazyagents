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

/// A read-only `stuck` directory holding a file, so neither git nor `remove_dir_all` can
/// delete the directory it is in; it is made writable again when dropped.
pub struct Stuck(pub std::path::PathBuf);

impl Stuck {
    pub fn new(path: std::path::PathBuf) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&path).expect("stuck dir");
        std::fs::write(path.join("file"), "x").expect("stuck file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        Stuck(path)
    }
}

impl Drop for Stuck {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}
