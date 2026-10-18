//! Git worktrees and change queries, through the git CLI so the user's git config applies.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;
use protocol::{DiffScope, GitStat};
use uuid::Uuid;

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

pub fn branch_exists(repo: &Path, branch: &str) -> anyhow::Result<bool> {
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

/// Removes a worktree `add_worktree` made, with the branch it created; true once the
/// directory, git's record of the worktree and the branch are all gone. git keeps a locked
/// worktree, and a branch checked out in a worktree it keeps.
pub fn discard_worktree(repo: &Path, path: &Path, branch: &str) -> bool {
    undo_add(repo, path, branch, false);
    !path.exists()
        && registered(repo, path) == Some(false)
        && branch_exists(repo, branch).ok() == Some(false)
}

/// Whether git lists `path` as one of the repo's worktrees; `None` when it cannot tell.
fn registered(repo: &Path, path: &Path) -> Option<bool> {
    let list = git(Some(repo), ["worktree", "list", "--porcelain", "-z"]).ok()?;
    Some(
        list.split('\0')
            .any(|entry| entry.strip_prefix("worktree ").map(Path::new) == Some(path)),
    )
}

fn undo_add(repo: &Path, path: &Path, branch: &str, branch_existed: bool) {
    let registered = registered(repo, path).unwrap_or(false);
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

/// Copies into a new worktree the project's files that git ignores and its `.worktreeinclude`
/// names, as Claude Code does for its own worktrees; gitignore syntax, relative to the
/// project. A directory git ignores as a whole is searched only when a pattern may reach
/// into it (`may_reach_into`), so a large ignored tree such as `target/` is never walked
/// for a pattern like `*.env`. A file the first listing names on its own is ignored
/// already, so only the directories are listed again for what git ignores within them.
/// Returns the copied paths, sorted.
pub fn copy_worktree_includes(project: &Path, worktree: &Path) -> anyhow::Result<Vec<String>> {
    let include = project.join(".worktreeinclude");
    if !include.is_file() {
        return Ok(Vec::new());
    }
    let patterns = std::fs::read_to_string(&include)
        .with_context(|| format!("reading {}", include.display()))?;
    let entries = ls_ignored(project, &["--exclude-standard", "--directory"], &[])?;
    let scope: Vec<&str> = entries
        .iter()
        .map(String::as_str)
        .filter(|entry| {
            !entry.ends_with('/') || patterns.lines().any(|line| may_reach_into(line, entry))
        })
        .collect();
    if scope.is_empty() {
        return Ok(Vec::new());
    }
    let exclude_from = format!("--exclude-from={}", include.display());
    let named = ls_ignored(project, &[exclude_from.as_str()], &scope)?;
    let (dirs, files): (Vec<&str>, Vec<&str>) =
        scope.iter().partition(|entry| entry.ends_with('/'));
    let mut ignored: std::collections::BTreeSet<String> =
        files.into_iter().map(str::to_string).collect();
    if !dirs.is_empty() {
        ignored.extend(ls_ignored(project, &["--exclude-standard"], &dirs)?);
    }
    let mut copied = Vec::new();
    for path in named.intersection(&ignored) {
        let target = worktree.join(path);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(project.join(path), &target)
            .with_context(|| format!("copying {path} into the worktree"))?;
        copied.push(path.clone());
    }
    Ok(copied)
}

/// Untracked files that `excludes` ignore, within the `scope` paths when there are any;
/// they go to git in batches, so no command line grows too long.
fn ls_ignored(
    project: &Path,
    excludes: &[&str],
    scope: &[&str],
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let batches: Vec<&[&str]> = if scope.is_empty() {
        vec![&[]]
    } else {
        scope.chunks(500).collect()
    };
    let mut listed = std::collections::BTreeSet::new();
    for batch in batches {
        let mut args = vec!["ls-files", "-z", "--others", "--ignored"];
        args.extend(excludes);
        let literal: Vec<String> = batch
            .iter()
            .map(|path| format!(":(literal){path}"))
            .collect();
        if !literal.is_empty() {
            args.push("--");
            args.extend(literal.iter().map(String::as_str));
        }
        let out = git_read(project, &args)?;
        listed.extend(
            out.split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string),
        );
    }
    Ok(listed)
}

/// Whether the `.worktreeinclude` line may match a path inside `dir`, a directory git listed
/// whole, with its trailing `/`: whether the pattern, read up to its first wildcard, names
/// the directory, a path in it or a directory holding it. A pattern without a `/` before
/// its end, or one starting with `**/`, is tried at every depth of `dir`; one starting with
/// a wildcard reaches into no such directory.
fn may_reach_into(line: &str, dir: &str) -> bool {
    let pattern = line.trim_end();
    if pattern.is_empty() || pattern.starts_with(['#', '!']) {
        return false;
    }
    let mut body = pattern;
    let mut any_depth = false;
    while let Some(rest) = body.strip_prefix("**/") {
        body = rest;
        any_depth = true;
    }
    let anchored = body.starts_with('/');
    let body = body.trim_start_matches('/').trim_end_matches('/');
    if body.is_empty() {
        return false;
    }
    if !any_depth && (anchored || body.contains('/')) {
        return reaches(body, dir);
    }
    let mut start = dir;
    loop {
        if reaches(body, start) {
            return true;
        }
        match start.trim_end_matches('/').find('/') {
            Some(slash) => start = &start[slash + 1..],
            None => return false,
        }
    }
}

/// Whether the anchored pattern `body` may match `dir` or a path inside it.
fn reaches(body: &str, dir: &str) -> bool {
    match body.find(['*', '?', '[', '\\']) {
        None => format!("{body}/").starts_with(dir) || dir.starts_with(&format!("{body}/")),
        Some(0) => false,
        Some(wildcard) => {
            let literal = &body[..wildcard];
            literal.starts_with(dir) || dir.starts_with(literal)
        }
    }
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

/// A copy of a worktree's index that also lists its untracked, unignored files as
/// intent-to-add, so a `git diff` against the working tree shows them as new files. The
/// worktree's own index is never written; the copy is deleted on drop.
struct IndexWithUntracked(PathBuf);

impl IndexWithUntracked {
    fn new(dir: &Path) -> anyhow::Result<Self> {
        let index = git_read(
            dir,
            ["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?;
        let copy = Self(std::env::temp_dir().join(format!("lazyagents-index-{}", Uuid::new_v4())));
        match std::fs::copy(index.trim(), &copy.0) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error).context("copying the index");
            }
            _ => {}
        }
        finish(
            command(Some(dir))
                .args(["add", "--intent-to-add", "--all"])
                .env("GIT_INDEX_FILE", &copy.0),
        )?;
        Ok(copy)
    }

    fn diff<I, S>(&self, dir: &Path, args: I) -> anyhow::Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        finish(
            command(Some(dir))
                .arg("diff")
                .args(args)
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_INDEX_FILE", &self.0),
        )
    }
}

