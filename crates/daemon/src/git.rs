//! Git worktrees and change queries, through the git CLI so the user's git config applies.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Context;
use protocol::GitStat;

fn git<I, S>(dir: Option<&Path>, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let output = command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("running git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            anyhow::bail!("git failed: {}", output.status);
        }
        anyhow::bail!("{stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn current_branch(repo: &Path) -> anyhow::Result<String> {
    Ok(git(Some(repo), ["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string())
}

pub fn check_branch(name: &str) -> anyhow::Result<()> {
    git(None, ["check-ref-format", "--branch", name]).map(drop)
}

pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    if base.starts_with('-') {
        anyhow::bail!("invalid base {base:?}");
    }
    git(
        Some(repo),
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            path.as_os_str(),
            OsStr::new(base),
        ],
    )
    .map(drop)
}

pub fn dirty_files(worktree: &Path) -> anyhow::Result<Vec<String>> {
    let status = git(Some(worktree), ["status", "--porcelain"])?;
    Ok(status
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::to_string)
        .collect())
}

pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> anyhow::Result<()> {
    let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force {
        args.push(OsStr::new("--force"));
    }
    args.push(path.as_os_str());
    git(Some(repo), args).map(drop)
}

/// The commit `stat` and `diff` compare the working tree with: the merge base with
/// `base`, so committed work counts too, or `HEAD` for uncommitted changes only.
fn compare_point(dir: &Path, base: Option<&str>) -> anyhow::Result<String> {
    let point = match base {
        Some(base) => git(Some(dir), ["merge-base", base, "HEAD"])?,
        None => git(Some(dir), ["rev-parse", "--verify", "HEAD"])?,
    };
    Ok(point.trim().to_string())
}

pub fn stat(dir: &Path, base: Option<&str>) -> anyhow::Result<GitStat> {
    let point = compare_point(dir, base)?;
    let numstat = git(Some(dir), ["diff", "--numstat", point.as_str()])?;
    let mut stat = GitStat::default();
    for line in numstat.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split('\t');
        let mut count = || {
            fields
                .next()
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        };
        stat.insertions += count();
        stat.deletions += count();
        stat.files += 1;
    }
    stat.uncommitted = u32::try_from(dirty_files(dir)?.len()).unwrap_or(u32::MAX);
    Ok(stat)
}

pub fn diff(dir: &Path, base: Option<&str>) -> anyhow::Result<String> {
    let point = compare_point(dir, base)?;
    git(
        Some(dir),
        ["diff", "--no-color", "--no-ext-diff", point.as_str()],
    )
}
