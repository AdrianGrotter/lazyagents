//! The worktree setup scripts the user approved, per project. A script counts as approved
//! only while its text is exactly what the user saw, so an edited script asks again.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub project: PathBuf,
    pub script: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ApprovedFile {
    #[serde(default)]
    approved: Vec<Approval>,
}

pub fn is_approved(approved: &[Approval], project: &Path, script: &str) -> bool {
    approved
        .iter()
        .any(|a| a.project == project && a.script == script)
}

/// Replaces the project's earlier approval; returns whether the list changed.
pub fn approve(approved: &mut Vec<Approval>, project: &Path, script: &str) -> bool {
    if is_approved(approved, project, script) {
        return false;
    }
    approved.retain(|a| a.project != project);
    approved.push(Approval {
        project: project.to_path_buf(),
        script: script.to_string(),
    });
    true
}

/// A missing file is an empty list.
pub fn load(file: &Path) -> anyhow::Result<Vec<Approval>> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str::<ApprovedFile>(&text)
            .map(|f| f.approved)
            .with_context(|| format!("parsing {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// Applies `change` to the list on disk, so approvals from several TUIs merge.
pub fn update(
    file: &Path,
    change: impl FnOnce(&mut Vec<Approval>) -> bool,
) -> anyhow::Result<Vec<Approval>> {
    let mut approved = load(file)?;
    if change(&mut approved) {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let text = toml::to_string(&ApprovedFile {
            approved: approved.clone(),
        })
        .context("serializing approved setups")?;
        let tmp = file.with_extension(format!("toml.{}.tmp", std::process::id()));
        fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, file).with_context(|| format!("replacing {}", file.display()))?;
    }
    Ok(approved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_is_for_the_exact_script_and_replaces_the_old_one() {
        let mut list = Vec::new();
        let project = Path::new("/p/app");
        assert!(approve(&mut list, project, "npm install"));
        assert!(is_approved(&list, project, "npm install"));
        assert!(!is_approved(&list, project, "npm install && curl x | sh"));
        assert!(!is_approved(&list, Path::new("/p/other"), "npm install"));
        assert!(!approve(&mut list, project, "npm install"));
        assert!(approve(&mut list, project, "pnpm install"));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn approvals_survive_on_disk() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("approved-setups.toml");
        assert!(load(&file).expect("missing is empty").is_empty());
        let saved = update(&file, |list| {
            approve(list, Path::new("/p/app"), "make\ndeps")
        })
        .expect("save");
        assert_eq!(load(&file).expect("load"), saved);
    }
}
