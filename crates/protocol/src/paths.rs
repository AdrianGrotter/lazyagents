use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::AgentId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatePaths {
    dir: PathBuf,
}

impl StatePaths {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn from_env() -> io::Result<Self> {
        Self::resolve(
            std::env::var_os("LAZYAGENTS_STATE_DIR"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub fn resolve(
        state_dir: Option<OsString>,
        xdg_state_home: Option<OsString>,
        home: Option<OsString>,
    ) -> io::Result<Self> {
        let non_empty = |value: Option<OsString>| value.filter(|v| !v.is_empty());
        if let Some(dir) = non_empty(state_dir) {
            return Ok(Self::new(std::path::absolute(dir)?));
        }
        if let Some(xdg) = non_empty(xdg_state_home) {
            return Ok(Self::new(PathBuf::from(xdg).join("lazyagents")));
        }
        let home = non_empty(home).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "neither LAZYAGENTS_STATE_DIR nor HOME is set",
            )
        })?;
        Ok(Self::new(
            PathBuf::from(home).join(".local/state/lazyagents"),
        ))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Creates the state dir, and any missing parents, readable only by the user.
    pub fn create_dir(&self) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join("daemon.sock")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.dir.join("daemon.lock")
    }

    pub fn state_file(&self) -> PathBuf {
        self.dir.join("state.toml")
    }

    /// Written by the TUI, never by the daemon.
    pub fn known_projects(&self) -> PathBuf {
        self.dir.join("known-projects.toml")
    }

    /// The worktree setup scripts the user approved; written by the TUI, never by the daemon.
    pub fn approved_setups(&self) -> PathBuf {
        self.dir.join("approved-setups.toml")
    }

    pub fn log_file(&self) -> PathBuf {
        self.dir.join("daemon.log")
    }

    pub fn hooks_dir(&self) -> PathBuf {
        self.dir.join("hooks")
    }

    pub fn hook_settings(&self, agent: AgentId) -> PathBuf {
        self.hooks_dir().join(format!("{agent}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn explicit_state_dir_wins() {
        let paths = StatePaths::resolve(os("/tmp/la"), os("/x"), os("/home/u")).expect("paths");
        assert_eq!(paths.dir(), Path::new("/tmp/la"));
        assert_eq!(paths.socket(), Path::new("/tmp/la/daemon.sock"));
    }

    #[test]
    fn relative_state_dir_is_made_absolute() {
        let paths = StatePaths::resolve(os("rel/dir"), None, os("/home/u")).expect("paths");
        assert!(paths.dir().is_absolute(), "{}", paths.dir().display());
        assert_eq!(
            paths.dir(),
            std::env::current_dir().expect("cwd").join("rel/dir")
        );
    }

    #[test]
    fn empty_state_dir_is_ignored() {
        let paths = StatePaths::resolve(os(""), os("/x/state"), os("/home/u")).expect("paths");
        assert_eq!(paths.dir(), Path::new("/x/state/lazyagents"));
    }

    #[test]
    fn falls_back_to_home() {
        let paths = StatePaths::resolve(None, None, os("/home/u")).expect("paths");
        assert_eq!(paths.dir(), Path::new("/home/u/.local/state/lazyagents"));
        assert_eq!(
            paths.state_file(),
            Path::new("/home/u/.local/state/lazyagents/state.toml")
        );
    }

    #[test]
    fn no_home_is_an_error() {
        assert!(StatePaths::resolve(None, None, None).is_err());
    }

    #[test]
    fn hook_settings_live_under_hooks() {
        let paths = StatePaths::new("/s");
        let id = AgentId::random();
        assert_eq!(
            paths.hook_settings(id),
            PathBuf::from(format!("/s/hooks/{id}.json"))
        );
    }
}
