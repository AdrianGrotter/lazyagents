use std::path::{Path, PathBuf};

use daemon::direnv::{allow, allowed_envrc_above, auto_allow, is_allowed};

const ENVRC: &str = "export LAZYAGENTS_DIRENV_TEST=1\n";

/// direnv's home, data, config and cache dirs, all inside one tempdir so the user's
/// allow list and config are never read or written.
struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    config: PathBuf,
    cache: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("direnv home");
        let root = dir.path().to_path_buf();
        let [data, config, cache] = ["data", "config", "cache"].map(|name| {
            let path = root.join(name);
            std::fs::create_dir(&path).expect("xdg dir");
            path
        });
        Self {
            _dir: dir,
            root,
            data,
            config,
            cache,
        }
    }

    fn env(&self) -> [(&str, &Path); 5] {
        [
            ("HOME", &self.root),
            ("XDG_DATA_HOME", &self.data),
            ("XDG_CONFIG_HOME", &self.config),
            ("XDG_CACHE_HOME", &self.cache),
            ("DIRENV_CONFIG", &self.config),
        ]
    }
}

fn direnv() -> PathBuf {
    daemon::find_in_path("direnv").expect("direnv in the nix dev shell")
}

fn dir_with_envrc(content: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join(".envrc"), content).expect("write .envrc");
    dir
}

#[test]
fn real_direnv_allows_an_identical_worktree_envrc() {
    let direnv = direnv();
    let home = Home::new();
    let env = home.env();
    let project = dir_with_envrc(ENVRC);
    let worktree = dir_with_envrc(ENVRC);
    allow(&direnv, project.path(), &env).expect("allow the project");
    assert!(!is_allowed(&direnv, worktree.path(), &env).expect("worktree status"));

    assert!(auto_allow(&direnv, project.path(), worktree.path(), &env).expect("auto allow"));
    assert!(is_allowed(&direnv, worktree.path(), &env).expect("worktree status"));
}

#[test]
fn real_direnv_leaves_a_different_worktree_envrc_blocked() {
    let direnv = direnv();
    let home = Home::new();
    let env = home.env();
    let project = dir_with_envrc(ENVRC);
    let worktree = dir_with_envrc("export LAZYAGENTS_DIRENV_TEST=2\n");
    allow(&direnv, project.path(), &env).expect("allow the project");

    assert!(!auto_allow(&direnv, project.path(), worktree.path(), &env).expect("auto allow"));
    assert!(!is_allowed(&direnv, worktree.path(), &env).expect("worktree status"));
}

#[test]
fn real_direnv_reports_an_envrc_above_the_project_only_once_allowed() {
    let direnv = direnv();
    let home = Home::new();
    let env = home.env();
    let outer = dir_with_envrc(ENVRC);
    let root = outer.path().join("project");
    std::fs::create_dir(&root).expect("project dir");
    let worktree = tempfile::tempdir().expect("worktree");
    let above = |cwd: &Path| allowed_envrc_above(&direnv, cwd, &root, &env).expect("status");
    assert_eq!(above(&root), None);
    assert!(!is_allowed(&direnv, outer.path(), &env).expect("outer status"));

    allow(&direnv, outer.path(), &env).expect("allow the outer dir");
    assert_eq!(above(&root), Some(outer.path().to_path_buf()));
    assert_eq!(above(worktree.path()), Some(outer.path().to_path_buf()));

    std::fs::write(root.join(".envrc"), ENVRC).expect("write .envrc");
    assert_eq!(above(&root), None);
}

#[test]
fn real_direnv_does_not_reach_past_a_blocked_nearer_envrc() {
    let direnv = direnv();
    let home = Home::new();
    let env = home.env();
    let outer = dir_with_envrc(ENVRC);
    allow(&direnv, outer.path(), &env).expect("allow the outer dir");
    let mid = outer.path().join("mid");
    let root = mid.join("project");
    std::fs::create_dir_all(&root).expect("project dir");
    std::fs::write(mid.join(".envrc"), ENVRC).expect("write .envrc");

    assert_eq!(
        allowed_envrc_above(&direnv, &root, &root, &env).expect("status"),
        None
    );
    assert!(!is_allowed(&direnv, &mid, &env).expect("mid status"));
}
