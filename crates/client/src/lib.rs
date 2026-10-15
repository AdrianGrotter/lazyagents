//! Connects to the lazyagents daemon, starting it when nothing is listening.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use protocol::{
    ClientMessage, PROTOCOL_VERSION, Request, Response, ServerMessage, StatePaths, read_message,
    write_message,
};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot};

const TERMINAL_VARS: [&str; 13] = [
    "TMUX",
    "TMUX_PANE",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
    "KITTY_WINDOW_ID",
    "KITTY_PID",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "VSCODE_INJECTION",
];

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Response, String>>>>>;

pub struct Client {
    write: OwnedWriteHalf,
    next_id: u64,
    pending: Pending,
    events: mpsc::UnboundedReceiver<ServerMessage>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Client {
    pub async fn connect(paths: &StatePaths) -> anyhow::Result<Client> {
        let socket = paths.socket();
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        let (mut read, mut write) = stream.into_split();
        let welcome = tokio::time::timeout(
            Duration::from_secs(2),
            read_message::<_, ServerMessage>(&mut read),
        )
        .await
        .context("daemon did not greet in time")??;
        match welcome {
            Some(ServerMessage::Welcome {
                protocol_version, ..
            }) if protocol_version == PROTOCOL_VERSION => {}
            Some(ServerMessage::Welcome {
                protocol_version,
                daemon_version,
            }) => anyhow::bail!(
                "daemon {daemon_version} speaks protocol {protocol_version}, this client speaks {PROTOCOL_VERSION}; \
                 stop it with `lazyagents shutdown` and retry"
            ),
            other => anyhow::bail!("unexpected greeting from daemon: {other:?}"),
        }
        write_message(
            &mut write,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;

        let pending: Pending = Arc::default();
        let (events_tx, events) = mpsc::unbounded_channel();
        let reader_pending = Arc::clone(&pending);
        let reader = tokio::spawn(async move {
            while let Ok(Some(message)) = read_message::<_, ServerMessage>(&mut read).await {
                match message {
                    ServerMessage::Response { id, result } => {
                        let waiter = reader_pending.lock().ok().and_then(|mut p| p.remove(&id));
                        if let Some(waiter) = waiter {
                            let _ = waiter.send(result);
                        }
                    }
                    other => {
                        if events_tx.send(other).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Ok(Client {
            write,
            next_id: 1,
            pending,
            events,
            reader,
        })
    }

    pub async fn connect_or_spawn(paths: &StatePaths, exe: &Path) -> anyhow::Result<Client> {
        match Client::connect(paths).await {
            Ok(client) => return Ok(client),
            Err(e) if e.to_string().contains("protocol") => return Err(e),
            Err(_) => {}
        }
        std::fs::create_dir_all(paths.dir())?;
        spawn_daemon(paths, exe).context("starting the daemon")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match Client::connect(paths).await {
                Ok(client) => return Ok(client),
                Err(e) if tokio::time::Instant::now() >= deadline => {
                    return Err(e.context(format!(
                        "daemon did not start; see {}",
                        paths.log_file().display()
                    )));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }

    pub async fn request(&mut self, request: Request) -> anyhow::Result<Response> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("client state poisoned"))?
            .insert(id, tx);
        write_message(&mut self.write, &ClientMessage::Request { id, request }).await?;
        match rx.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(message)) => Err(anyhow::anyhow!(message)),
            Err(_) => anyhow::bail!("daemon closed the connection"),
        }
    }

    pub async fn next_event(&mut self) -> Option<ServerMessage> {
        self.events.recv().await
    }
}

pub fn spawn_daemon(paths: &StatePaths, exe: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .env("LAZYAGENTS_STATE_DIR", paths.dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    for var in TERMINAL_VARS {
        command.env_remove(var);
    }
    command.spawn().map(drop)
}
