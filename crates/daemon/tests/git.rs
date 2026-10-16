mod common;

use std::path::Path;

use common::{git, repo};
use daemon::git::{
    add_worktree, check_branch, current_branch, diff, dirty_files, remove_worktree, stat,
};
use protocol::GitStat;

#[test]
fn current_branch_reports_main() {
    let repo = repo();
    assert_eq!(current_branch(repo.path()).expect("branch"), "main");
}

#[test]
fn check_branch_rejects_lock_and_double_dot() {
    for bad in ["foo.lock", "a..b"] {
        let err = check_branch(bad).expect_err(bad);
        assert!(format!("{err:#}").contains(bad), "{err:#}");
    }
    check_branch("lazyagents/ok").expect("valid branch");
}

#[test]
fn worktree_lifecycle() {
    let repo = repo();
    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    assert_eq!(
        std::fs::read_to_string(wt.join("README")).expect("checked out"),
        "hello\n"
    );
    assert!(dirty_files(&wt).expect("clean").is_empty());

    std::fs::write(wt.join("notes.txt"), "a\nb\n").expect("write notes");
    git(&wt, &["add", "notes.txt"]);
    git(&wt, &["commit", "-q", "-m", "notes"]);
    std::fs::write(wt.join("README"), "hello\nworld\n").expect("edit README");
    assert_eq!(dirty_files(&wt).expect("dirty"), vec!["README".to_string()]);

    assert_eq!(
        stat(&wt, Some("main")).expect("stat"),
        GitStat {
            insertions: 3,
            deletions: 0,
            files: 2,
            uncommitted: 1,
        }
    );
    let text = diff(&wt, Some("main")).expect("diff");
    assert!(text.contains("+world"), "{text}");
    assert!(text.contains("+a\n+b\n"), "{text}");

    let err = remove_worktree(repo.path(), &wt, false).expect_err("dirty worktree kept");
    assert!(!format!("{err:#}").is_empty());
    assert!(wt.exists());
    remove_worktree(repo.path(), &wt, true).expect("forced removal");
    assert!(!wt.exists());
    assert!(
        git(repo.path(), &["branch", "--list", "lazyagents/wt"]).contains("lazyagents/wt"),
        "branch deleted"
    );
}

#[test]
fn stat_without_base_counts_uncommitted_only() {
    let repo = repo();
    std::fs::write(repo.path().join("second.txt"), "x\n").expect("write");
    git(repo.path(), &["add", "second.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "second"]);
    std::fs::write(repo.path().join("README"), "bye\n").expect("edit README");
    std::fs::write(repo.path().join("untracked.txt"), "u\n").expect("untracked");

    assert_eq!(
        stat(repo.path(), None).expect("stat"),
        GitStat {
            insertions: 1,
            deletions: 1,
            files: 1,
            uncommitted: 2,
        }
    );
    let text = diff(repo.path(), None).expect("diff");
    assert!(text.contains("-hello") && text.contains("+bye"), "{text}");
    assert!(!text.contains("second.txt"), "{text}");
}

#[test]
fn non_repo_errors_mention_git_repository() {
    let dir = tempfile::tempdir().expect("plain dir");
    let plain: &Path = dir.path();
    let errors = [
        stat(plain, None).map(|_| ()),
        stat(plain, Some("main")).map(|_| ()),
        diff(plain, None).map(|_| ()),
        diff(plain, Some("main")).map(|_| ()),
        dirty_files(plain).map(|_| ()),
        current_branch(plain).map(|_| ()),
    ];
    for result in errors {
        let err = result.expect_err("not a repo");
        assert!(
            format!("{err:#}").contains("not a git repository"),
            "{err:#}"
        );
    }
}

#[test]
fn dirty_files_returns_exact_paths() {
    let repo = repo();
    git(repo.path(), &["mv", "README", "README.md"]);
    std::fs::write(repo.path().join("my notes.txt"), "n\n").expect("spaced name");
    assert_eq!(
        dirty_files(repo.path()).expect("dirty"),
        vec!["README.md".to_string(), "my notes.txt".to_string()]
    );
}

#[test]
fn bases_never_reach_git_as_options() {
    let repo = repo();
    let root = tempfile::tempdir().expect("worktree root");
    let base = "--output=/dev/null";
    for err in [
        stat(repo.path(), Some(base)).map(|_| ()).expect_err("stat"),
        diff(repo.path(), Some(base)).map(|_| ()).expect_err("diff"),
        add_worktree(repo.path(), &root.path().join("wt"), "lazyagents/wt", base).expect_err("add"),
    ] {
        assert!(format!("{err:#}").contains("invalid base"), "{err:#}");
    }
}

#[test]
fn untracked_files_count_as_dirty_whatever_the_repo_config() {
    let repo = repo();
    git(repo.path(), &["config", "status.showUntrackedFiles", "no"]);
    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    std::fs::write(wt.join("untracked.txt"), "u\n").expect("untracked");

    assert_eq!(
        dirty_files(&wt).expect("dirty"),
        vec!["untracked.txt".to_string()]
    );
    remove_worktree(repo.path(), &wt, false).expect_err("untracked file kept");
    assert!(wt.join("untracked.txt").is_file());
}

#[test]
fn a_failed_add_leaves_other_stale_worktrees_registered() {
    let repo = repo();
    let root = tempfile::tempdir().expect("worktree root");
    let stale = root.path().join("stale");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "mine",
            stale.to_str().expect("utf8"),
        ],
    );
    std::fs::remove_dir_all(&stale).expect("remove the checkout");

    add_worktree(
        repo.path(),
        &root.path().join("wt"),
        "lazyagents/wt",
        "no-such-base",
    )
    .expect_err("bad base");
    let list = git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(list.contains("stale"), "{list}");
}
