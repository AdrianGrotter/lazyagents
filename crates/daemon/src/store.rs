use std::fs;
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredState {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub agents: Vec<AgentRecord>,
}

pub fn load(path: &Path) -> anyhow::Result<StoredState> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoredState::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, state: &StoredState) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string(state).context("serializing state")?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
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
                },
            ],
            projects: vec![project],
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
}
