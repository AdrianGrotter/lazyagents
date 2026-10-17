use std::path::PathBuf;
use std::time::Duration;

use protocol::{
    AgentId, ClientMessage, HookKind, PROTOCOL_VERSION, Request, ServerMessage, read_message,
    write_message,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::UnixStream;
use uuid::Uuid;

const BUDGET: Duration = Duration::from_millis(200);

pub fn run(event: &str) {
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(|| forward(event));
}

fn forward(event: &str) {
    let Some(kind) = HookKind::from_name(event) else {
        return;
    };
    let agent = std::env::var("LAZYAGENTS_AGENT_ID")
        .ok()
        .and_then(|s| s.parse::<Uuid>().ok());
    let socket = std::env::var_os("LAZYAGENTS_SOCKET");
    let (Some(agent), Some(socket)) = (agent, socket) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(deliver(
        kind,
        agent,
        PathBuf::from(socket),
        tokio::io::stdin(),
    ));
    runtime.shutdown_background();
}

/// Reads the payload and forwards it, giving up silently once `BUDGET` has passed.
async fn deliver(kind: HookKind, agent: Uuid, socket: PathBuf, mut input: impl AsyncRead + Unpin) {
    let _ = tokio::time::timeout(BUDGET, async {
        let mut payload = String::new();
        let _ = input.read_to_string(&mut payload).await;
        let payload = serde_json::from_str::<serde_json::Value>(&payload).ok();
        let field = |name: &str| {
            payload
                .as_ref()
                .and_then(|value| value.get(name)?.as_str().map(str::to_owned))
        };
        let Some(kind) = effective_kind(kind, payload.as_ref()) else {
            return Ok(());
        };
        let request = Request::Hook {
            id: AgentId(agent),
            kind,
            session_id: field("session_id"),
            source: field("source"),
            detail: detail(kind, payload.as_ref()),
        };
        send(socket, request).await
    })
    .await;
}

const DETAIL_CHARS: usize = 120;

/// What a hook is about, on one line of at most `DETAIL_CHARS`: a `PermissionRequest`'s tool
/// and its main input, or a `Notification`'s message.
fn detail(kind: HookKind, payload: Option<&serde_json::Value>) -> Option<String> {
    let payload = payload?;
    let text = |value: &serde_json::Value, name: &str| value.get(name)?.as_str().map(str::to_owned);
    let raw = match kind {
        HookKind::PermissionRequest => {
            let tool = text(payload, "tool_name")?;
            let input = payload.get("tool_input");
            let main = [
                "command",
                "file_path",
                "path",
                "url",
                "pattern",
                "query",
                "description",
                "prompt",
            ]
            .iter()
            .find_map(|name| input.and_then(|i| text(i, name)));
            match main {
                Some(main) => format!("{tool}: {main}"),
                None => tool,
            }
        }
        HookKind::Notification => text(payload, "message")?,
        _ => return None,
    };
    let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= DETAIL_CHARS {
        return Some(line);
    }
    let cut: String = line.chars().take(DETAIL_CHARS - 1).collect();
    Some(format!("{cut}…"))
}

/// The hook to forward for a Claude hook event, or `None` for a notification that says
/// nothing about whether Claude waits for the user. An idle-prompt `Notification` means
/// Claude waits at an empty prompt, not on a question, so it is forwarded as `Stop`.
/// Unknown notification types count as questions.
fn effective_kind(kind: HookKind, payload: Option<&serde_json::Value>) -> Option<HookKind> {
    if kind != HookKind::Notification {
        return Some(kind);
    }
    let text = |name: &str| payload.and_then(|value| value.get(name)?.as_str());
    let kind_of_notification = text("notification_type").unwrap_or_default();
    let waiting_message = text("message")
        .is_some_and(|message| message.to_lowercase().contains("waiting for your input"));
    match kind_of_notification {
        "idle_prompt" => Some(HookKind::Stop),
        "auth_success" | "elicitation_complete" | "elicitation_response" | "agent_completed" => {
            None
        }
        quota if quota.starts_with("quota_") => None,
        _ if waiting_message => Some(HookKind::Stop),
        _ => Some(HookKind::Notification),
    }
}

async fn send(socket: PathBuf, request: Request) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket).await?;
    let _welcome: Option<ServerMessage> = read_message(&mut stream).await?;
    write_message(
        &mut stream,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    write_message(&mut stream, &ClientMessage::Request { id: 1, request }).await?;
    let _reply: Option<ServerMessage> = read_message(&mut stream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notification(payload: serde_json::Value) -> Option<HookKind> {
        effective_kind(HookKind::Notification, Some(&payload))
    }

    async fn delivery_time(socket: PathBuf) -> std::time::Duration {
        let (_open, stdin) = tokio::io::duplex(64);
        let started = std::time::Instant::now();
        deliver(HookKind::Stop, Uuid::nil(), socket, stdin).await;
        started.elapsed()
    }

    #[tokio::test]
    async fn a_missing_daemon_or_open_stdin_costs_at_most_the_budget() {
        let took = delivery_time("/nonexistent/lazyagents.sock".into()).await;
        assert!(took < BUDGET + Duration::from_millis(500), "took {took:?}");
    }

    #[tokio::test]
    async fn a_daemon_that_never_answers_costs_at_most_the_budget() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("silent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let took = delivery_time(socket).await;
        assert!(took < BUDGET + Duration::from_millis(500), "took {took:?}");
    }

    #[test]
    fn idle_prompt_type_alone_is_stop() {
        assert_eq!(
            notification(json!({"notification_type": "idle_prompt"})),
            Some(HookKind::Stop)
        );
    }

    #[test]
    fn waiting_message_alone_is_stop() {
        assert_eq!(
            notification(json!({"message": "Claude is waiting for your input"})),
            Some(HookKind::Stop)
        );
    }

    #[test]
    fn waiting_message_is_matched_case_insensitively() {
        assert_eq!(
            notification(json!({"message": "CLAUDE IS WAITING FOR YOUR INPUT"})),
            Some(HookKind::Stop)
        );
    }

    #[test]
    fn questions_and_unknown_notifications_need_input() {
        for kind in [
            "permission_prompt",
            "elicitation_dialog",
            "elicitation_url_dialog",
            "agent_needs_input",
            "some_future_type",
        ] {
            assert_eq!(
                notification(json!({"notification_type": kind, "message": "?"})),
                Some(HookKind::Notification),
                "{kind}"
            );
        }
        assert_eq!(
            effective_kind(HookKind::Notification, None),
            Some(HookKind::Notification)
        );
    }

    #[test]
    fn informational_notifications_are_dropped() {
        for kind in [
            "auth_success",
            "elicitation_complete",
            "elicitation_response",
            "agent_completed",
            "quota_auto_resume_scheduled",
        ] {
            assert_eq!(
                notification(json!({"notification_type": kind})),
                None,
                "{kind}"
            );
        }
    }

    #[test]
    fn other_hooks_are_unchanged() {
        let idle = json!({"notification_type": "auth_success"});
        for kind in [
            HookKind::PreToolUse,
            HookKind::PermissionRequest,
            HookKind::StopFailure,
        ] {
            assert_eq!(effective_kind(kind, Some(&idle)), Some(kind));
        }
    }
}

#[cfg(test)]
mod detail_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_permission_request_names_the_tool_and_its_main_input() {
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "npm test\n  -- --watch", "description": "run the tests"}
        });
        assert_eq!(
            detail(HookKind::PermissionRequest, Some(&payload)).as_deref(),
            Some("Bash: npm test -- --watch")
        );
        let payload = json!({"tool_name": "Edit", "tool_input": {"file_path": "/w/a.rs"}});
        assert_eq!(
            detail(HookKind::PermissionRequest, Some(&payload)).as_deref(),
            Some("Edit: /w/a.rs")
        );
        let payload = json!({"tool_name": "mcp__x__y", "tool_input": {"n": 3}});
        assert_eq!(
            detail(HookKind::PermissionRequest, Some(&payload)).as_deref(),
            Some("mcp__x__y")
        );
    }

    #[test]
    fn a_long_detail_is_cut_short() {
        let long = "x".repeat(300);
        let payload = json!({"tool_name": "Bash", "tool_input": {"command": long}});
        let text = detail(HookKind::PermissionRequest, Some(&payload)).expect("detail");
        assert_eq!(text.chars().count(), 120);
        assert!(text.ends_with('…'), "{text}");
    }

    #[test]
    fn a_notification_gives_its_message_and_other_hooks_nothing() {
        let payload = json!({"message": "Pick a colour"});
        assert_eq!(
            detail(HookKind::Notification, Some(&payload)).as_deref(),
            Some("Pick a colour")
        );
        assert_eq!(detail(HookKind::Stop, Some(&payload)), None);
        assert_eq!(detail(HookKind::PermissionRequest, None), None);
    }
}
