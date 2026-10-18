//! `lazyagents statusline`: the status line lazyagents gives a Claude agent in place of the
//! user's own. It runs the user's command with Claude's JSON, so their line shows unchanged,
//! and forwards the usage figures in the JSON to the daemon.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use protocol::{AgentId, Request, Usage};
use serde_json::Value;

use crate::hook;

pub fn run() -> ExitCode {
    let mut payload = String::new();
    let _ = std::io::stdin().read_to_string(&mut payload);
    let json: Option<Value> = serde_json::from_str(&payload).ok();
    let theirs = their_command(json.as_ref());
    let child = theirs.as_ref().and_then(|command| {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::piped())
            .spawn()
            .ok()?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(payload.as_bytes());
        }
        Some(child)
    });
    if let Some(json) = &json {
        std::panic::set_hook(Box::new(|_| {}));
        let _ = std::panic::catch_unwind(|| forward(usage(json)));
        let _ = std::panic::take_hook();
    }
    match child.map(|mut child| child.wait()) {
        Some(Ok(status)) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Some(Err(_)) => ExitCode::FAILURE,
        None => ExitCode::SUCCESS,
    }
}

/// The user's status line command for the directory Claude was started in.
fn their_command(json: Option<&Value>) -> Option<String> {
    let text = |path: &[&str]| {
        path.iter()
            .try_fold(json?, |value, key| value.get(key))
            .and_then(Value::as_str)
            .map(PathBuf::from)
    };
    let project = text(&["workspace", "project_dir"])
        .or_else(|| text(&["cwd"]))
        .or_else(|| std::env::current_dir().ok())?;
    let line = daemon::claude_settings::status_line(
        &project,
        daemon::claude_settings::config_dir().as_deref(),
    )?;
    line.get("command")?.as_str().map(str::to_string)
}

/// The figures lazyagents shows, from the status line JSON.
pub fn usage(json: &Value) -> Usage {
    let at = |path: &[&str]| path.iter().try_fold(json, |value, key| value.get(key));
    let text = |path: &[&str]| at(path).and_then(Value::as_str).map(str::to_string);
    let percent = |path: &[&str]| {
        at(path)
            .and_then(Value::as_f64)
            .filter(|p| p.is_finite())
            .map(|p| p.round().clamp(0.0, 100.0) as u8)
    };
    Usage {
        model: text(&["model", "display_name"]),
        cost_micros: at(&["cost", "total_cost_usd"])
            .and_then(Value::as_f64)
            .filter(|usd| usd.is_finite() && *usd >= 0.0)
            .map(|usd| (usd * 1_000_000.0).round() as u64),
        context_percent: percent(&["context_window", "used_percentage"]),
        five_hour_percent: percent(&["rate_limits", "five_hour", "used_percentage"]),
        seven_day_percent: percent(&["rate_limits", "seven_day", "used_percentage"]),
        pr_number: at(&["pr", "number"])
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        pr_review: text(&["pr", "review_state"]),
    }
}

fn forward(usage: Usage) {
    hook::block_on_agent(|agent, socket| {
        let request = Request::Usage {
            id: AgentId(agent),
            usage,
        };
        async move {
            let _ = tokio::time::timeout(hook::BUDGET, hook::send(socket, request)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_figures_are_read_from_their_documented_places() {
        let json = json!({
            "model": {"id": "claude-opus-5-5", "display_name": "Opus"},
            "cost": {"total_cost_usd": 0.4237},
            "context_window": {"used_percentage": 22.6},
            "rate_limits": {
                "five_hour": {"used_percentage": 23.5, "resets_at": 1},
                "seven_day": {"used_percentage": 41.2, "resets_at": 2}
            },
            "pr": {"number": 7, "url": "https://x/7", "review_state": "approved"}
        });
        assert_eq!(
            usage(&json),
            Usage {
                model: Some("Opus".into()),
                cost_micros: Some(423_700),
                context_percent: Some(23),
                five_hour_percent: Some(24),
                seven_day_percent: Some(41),
                pr_number: Some(7),
                pr_review: Some("approved".into()),
            }
        );
    }

    #[test]
    fn missing_or_odd_figures_are_none() {
        assert_eq!(usage(&json!({})), Usage::default());
        let odd = json!({
            "cost": {"total_cost_usd": -1},
            "context_window": {"used_percentage": 250},
            "pr": {"number": "seven"}
        });
        let parsed = usage(&odd);
        assert_eq!(parsed.cost_micros, None);
        assert_eq!(parsed.context_percent, Some(100));
        assert_eq!(parsed.pr_number, None);
    }
}
