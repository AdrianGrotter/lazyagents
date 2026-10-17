//! Reads the user's own Claude Code settings, which lazyagents never writes.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// The `statusLine` Claude would use in `project`: from `.claude/settings.local.json`,
/// else `.claude/settings.json`, else the user's `settings.json`, the first file that has
/// one winning whole. Only a `command` status line counts.
pub fn status_line(project: &Path, config_dir: Option<&Path>) -> Option<Map<String, Value>> {
    let files = [
        Some(project.join(".claude/settings.local.json")),
        Some(project.join(".claude/settings.json")),
        config_dir.map(|dir| dir.join("settings.json")),
    ];
    let line = files.into_iter().flatten().find_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        let settings: Value = serde_json::from_str(&text).ok()?;
        settings.get("statusLine")?.as_object().cloned()
    })?;
    let command = line
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (line.get("type").and_then(Value::as_str) == Some("command") && !command.trim().is_empty())
        .then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, name: &str, value: Value) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, value.to_string()).expect("write");
    }

    #[test]
    fn the_most_local_status_line_wins() {
        let project = tempfile::tempdir().expect("project");
        let config = tempfile::tempdir().expect("config");
        let line = |command: &str| json!({"statusLine": {"type": "command", "command": command, "padding": 1}});
        write(config.path(), "settings.json", line("user.sh"));
        let found = |p: &Path| {
            status_line(p, Some(config.path()))
                .and_then(|s| s.get("command").and_then(Value::as_str).map(str::to_string))
        };
        assert_eq!(found(project.path()).as_deref(), Some("user.sh"));
        write(project.path(), ".claude/settings.json", line("project.sh"));
        assert_eq!(found(project.path()).as_deref(), Some("project.sh"));
        write(
            project.path(),
            ".claude/settings.local.json",
            line("local.sh"),
        );
        assert_eq!(found(project.path()).as_deref(), Some("local.sh"));
        let whole = status_line(project.path(), Some(config.path())).expect("line");
        assert_eq!(whole.get("padding"), Some(&json!(1)));
    }

    #[test]
    fn files_without_a_usable_status_line_are_skipped() {
        let project = tempfile::tempdir().expect("project");
        let config = tempfile::tempdir().expect("config");
        assert_eq!(status_line(project.path(), Some(config.path())), None);
        assert_eq!(status_line(project.path(), None), None);
        write(
            project.path(),
            ".claude/settings.local.json",
            json!({"model": "opus"}),
        );
        std::fs::write(project.path().join(".claude/settings.json"), "{ not json").expect("write");
        write(
            config.path(),
            "settings.json",
            json!({"statusLine": {"type": "static", "text": "hi"}}),
        );
        assert_eq!(status_line(project.path(), Some(config.path())), None);
        write(
            config.path(),
            "settings.json",
            json!({"statusLine": {"type": "command", "command": "  "}}),
        );
        assert_eq!(status_line(project.path(), Some(config.path())), None);
    }
}
