//! Projects the user has added before, kept after they leave the sidebar so the add
//! picker can offer them again. Most recently added first; one entry per path.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownProject {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownFile {
    #[serde(default)]
    projects: Vec<KnownProject>,
}

/// Puts `added` at the front, in order, replacing older entries with the same path.
/// Returns whether the list changed.
pub fn remember(known: &mut Vec<KnownProject>, added: &[KnownProject]) -> bool {
    let mut next = added.to_vec();
    next.extend(
        known
            .iter()
            .filter(|old| !added.iter().any(|new| new.path == old.path))
            .cloned(),
    );
    let changed = next != *known;
    *known = next;
    changed
}

/// Returns whether an entry was removed.
pub fn forget(known: &mut Vec<KnownProject>, path: &Path) -> bool {
    let before = known.len();
    known.retain(|p| p.path != path);
    known.len() != before
}

/// A missing file is an empty list.
pub fn load(file: &Path) -> anyhow::Result<Vec<KnownProject>> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str::<KnownFile>(&text)
            .map(|f| f.projects)
            .with_context(|| format!("parsing {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

/// Writes through a temp file named after this process, so two TUIs never share one.
pub fn save(file: &Path, known: &[KnownProject]) -> anyhow::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let text = toml::to_string(&KnownFile {
        projects: known.to_vec(),
    })
    .context("serializing known projects")?;
    let tmp = file.with_extension(format!("toml.{}.tmp", std::process::id()));
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, file).with_context(|| format!("replacing {}", file.display()))
}

/// Applies `change` to the list on disk, so changes from several TUIs merge, and returns
/// the result. A file that cannot be parsed is left alone.
pub fn update(
    file: &Path,
    change: impl FnOnce(&mut Vec<KnownProject>) -> bool,
) -> anyhow::Result<Vec<KnownProject>> {
    let mut known = load(file)?;
    if change(&mut known) {
        save(file, &known)?;
    }
    Ok(known)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(name: &str) -> KnownProject {
        KnownProject {
            name: name.into(),
            path: format!("/p/{name}").into(),
        }
    }

    #[test]
    fn remember_puts_added_projects_first_in_order() {
        let mut list = vec![known("a"), known("b")];
        assert!(remember(&mut list, &[known("c"), known("b")]));
        assert_eq!(list, vec![known("c"), known("b"), known("a")]);
    }

    #[test]
    fn remember_replaces_the_name_for_a_known_path() {
        let mut list = vec![known("a")];
        let renamed = KnownProject {
            name: "alpha".into(),
            path: "/p/a".into(),
        };
        assert!(remember(&mut list, std::slice::from_ref(&renamed)));
        assert_eq!(list, vec![renamed]);
    }

    #[test]
    fn remember_reports_no_change_when_already_in_front() {
        let mut list = vec![known("a"), known("b")];
        assert!(!remember(&mut list, &[known("a")]));
        assert!(!remember(&mut list, &[]));
    }

    #[test]
    fn forget_removes_by_path() {
        let mut list = vec![known("a"), known("b")];
        assert!(forget(&mut list, Path::new("/p/a")));
        assert_eq!(list, vec![known("b")]);
        assert!(!forget(&mut list, Path::new("/p/a")));
    }

    #[test]
    fn saved_list_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/known-projects.toml");
        assert_eq!(load(&file).unwrap(), Vec::new());
        let list = vec![known("a"), known("b")];
        save(&file, &list).unwrap();
        assert_eq!(load(&file).unwrap(), list);
    }

    #[test]
    fn update_merges_with_what_is_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known-projects.toml");
        save(&file, &[known("other-tui")]).unwrap();
        let list = update(&file, |list| remember(list, &[known("a")])).unwrap();
        assert_eq!(list, vec![known("a"), known("other-tui")]);
        assert_eq!(load(&file).unwrap(), list);
    }

    #[test]
    fn update_leaves_an_unparsable_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known-projects.toml");
        fs::write(&file, "projects = 7\n[[[").unwrap();
        assert!(update(&file, |list| remember(list, &[known("a")])).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "projects = 7\n[[[");
    }
}
