//! Git worktrees and change queries, through the git CLI so the user's git config applies.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Context;
use protocol::{DiffScope, GitStat};

fn command(dir: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    command.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    command
}

fn git<I, S>(dir: Option<&Path>, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    finish(command(dir).args(args))
}

/// For queries that may run while an agent uses git in the same tree: they skip the
/// optional index refresh, so they never hold `index.lock` against the agent's commits.
fn git_read<I, S>(dir: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    finish(command(Some(dir)).args(args).env("GIT_OPTIONAL_LOCKS", "0"))
}

fn finish(command: &mut Command) -> anyhow::Result<String> {
    let output = command.output().context("running git")?;
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

pub fn head_commit(repo: &Path) -> anyhow::Result<String> {
    Ok(git(Some(repo), ["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_string())
}

fn branch_exists(repo: &Path, branch: &str) -> anyhow::Result<bool> {
    let output = command(Some(repo))
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .context("running git")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim()),
    }
}

fn check_base(base: &str) -> anyhow::Result<()> {
    if base.starts_with('-') {
        anyhow::bail!("invalid base {base:?}");
    }
    Ok(())
}

pub fn check_branch(name: &str) -> anyhow::Result<()> {
    git(None, ["check-ref-format", "--branch", name]).map(drop)
}

/// Adds the worktree, or leaves nothing behind: an add that fails after git created
/// the branch or the checkout (a failing hook, say) has both removed again. A branch
/// that existed before is never touched, and neither is a path that already exists.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    check_base(base)?;
    if path.symlink_metadata().is_ok() {
        anyhow::bail!("'{}' already exists", path.display());
    }
    let branch_existed = branch_exists(repo, branch)?;
    let added = git(
        Some(repo),
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            path.as_os_str(),
            OsStr::new(base),
        ],
    );
    if let Err(e) = added {
        undo_add(repo, path, branch, branch_existed);
        return Err(e);
    }
    Ok(())
}

fn undo_add(repo: &Path, path: &Path, branch: &str, branch_existed: bool) {
    let registered = git(Some(repo), ["worktree", "list", "--porcelain", "-z"])
        .map(|list| {
            list.split('\0')
                .any(|entry| entry.strip_prefix("worktree ").map(Path::new) == Some(path))
        })
        .unwrap_or(false);
    let removed = registered && remove_worktree(repo, path, true).is_ok();
    if !removed && path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
    if registered && !removed {
        let _ = git(Some(repo), ["worktree", "prune"]);
    }
    if !branch_existed {
        let _ = git(Some(repo), ["branch", "-D", branch]);
    }
}

pub fn dirty_files(worktree: &Path) -> anyhow::Result<Vec<String>> {
    let status = git_read(
        worktree,
        ["status", "--porcelain", "-z", "--untracked-files=normal"],
    )?;
    let mut entries = status.split('\0').filter(|entry| !entry.is_empty());
    let mut paths = Vec::new();
    while let Some(entry) = entries.next() {
        let (code, path) = (entry.get(..2).unwrap_or(""), entry.get(3..).unwrap_or(""));
        paths.push(path.to_string());
        if code.contains(['R', 'C']) {
            entries.next();
        }
    }
    Ok(paths)
}

/// Without `force`, git refuses a worktree with changes or untracked files, whatever
/// the user's `status.showUntrackedFiles`.
pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> anyhow::Result<()> {
    let mut args = vec![
        OsStr::new("-c"),
        OsStr::new("status.showUntrackedFiles=normal"),
        OsStr::new("worktree"),
        OsStr::new("remove"),
    ];
    if force {
        args.push(OsStr::new("--force"));
    }
    args.push(path.as_os_str());
    git(Some(repo), args).map(drop)
}

/// Merges `branch` into `base` in the project's own checkout. The agent's worktree must hold
/// no uncommitted work, which the merge would leave behind, and the checkout must be on
/// `base` with no tracked changes. A merge that fails is aborted, so the checkout is left as
/// it was.
pub fn merge(project: &Path, worktree: &Path, branch: &str, base: &str) -> anyhow::Result<String> {
    if !dirty_files(worktree)?.is_empty() {
        anyhow::bail!("the agent's worktree has uncommitted changes; commit them first");
    }
    let current = current_branch(project)?;
    if current != base {
        anyhow::bail!("the project checkout is on {current}, not {base}");
    }
    let tracked = git_read(project, ["status", "--porcelain", "--untracked-files=no"])?;
    if !tracked.trim().is_empty() {
        anyhow::bail!("the project checkout has changes; commit or stash them first");
    }
    if let Err(error) = git(Some(project), ["merge", "--no-edit", branch]) {
        let conflicts = git_read(project, ["diff", "--name-only", "--diff-filter=U"])
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let _ = git(Some(project), ["merge", "--abort"]);
        if conflicts.is_empty() {
            anyhow::bail!("merging {branch} into {base} failed and was abandoned: {error:#}");
        }
        anyhow::bail!(
            "merging {branch} into {base} hit a conflict in {} and was abandoned",
            conflicts.join(", ")
        );
    }
    Ok(format!("merged {branch} into {base}"))
}

/// Whether `base` already contains every commit of `branch`.
pub fn is_merged(repo: &Path, branch: &str, base: &str) -> anyhow::Result<bool> {
    let merged = command(Some(repo))
        .args(["merge-base", "--is-ancestor", branch, base])
        .status()
        .context("running git")?;
    match merged.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!("checking whether {branch} is merged into {base} failed"),
    }
}

/// Deletes `branch` only when `base` already contains all of it.
pub fn delete_merged_branch(repo: &Path, branch: &str, base: &str) -> anyhow::Result<()> {
    if !is_merged(repo, branch, base)? {
        anyhow::bail!("{branch} is not merged into {base}");
    }
    git(Some(repo), ["branch", "-D", branch]).map(drop)
}

/// The commit `stat` and `diff` compare the working tree with: the merge base with
/// `base`, so committed work counts too, or `HEAD` for uncommitted changes only.
fn compare_point(dir: &Path, base: Option<&str>) -> anyhow::Result<String> {
    let point = match base {
        Some(base) => {
            check_base(base)?;
            git_read(dir, ["merge-base", base, "HEAD"])?
        }
        None => git_read(dir, ["rev-parse", "--verify", "HEAD"])?,
    };
    Ok(point.trim().to_string())
}

pub fn stat(dir: &Path, base: Option<&str>) -> anyhow::Result<GitStat> {
    let point = compare_point(dir, base)?;
    let numstat = git_read(dir, ["diff", "--numstat", point.as_str()])?;
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

pub fn diff(dir: &Path, base: Option<&str>, scope: DiffScope) -> anyhow::Result<String> {
    let point = compare_point(dir, base)?;
    let mut args = vec!["diff", "--no-color", "--no-ext-diff"];
    match scope {
        DiffScope::All => args.push(&point),
        DiffScope::Uncommitted => args.push("HEAD"),
        DiffScope::Committed => args.extend([point.as_str(), "HEAD"]),
    }
    git_read(dir, args)
}
