use std::path::Path;

use serde_json::{Value, json};

const EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "Notification",
    "Stop",
];

pub fn settings_json(log: &Path) -> Value {
    let quoted = format!("'{}'", log.display().to_string().replace('\'', r"'\''"));
    let mut hooks = serde_json::Map::new();
    for event in EVENTS {
        let command = format!("cat >/dev/null; printf '%s\\n' {event} >> {quoted}");
        hooks.insert(
            event.to_string(),
            json!([{ "hooks": [{ "type": "command", "command": command }] }]),
        );
    }
    json!({ "hooks": hooks })
}

pub fn status_from_log(log: &str) -> &'static str {
    match log.lines().last() {
        None => "starting",
        Some("SessionStart" | "Stop") => "idle",
        Some("UserPromptSubmit" | "PreToolUse") => "working",
        Some("Notification") => "needs input",
        Some(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_register_every_event() {
        let value = settings_json(Path::new("/tmp/x/hooks.log"));
        for event in EVENTS {
            let command = value["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("{event} has a command"));
            assert!(command.contains(event), "{command}");
            assert!(command.contains("'/tmp/x/hooks.log'"), "{command}");
            assert_eq!(value["hooks"][event][0]["hooks"][0]["type"], "command");
        }
    }

    #[test]
    fn hook_command_appends_event_to_log() {
        let dir =
            std::env::temp_dir().join(format!("lazyagents-spike-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let log = dir.join("hooks.log");
        let value = settings_json(&log);
        let command = value["hooks"]["Notification"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command");
        let status = std::process::Command::new("sh")
            .args(["-c", command])
            .stdin(std::process::Stdio::null())
            .status()
            .expect("runs");
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(&log).expect("log written"),
            "Notification\n"
        );
        std::fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn status_follows_last_event() {
        assert_eq!(status_from_log(""), "starting");
        assert_eq!(status_from_log("SessionStart\n"), "idle");
        assert_eq!(
            status_from_log("SessionStart\nUserPromptSubmit\n"),
            "working"
        );
        assert_eq!(status_from_log("PreToolUse\nNotification\n"), "needs input");
        assert_eq!(status_from_log("PreToolUse\nStop\n"), "idle");
        assert_eq!(status_from_log("Mystery\n"), "unknown");
    }
}
