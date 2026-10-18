mod common;

use std::path::Path;

use common::{git, repo};
use daemon::git::{
    add_worktree, check_branch, copy_worktree_includes, current_branch, delete_merged_branch, diff,
    dirty_files, discard_worktree, merge, remove_worktree, stat,
};
use protocol::{DiffScope, GitStat};

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
    let text = diff(&wt, Some("main"), DiffScope::All).expect("diff");
    assert!(text.contains("+world"), "{text}");
    assert!(text.contains("+a\n+b\n"), "{text}");
    let uncommitted = diff(&wt, Some("main"), DiffScope::Uncommitted).expect("diff");
    assert!(uncommitted.contains("+world"), "{uncommitted}");
    assert!(!uncommitted.contains("+a\n"), "{uncommitted}");
    let committed = diff(&wt, Some("main"), DiffScope::Committed).expect("diff");
    assert!(committed.contains("+a\n+b\n"), "{committed}");
    assert!(!committed.contains("+world"), "{committed}");
    let plain = diff(&wt, None, DiffScope::Committed).expect("diff");
    assert_eq!(plain, "");

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
            insertions: 2,
            deletions: 1,
            files: 2,
            uncommitted: 2,
        }
    );
    let text = diff(repo.path(), None, DiffScope::All).expect("diff");
    assert!(text.contains("-hello") && text.contains("+bye"), "{text}");
    assert!(!text.contains("second.txt"), "{text}");
}

#[test]
fn untracked_files_count_as_changes_without_touching_the_index() {
    let repo = repo();
    std::fs::write(repo.path().join(".gitignore"), "*.log\n").expect("gitignore");
    git(repo.path(), &["add", ".gitignore"]);
    git(repo.path(), &["commit", "-q", "-m", "ignore logs"]);
    std::fs::create_dir(repo.path().join("sub")).expect("mkdir");
    std::fs::write(repo.path().join("sub/new.txt"), "a\nb\n").expect("untracked");
    std::fs::write(repo.path().join("noise.log"), "x\n").expect("ignored");

    assert_eq!(
        stat(repo.path(), Some("main")).expect("stat"),
        GitStat {
            insertions: 2,
            deletions: 0,
            files: 1,
            uncommitted: 1,
        }
    );
    for scope in [DiffScope::All, DiffScope::Uncommitted] {
        let text = diff(repo.path(), Some("main"), scope).expect("diff");
        assert!(
            text.contains("sub/new.txt") && text.contains("+a\n+b\n"),
            "{text}"
        );
        assert!(!text.contains("noise.log"), "{text}");
    }
    let committed = diff(repo.path(), Some("main"), DiffScope::Committed).expect("diff");
    assert_eq!(committed, "");
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]),
        "?? sub/\n",
        "the real index still lists the file as untracked"
    );
}

#[test]
fn non_repo_errors_mention_git_repository() {
    let dir = tempfile::tempdir().expect("plain dir");
    let plain: &Path = dir.path();
    let errors = [
        stat(plain, None).map(|_| ()),
        stat(plain, Some("main")).map(|_| ()),
        diff(plain, None, DiffScope::All).map(|_| ()),
        diff(plain, Some("main"), DiffScope::All).map(|_| ()),
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
        diff(repo.path(), Some(base), DiffScope::All)
            .map(|_| ())
            .expect_err("diff"),
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

fn worktree_with_commit(
    repo: &Path,
    file: &str,
    text: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo, &wt, "lazyagents/wt", "main").expect("add");
    std::fs::write(wt.join(file), text).expect("write");
    git(&wt, &["add", file]);
    git(&wt, &["commit", "-q", "-m", "work"]);
    (root, wt)
}

#[test]
fn merging_brings_the_agents_commits_into_its_base() {
    let repo = repo();
    let (_root, wt) = worktree_with_commit(repo.path(), "feature.txt", "done\n");
    std::fs::write(repo.path().join("scratch.txt"), "untracked\n").expect("untracked");
    let message = merge(repo.path(), &wt, "lazyagents/wt", "main").expect("merge");
    assert_eq!(message, "merged lazyagents/wt into main");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("feature.txt")).expect("merged"),
        "done\n"
    );
}

