//! The project the Agents box listed when the TUI last changed it, so the next TUI starts
//! there. Projects are kept by path, which outlives the daemon's project ids.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentView {
    /// The Agents box lists all agents.
    #[serde(default)]
    pub all: bool,
    /// The current project, kept while all agents are listed too.
    pub project: Option<PathBuf>,
}

/// A missing file is the default view.
pub fn load(file: &Path) -> anyhow::Result<CurrentView> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CurrentView::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// Writes through a temp file named after this process, so two TUIs never share one.
pub fn save(file: &Path, view: &CurrentView) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string(view).context("serializing the current project")?;
    let tmp = file.with_extension(format!("toml.{}.tmp", std::process::id()));
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, file).with_context(|| format!("replacing {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_view_loads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("current-project.toml");
        assert_eq!(load(&file).expect("missing file"), CurrentView::default());
        let view = CurrentView {
            all: true,
            project: Some("/p/q".into()),
        };
        save(&file, &view).expect("save");
        assert_eq!(load(&file).expect("load"), view);
    }
}
