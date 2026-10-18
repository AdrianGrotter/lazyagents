use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use protocol::{AgentId, AgentKind, Project, ProjectId, WorktreeInfo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: AgentId,
    pub project: ProjectId,
    pub name: String,
    pub kind: AgentKind,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub resumable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredState {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub agents: Vec<AgentRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_worktrees: Vec<PendingWorktree>,
}

/// A worktree, with its branch, that git may be making for an agent not recorded yet; what
/// a create left when the daemon stopped before it finished is removed at the next start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingWorktree {
    pub project: ProjectId,
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
}

pub fn load(path: &Path) -> anyhow::Result<StoredState> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => anyhow::bail!("{} is empty", path.display()),
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoredState::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Replaces the file atomically and durably: the new text reaches the disk before the
/// rename, and the rename before this returns.
pub fn save(path: &Path, state: &StoredState) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let text = toml::to_string(state).context("serializing state")?;
    let tmp = path.with_extension("toml.tmp");
    fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredState {
        let project = Project {
            id: ProjectId::random(),
            name: "lazyagents".into(),
            path: "/Users/u/git/lazyagents".into(),
        };
        StoredState {
            agents: vec![
                AgentRecord {
                    id: AgentId::random(),
                    project: project.id,
                    name: "api".into(),
                    kind: AgentKind::Claude,
                    command: vec!["claude".into()],
                    cwd: project.path.clone(),
                    session_id: Some(Uuid::new_v4()),
                    resumable: true,
                    worktree: Some(WorktreeInfo {
                        path: "/Users/u/git/lazyagents/.worktrees/api".into(),
                        branch: "agent/api".into(),
                        base: "main".into(),
                    }),
                    title: None,
                },
                AgentRecord {
                    id: AgentId::random(),
                    project: project.id,
                    name: "shell".into(),
                    kind: AgentKind::Generic,
                    command: vec!["sh".into(), "-c".into(), "echo 'hi'".into()],
                    cwd: project.path.clone(),
                    session_id: None,
                    resumable: false,
                    worktree: None,
                    title: None,
                },
            ],
            projects: vec![project],
            pending_worktrees: Vec::new(),
        }
    }

    #[test]
    fn missing_file_loads_empty_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            load(&dir.path().join("state.toml")).expect("load"),
            StoredState::default()
        );
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/state.toml");
        let state = sample();
        save(&path, &state).expect("save");
        assert_eq!(load(&path).expect("load"), state);
    }

    #[test]
    fn state_without_worktrees_still_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        let id = AgentId::random();
        let project = ProjectId::random();
        fs::write(
            &path,
            format!(
                "[[projects]]\nid = \"{project}\"\nname = \"p\"\npath = \"/p\"\n\n\
                 [[agents]]\nid = \"{id}\"\nproject = \"{project}\"\nname = \"a\"\n\
                 kind = \"Generic\"\ncommand = [\"sh\"]\ncwd = \"/p\"\nresumable = false\n"
            ),
        )
        .expect("write");
        let state = load(&path).expect("load");
        assert_eq!(state.agents.len(), 1);
        assert_eq!(state.agents[0].worktree, None);
    }

    #[test]
    fn worktree_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        let state = sample();
        save(&path, &state).expect("save");
        let loaded = load(&path).expect("load");
        let worktree = loaded.agents[0].worktree.as_ref().expect("worktree");
        assert_eq!(worktree.branch, "agent/api");
        assert_eq!(worktree.base, "main");
        assert_eq!(loaded.agents[1].worktree, None);
    }

    #[test]
    fn save_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        save(&path, &sample()).expect("save");
        let names: Vec<_> = fs::read_dir(dir.path())
            .expect("read dir")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("state.toml")]);
    }

    #[test]
    fn corrupt_file_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        fs::write(&path, "projects = 7\n[[[").expect("write");
        let err = load(&path).expect_err("corrupt");
        assert!(format!("{err:#}").contains("state.toml"), "{err:#}");
    }

    #[test]
    fn an_empty_file_is_corrupt_not_an_empty_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        for text in ["", " \n\n"] {
            fs::write(&path, text).expect("write");
            let err = load(&path).expect_err("empty");
            assert!(
                format!("{err:#}").contains("state.toml is empty"),
                "{err:#}"
            );
        }
    }

    #[test]
    fn an_empty_state_is_saved_as_a_file_that_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        save(&path, &StoredState::default()).expect("save");
        assert_eq!(load(&path).expect("load"), StoredState::default());
    }

    #[test]
    fn a_state_file_from_before_pending_worktrees_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        fs::write(
            &path,
            "[[projects]]\n\
             id = \"7d0c1f5e-2b4a-4f7e-9c51-0a6f3d2e8b11\"\n\
             name = \"lazyagents\"\n\
             path = \"/Users/u/git/lazyagents\"\n\
             \n\
             [[agents]]\n\
             id = \"0b6f4c2a-93d1-4e8f-a7b5-6c2d1e9f3a40\"\n\
             project = \"7d0c1f5e-2b4a-4f7e-9c51-0a6f3d2e8b11\"\n\
             name = \"api\"\n\
             kind = \"Claude\"\n\
             command = [\"claude\"]\n\
             cwd = \"/Users/u/.local/state/lazyagents/worktrees/lazyagents-7d0c1f5e/api\"\n\
             session_id = \"5f1e2d3c-4b5a-4968-8776-655443322110\"\n\
             resumable = true\n\
             title = \"Fix the parser\"\n\
             \n\
             [agents.worktree]\n\
             path = \"/Users/u/.local/state/lazyagents/worktrees/lazyagents-7d0c1f5e/api\"\n\
             branch = \"lazyagents/api\"\n\
             base = \"main\"\n",
        )
        .expect("write");
        let state = load(&path).expect("load");
        assert_eq!(state.projects.len(), 1);
        assert_eq!(state.agents.len(), 1);
        assert_eq!(
            state.agents[0].worktree.as_ref().map(|w| w.branch.as_str()),
            Some("lazyagents/api")
        );
        assert!(state.pending_worktrees.is_empty());
    }

    #[test]
    fn pending_worktrees_round_trip_and_stay_out_of_the_file_when_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.toml");
        let mut state = sample();
        save(&path, &state).expect("save");
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains("pending"), "{text}");
        state.pending_worktrees.push(PendingWorktree {
            project: state.projects[0].id,
            name: "web".into(),
            path: "/Users/u/.local/state/lazyagents/worktrees/lazyagents-7d0c1f5e/web".into(),
            branch: "lazyagents/web".into(),
        });
        save(&path, &state).expect("save");
        assert_eq!(load(&path).expect("load"), state);
    }
}
