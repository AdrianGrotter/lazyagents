use std::path::PathBuf;
use std::time::Duration;

use protocol::{
    AgentId, ClientMessage, HookKind, PROTOCOL_VERSION, Request, ServerMessage, read_message,
    write_message,
};
use tokio::io::AsyncReadExt;
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
    let _ = runtime.block_on(async {
        tokio::time::timeout(BUDGET, async {
            let mut payload = String::new();
            let _ = tokio::io::stdin().read_to_string(&mut payload).await;
            let payload = serde_json::from_str::<serde_json::Value>(&payload).ok();
            let field = |name: &str| {
                payload
                    .as_ref()
                    .and_then(|value| value.get(name)?.as_str().map(str::to_owned))
            };
            let idle_prompt = field("notification_type").as_deref() == Some("idle_prompt")
                || field("message").is_some_and(|message| {
                    message.to_lowercase().contains("waiting for your input")
                });
            let kind = if kind == HookKind::Notification && idle_prompt {
                HookKind::Stop
            } else {
                kind
            };
            let request = Request::Hook {
                id: AgentId(agent),
                kind,
                session_id: field("session_id"),
                source: field("source"),
            };
            send(PathBuf::from(socket), request).await
        })
        .await
    });
    runtime.shutdown_background();
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