#[test]
fn merging_refuses_unsafe_states() {
    let repo = repo();
    let (_root, wt) = worktree_with_commit(repo.path(), "feature.txt", "done\n");
    std::fs::write(wt.join("feature.txt"), "more\n").expect("dirty");
    let err = merge(repo.path(), &wt, "lazyagents/wt", "main").expect_err("dirty worktree");
    assert!(
        format!("{err:#}").contains("uncommitted changes"),
        "{err:#}"
    );
    git(&wt, &["checkout", "--", "feature.txt"]);

    std::fs::write(repo.path().join("README"), "edited\n").expect("dirty project");
    let err = merge(repo.path(), &wt, "lazyagents/wt", "main").expect_err("dirty project");
    assert!(format!("{err:#}").contains("has changes"), "{err:#}");
    git(repo.path(), &["checkout", "--", "README"]);

    git(repo.path(), &["checkout", "-q", "-b", "other"]);
    let err = merge(repo.path(), &wt, "lazyagents/wt", "main").expect_err("wrong branch");
    assert!(
        format!("{err:#}").contains("is on other, not main"),
        "{err:#}"
    );
}

#[test]
fn a_conflicting_merge_is_abandoned_cleanly() {
    let repo = repo();
    let (_root, wt) = worktree_with_commit(repo.path(), "README", "from the agent\n");
    std::fs::write(repo.path().join("README"), "from main\n").expect("write");
    git(repo.path(), &["commit", "-q", "-am", "main moved"]);
    let err = merge(repo.path(), &wt, "lazyagents/wt", "main").expect_err("conflict");
    assert!(format!("{err:#}").contains("conflict"), "{err:#}");
    assert_eq!(git(repo.path(), &["status", "--porcelain"]), "");
    assert!(!repo.path().join(".git/MERGE_HEAD").exists());
}

#[test]
fn only_a_merged_branch_is_deleted() {
    let repo = repo();
    let (_root, wt) = worktree_with_commit(repo.path(), "feature.txt", "done\n");
    remove_worktree(repo.path(), &wt, false).expect("remove");
    let err = delete_merged_branch(repo.path(), "lazyagents/wt", "main").expect_err("unmerged");
    assert!(
        format!("{err:#}").contains("not merged into main"),
        "{err:#}"
    );
    git(repo.path(), &["merge", "-q", "--ff-only", "lazyagents/wt"]);
    delete_merged_branch(repo.path(), "lazyagents/wt", "main").expect("merged");
    assert_eq!(git(repo.path(), &["branch", "--list", "lazyagents/wt"]), "");
}

#[test]
fn worktree_include_copies_matching_ignored_files_only() {
    let repo = repo();
    std::fs::write(
        repo.path().join(".gitignore"),
        ".env\nconfig/local.toml\nbuild/\n",
    )
    .expect("ignore");
    std::fs::write(
        repo.path().join(".worktreeinclude"),
        ".env\nconfig/local.toml\n",
    )
    .expect("include");
    git(repo.path(), &["add", ".gitignore", ".worktreeinclude"]);
    git(repo.path(), &["commit", "-q", "-m", "ignore"]);
    std::fs::write(repo.path().join(".env"), "SECRET=1\n").expect("env");
    std::fs::create_dir_all(repo.path().join("config")).expect("config dir");
    std::fs::write(repo.path().join("config/local.toml"), "port = 1\n").expect("local");
    std::fs::create_dir_all(repo.path().join("build")).expect("build dir");
    std::fs::write(repo.path().join("build/out.o"), "binary").expect("build");
    std::fs::write(repo.path().join("notes.txt"), "untracked, not ignored\n").expect("notes");

    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    let copied = copy_worktree_includes(repo.path(), &wt).expect("copy");
    assert_eq!(copied, [".env", "config/local.toml"]);
    assert_eq!(
        std::fs::read_to_string(wt.join(".env")).expect("env"),
        "SECRET=1\n"
    );
    assert!(wt.join("config/local.toml").exists());
    assert!(!wt.join("build/out.o").exists());
    assert!(!wt.join("notes.txt").exists());
}

#[test]
fn worktree_include_searches_only_the_ignored_directories_it_names() {
    let repo = repo();
    std::fs::write(
        repo.path().join(".gitignore"),
        "target/\n.cache/\nnode_modules/\n.env\n",
    )
    .expect("ignore");
    std::fs::write(
        repo.path().join(".worktreeinclude"),
        "*.env\n.env\n.cache/token\nnode_modules\n",
    )
    .expect("include");
    git(repo.path(), &["add", ".gitignore", ".worktreeinclude"]);
    git(repo.path(), &["commit", "-q", "-m", "ignore"]);
    let files = [
        ".env",
        "pkg/a/.env",
        "target/debug/build.env",
        ".cache/token",
        ".cache/other",
        "node_modules/m/index.js",
    ];
    for file in files {
        let path = repo.path().join(file);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, file).expect("write");
    }

    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    let copied = copy_worktree_includes(repo.path(), &wt).expect("copy");
    assert_eq!(
        copied,
        [
            ".cache/token",
            ".env",
            "node_modules/m/index.js",
            "pkg/a/.env"
        ]
    );
    assert!(!wt.join("target").exists());
}

