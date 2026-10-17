//! A project's own `.lazyagents.toml`, which travels with the repository.

use std::path::Path;

use toml::{Table, Value};

pub const FILE: &str = ".lazyagents.toml";

/// The `[worktree] setup` script of `project`, `None` without one. A file that cannot be
/// read or parsed is an error, so a broken setup is never skipped silently.
pub fn worktree_setup(project: &Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(project.join(FILE)) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{FILE}: {error}")),
    };
    let root: Table = text
        .parse()
        .map_err(|error: toml::de::Error| format!("{FILE}: {}", error.message().trim()))?;
    let setup = root
        .get("worktree")
        .and_then(Value::as_table)
        .and_then(|worktree| worktree.get("setup"));
    match setup {
        None => Ok(None),
        Some(Value::String(script)) if script.trim().is_empty() => Ok(None),
        Some(Value::String(script)) => Ok(Some(script.clone())),
        Some(_) => Err(format!("{FILE}: worktree.setup must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(text: &str) -> (tempfile::TempDir, Result<Option<String>, String>) {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(dir.path().join(FILE), text).expect("write");
        let result = worktree_setup(dir.path());
        (dir, result)
    }

    #[test]
    fn the_setup_script_is_read() {
        let (_dir, setup) = with("[worktree]\nsetup = \"npm install\"\n");
        assert_eq!(setup, Ok(Some("npm install".into())));
    }

    #[test]
    fn no_file_or_no_setup_is_none() {
        let dir = tempfile::tempdir().expect("project");
        assert_eq!(worktree_setup(dir.path()), Ok(None));
        let (_dir, setup) = with("[worktree]\n");
        assert_eq!(setup, Ok(None));
        let (_dir, setup) = with("[worktree]\nsetup = \"  \"\n");
        assert_eq!(setup, Ok(None));
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let (_dir, setup) = with("[worktree\n");
        assert!(setup.is_err_and(|e| e.contains(".lazyagents.toml")));
        let (_dir, setup) = with("[worktree]\nsetup = 3\n");
        assert_eq!(
            setup,
            Err(".lazyagents.toml: worktree.setup must be a string".into())
        );
    }
}
