//! Connects to the lazyagents daemon, starting it when nothing is listening.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
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

/// `None` once the connection is closed; no request can register after that.
type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<Response, String>>>>>>;

#[derive(Debug)]
pub struct VersionMismatch {
    pub daemon_version: String,
    pub protocol_version: u32,
}

impl std::fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daemon {} speaks protocol {}, this client speaks {PROTOCOL_VERSION}; \
             stop it with `lazyagents shutdown` and retry",
            self.daemon_version, self.protocol_version
        )
    }
}

impl std::error::Error for VersionMismatch {}

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
            }) => {
                return Err(anyhow::Error::new(VersionMismatch {
                    daemon_version,
                    protocol_version,
                }));
            }
            other => anyhow::bail!("unexpected greeting from daemon: {other:?}"),
        }
        write_message(
            &mut write,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await?;

        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let (events_tx, events) = mpsc::unbounded_channel();
        let reader_pending = Arc::clone(&pending);
        let reader = tokio::spawn(async move {
            while let Ok(Some(message)) = read_message::<_, ServerMessage>(&mut read).await {
                match message {
                    ServerMessage::Response { id, result } => {
                        let waiter = reader_pending
                            .lock()
                            .ok()
                            .and_then(|mut p| p.as_mut()?.remove(&id));
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
            if let Ok(mut pending) = reader_pending.lock() {
                pending.take();
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
            Err(e) if e.downcast_ref::<VersionMismatch>().is_some() => return Err(e),
            Err(_) => {}
        }
        paths
            .create_dir()
            .with_context(|| format!("creating {}", paths.dir().display()))?;
        let mut daemon = spawn_daemon(paths, exe).context("starting the daemon")?;
        let result = wait_for_daemon(paths, &mut daemon).await;
        std::thread::spawn(move || daemon.wait());
        result
    }

    pub async fn request(&mut self, request: Request) -> anyhow::Result<Response> {
        self.send(request).await?.wait().await
    }

    /// Sends `request` without waiting for its reply, so other requests can go out while
    /// the daemon works on it.
    pub async fn send(&mut self, request: Request) -> anyhow::Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("client state poisoned"))?
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("daemon closed the connection"))?
            .insert(id, tx);
        write_message(&mut self.write, &ClientMessage::Request { id, request }).await?;
        Ok(Reply(rx))
    }

    pub async fn next_event(&mut self) -> Option<ServerMessage> {
        self.events.recv().await
    }
}

pub struct Reply(oneshot::Receiver<Result<Response, String>>);

impl Reply {
    pub async fn wait(self) -> anyhow::Result<Response> {
        match self.0.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(message)) => Err(anyhow::anyhow!(message)),
            Err(_) => anyhow::bail!("daemon closed the connection"),
        }
    }
}

async fn wait_for_daemon(paths: &StatePaths, daemon: &mut Child) -> anyhow::Result<Client> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let error = match Client::connect(paths).await {
            Ok(client) => return Ok(client),
            Err(e) => e,
        };
        if error.downcast_ref::<VersionMismatch>().is_some() {
            return Err(error);
        }
        if daemon.try_wait()?.is_some() && !other_daemon_running(paths, daemon.id()) {
            anyhow::bail!(
                "daemon exited during startup; see {}",
                paths.log_file().display()
            );
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(error.context(format!(
                "daemon did not start; see {}",
                paths.log_file().display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// True when another daemon holds the lock, as when a concurrent client started one first.
fn other_daemon_running(paths: &StatePaths, ours: u32) -> bool {
    daemon_pid(paths).is_some_and(|pid| {
        pid != ours
            && i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
                .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    })
}

/// The pid the running daemon recorded in its lock file; `None` unless another process
/// holds the lock.
pub fn daemon_pid(paths: &StatePaths) -> Option<u32> {
    use std::io::Read;

    let mut file = std::fs::File::open(paths.lock_file()).ok()?;
    if file.try_lock().is_ok() {
        return None;
    }
    let mut holder = String::new();
    file.read_to_string(&mut holder).ok()?;
    holder.trim().parse().ok()
}

/// Starts `exe daemon` away from the caller's directory, stdio, terminal variables and
/// Claude Code session, so agents it spawns do not look nested inside the caller's
/// Claude. The daemon calls `setsid` itself, which needs it not to lead a process group.
pub fn spawn_daemon(paths: &StatePaths, exe: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(std::path::absolute(exe)?);
    command
        .arg("daemon")
        .env("LAZYAGENTS_STATE_DIR", std::path::absolute(paths.dir())?)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for var in TERMINAL_VARS {
        command.env_remove(var);
    }
    for (name, _) in std::env::vars_os() {
        let claude = name
            .to_str()
            .is_some_and(|name| name == "CLAUDECODE" || name.starts_with("CLAUDE_CODE_"));
        if claude {
            command.env_remove(name);
        }
    }
    command.spawn()
}