impl Drop for IndexWithUntracked {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn stat(dir: &Path, base: Option<&str>) -> anyhow::Result<GitStat> {
    let point = compare_point(dir, base)?;
    let numstat = IndexWithUntracked::new(dir)?.diff(dir, ["--numstat", point.as_str()])?;
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
    let plain = ["--no-color", "--no-ext-diff"];
    match scope {
        DiffScope::All => {
            IndexWithUntracked::new(dir)?.diff(dir, [plain[0], plain[1], point.as_str()])
        }
        DiffScope::Uncommitted => {
            IndexWithUntracked::new(dir)?.diff(dir, [plain[0], plain[1], "HEAD"])
        }
        DiffScope::Committed => git_read(dir, ["diff", plain[0], plain[1], &point, "HEAD"]),
    }
}

#[cfg(test)]
mod tests {
    use super::may_reach_into;

    #[test]
    fn a_pattern_reaches_into_the_directories_it_names() {
        for (pattern, dir) in [
            (".cache/token", ".cache/"),
            ("/.cache/token", ".cache/"),
            ("config/*.json", "config/"),
            (".env*", ".envs/"),
            ("node_modules", "node_modules/"),
            ("node_modules/", "pkg/a/node_modules/"),
            ("build", "build/debug/"),
            ("**/secrets/key", "app/secrets/"),
        ] {
            assert!(may_reach_into(pattern, dir), "{pattern} into {dir}");
        }
    }

    #[test]
    fn a_pattern_leaves_unrelated_directories_alone() {
        for (pattern, dir) in [
            (".env", "target/"),
            ("*.env", "target/"),
            ("**/*.env", "target/"),
            ("/node_modules", "pkg/node_modules/"),
            ("config/local.toml", "target/config/"),
            ("!target", "target/"),
            ("# target", "target/"),
            ("", "target/"),
        ] {
            assert!(!may_reach_into(pattern, dir), "{pattern} into {dir}");
        }
    }
}