#[test]
fn without_a_worktree_include_nothing_is_copied() {
    let repo = repo();
    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    assert!(
        copy_worktree_includes(repo.path(), &wt)
            .expect("copy")
            .is_empty()
    );
}

/// The daemon's git calls inherit the test process's environment, which the nextest setup
/// script `scripts/nextest-isolated-git.sh` points away from the global and system config.
/// Here both hold settings that break a merge: `merge.ff = only` refuses a diverged branch
/// and `commit.gpgsign` with a failing `gpg.program` refuses the merge commit.
#[test]
fn hostile_global_and_system_git_config_do_not_reach_the_tests() {
    let repo = repo();
    let (_root, _wt) = worktree_with_commit(repo.path(), "feature.txt", "feature\n");
    std::fs::write(repo.path().join("main.txt"), "main\n").expect("write");
    git(repo.path(), &["add", "main.txt"]);
    git(repo.path(), &["commit", "-q", "-m", "main moves on"]);

    let home = tempfile::tempdir().expect("hostile home");
    std::fs::write(
        home.path().join(".gitconfig"),
        "[merge]\n\tff = only\n[init]\n\tdefaultBranch = hostile\n",
    )
    .expect("hostile global config");
    let system = home.path().join("system-gitconfig");
    std::fs::write(
        &system,
        "[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = false\n",
    )
    .expect("hostile system config");
    let hostile_git = |dir: &Path, args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path())
            .env("GIT_CONFIG_SYSTEM", &system)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run git")
    };
    let hint = "tests get their git isolation from nextest's setup script \
                (scripts/nextest-isolated-git.sh); run them with `just test`";

    let merged = hostile_git(repo.path(), &["merge", "--no-edit", "lazyagents/wt"]);
    assert!(
        merged.status.success(),
        "the merge read the hostile config: {}\n{hint}",
        String::from_utf8_lossy(&merged.stderr)
    );
    let parents = git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(parents.split_whitespace().count(), 3, "{parents}\n{hint}");

    let fresh = tempfile::tempdir().expect("fresh repo");
    let init = hostile_git(fresh.path(), &["init", "-q"]);
    assert!(init.status.success(), "{init:?}");
    let branch = hostile_git(fresh.path(), &["symbolic-ref", "--short", "HEAD"]);
    assert_ne!(
        String::from_utf8_lossy(&branch.stdout).trim(),
        "hostile",
        "git init read the hostile global config\n{hint}"
    );
}

#[test]
fn worktree_include_copies_many_ignored_files_beside_an_ignored_directory() {
    let repo = repo();
    std::fs::write(repo.path().join(".gitignore"), "*.local\ncache/\n").expect("ignore");
    std::fs::write(
        repo.path().join(".worktreeinclude"),
        "*.local\ncache/keep\n",
    )
    .expect("include");
    git(repo.path(), &["add", ".gitignore", ".worktreeinclude"]);
    git(repo.path(), &["commit", "-q", "-m", "ignore"]);
    let mut expected: Vec<String> = (0..600).map(|i| format!("f{i:03}.local")).collect();
    for file in &expected {
        std::fs::write(repo.path().join(file), file).expect("write");
    }
    std::fs::create_dir_all(repo.path().join("cache")).expect("cache dir");
    std::fs::write(repo.path().join("cache/keep"), "keep").expect("keep");
    std::fs::write(repo.path().join("cache/drop"), "drop").expect("drop");
    expected.insert(0, "cache/keep".into());

    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    let copied = copy_worktree_includes(repo.path(), &wt).expect("copy");
    assert_eq!(copied, expected);
    assert!(!wt.join("cache/drop").exists());
}

#[test]
fn discarding_a_worktree_git_keeps_reports_what_is_left() {
    let repo = repo();
    let root = tempfile::tempdir().expect("worktree root");
    let wt = root.path().canonicalize().expect("canonical").join("wt");
    add_worktree(repo.path(), &wt, "lazyagents/wt", "main").expect("add");
    let wt_arg = wt.to_str().expect("utf8");
    git(repo.path(), &["worktree", "lock", wt_arg]);

    assert!(!discard_worktree(repo.path(), &wt, "lazyagents/wt"));
    assert_ne!(git(repo.path(), &["branch", "--list", "lazyagents/wt"]), "");

    git(repo.path(), &["worktree", "unlock", wt_arg]);
    assert!(discard_worktree(repo.path(), &wt, "lazyagents/wt"));
    assert_eq!(git(repo.path(), &["branch", "--list", "lazyagents/wt"]), "");
    assert!(!wt.exists());
    let worktrees = git(repo.path(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains(wt_arg), "{worktrees}");
}
