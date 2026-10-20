//! Allows a new worktree's `.envrc` when the user already allowed the same content in
//! the project, finds an allowed `.envrc` above a project, and tells whether the daemon
//! inherited a direnv-loaded environment.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use serde_json::Value;

use crate::spawn::envrc_above;

/// Extra environment for the direnv commands.
pub type Env<'a> = &'a [(&'a str, &'a Path)];

pub fn should_allow(
    project_rc: Option<&[u8]>,
    worktree_rc: Option<&[u8]>,
    project_allowed: bool,
) -> bool {
    matches!((project_rc, worktree_rc), (Some(p), Some(w)) if p == w) && project_allowed
}

/// direnv reports `allowed: 0` for an allowed rc; any other value or rc means no.
fn parse_allowed(stdout: &[u8], dir: &Path) -> anyhow::Result<bool> {
    let status: Value = serde_json::from_slice(stdout).context("reading direnv status --json")?;
    let found = &status["state"]["foundRC"];
    let expected = canonical(&dir.join(".envrc"));
    let path_matches = found["path"]
        .as_str()
        .is_some_and(|p| canonical(Path::new(p)) == expected);
    Ok(path_matches && found["allowed"].as_i64() == Some(0))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn is_allowed(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<bool> {
    let output = Command::new(direnv)
        .args(["status", "--json"])
        .envs(env.iter().copied())
        .current_dir(dir)
        .output()
        .with_context(|| format!("running {} status", direnv.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "direnv status failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    parse_allowed(&output.stdout, dir)
}

/// The directory above `root` whose `.envrc` an agent in `cwd` loads: the nearest one, when
/// no `.envrc` in the project applies and direnv already allows it.
pub fn allowed_envrc_above(
    direnv: &Path,
    cwd: &Path,
    root: &Path,
    env: Env,
) -> anyhow::Result<Option<PathBuf>> {
    match envrc_above(cwd, root) {
        Some(dir) if is_allowed(direnv, &dir, env)? => Ok(Some(dir)),
        _ => Ok(None),
    }
}

pub fn allow(direnv: &Path, dir: &Path, env: Env) -> anyhow::Result<()> {
    let output = Command::new(direnv)
        .arg("allow")
        .arg(dir)
        .envs(env.iter().copied())
        .output()
        .with_context(|| format!("running {} allow", direnv.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "direnv allow failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

/// True when the daemon's environment holds changes a direnv-loaded `.envrc` made, which
/// `direnv export json` run outside any `.envrc` then prints the revert of. An error means
/// direnv could not read them, such as a corrupt `DIRENV_DIFF`.
pub fn inherited_env(direnv: &Path) -> anyhow::Result<bool> {
    let output = Command::new(direnv)
        .args(["export", "json"])
        .current_dir("/")
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running {} export", direnv.display()))?;
    anyhow::ensure!(
        output.status.success(),
        "direnv export failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(!output.stdout.trim_ascii().is_empty())
}

/// Runs `direnv allow` on `worktree` when its `.envrc` matches the project's allowed one;
/// true when it did.
pub fn auto_allow(
    direnv: &Path,
    project: &Path,
    worktree: &Path,
    env: Env,
) -> anyhow::Result<bool> {
    let project_rc = std::fs::read(project.join(".envrc")).ok();
    let worktree_rc = std::fs::read(worktree.join(".envrc")).ok();
    if project_rc.is_none() || project_rc != worktree_rc {
        return Ok(false);
    }
    let project_allowed = is_allowed(direnv, project, env)?;
    if !should_allow(
        project_rc.as_deref(),
        worktree_rc.as_deref(),
        project_allowed,
    ) {
        return Ok(false);
    }
    allow(direnv, worktree, env)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_allowed_envrc_is_allowed() {
        assert!(should_allow(
            Some(b"use flake\n"),
            Some(b"use flake\n"),
            true
        ));
    }

    #[test]
    fn different_envrc_is_not_allowed() {
        assert!(!should_allow(
            Some(b"use flake\n"),
            Some(b"use flake\nexport X=1\n"),
            true
        ));
    }

    #[test]
    fn unallowed_project_envrc_is_not_copied() {
        assert!(!should_allow(
            Some(b"use flake\n"),
            Some(b"use flake\n"),
            false
        ));
    }

    #[test]
    fn a_missing_envrc_on_either_side_allows_nothing() {
        assert!(!should_allow(None, Some(b"use flake\n"), true));
        assert!(!should_allow(Some(b"use flake\n"), None, true));
        assert!(!should_allow(None, None, true));
    }

    #[test]
    fn status_json_reports_allowed_only_for_that_envrc() {
        let json = br#"{"state":{"foundRC":{"allowed":0,"path":"/p/.envrc"},"loadedRC":null}}"#;
        assert!(parse_allowed(json, Path::new("/p")).unwrap());
        let other = br#"{"state":{"foundRC":{"allowed":0,"path":"/elsewhere/.envrc"}}}"#;
        assert!(!parse_allowed(other, Path::new("/p")).unwrap());
        let blocked = br#"{"state":{"foundRC":{"allowed":1,"path":"/p/.envrc"}}}"#;
        assert!(!parse_allowed(blocked, Path::new("/p")).unwrap());
        let none = br#"{"state":{"foundRC":null}}"#;
        assert!(!parse_allowed(none, Path::new("/p")).unwrap());
    }

    #[test]
    fn unparsable_status_is_an_error() {
        assert!(parse_allowed(b"direnv: unknown flag --json", Path::new("/p")).is_err());
    }

    #[test]
    fn status_path_matches_through_a_symlinked_dir() {
        let real = tempfile::tempdir().expect("tempdir");
        std::fs::write(real.path().join(".envrc"), "use flake\n").expect("write .envrc");
        let links = tempfile::tempdir().expect("tempdir");
        let link = links.path().join("link");
        std::os::unix::fs::symlink(real.path(), &link).expect("symlink");
        let reported = real
            .path()
            .canonicalize()
            .expect("canonical")
            .join(".envrc");
        let json = format!(
            r#"{{"state":{{"foundRC":{{"allowed":0,"path":{}}}}}}}"#,
            serde_json::to_string(reported.to_str().expect("utf8 path")).expect("json")
        );
        assert!(parse_allowed(json.as_bytes(), &link).unwrap());
    }
}
