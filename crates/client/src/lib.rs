//! Connects to the lazyagents daemon, starting it when nothing is listening.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use protocol::lockfile::{self, Holder};
use protocol::{
    ClientMessage, Failure, PROTOCOL_VERSION, Request, Response, ServerMessage, StatePaths,
    read_message, write_message,
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
type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<Response, Failure>>>>>>;

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

/// The daemon closed the connection, or it was already closed when a request was sent.
#[derive(Debug)]
pub struct ConnectionClosed;

impl std::fmt::Display for ConnectionClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("daemon closed the connection")
    }
}

impl std::error::Error for ConnectionClosed {}

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
        check_socket_path_length(paths)?;
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
            .ok_or(ConnectionClosed)?
            .insert(id, tx);
        write_message(&mut self.write, &ClientMessage::Request { id, request }).await?;
        Ok(Reply(rx))
    }

    pub async fn next_event(&mut self) -> Option<ServerMessage> {
        self.events.recv().await
    }
}

pub struct Reply(oneshot::Receiver<Result<Response, Failure>>);

impl Reply {
    pub async fn wait(self) -> anyhow::Result<Response> {
        match self.0.await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(failure)) => Err(failure.into()),
            Err(_) => Err(ConnectionClosed.into()),
        }
    }
}

/// The longest socket path `bind` accepts, counting the terminating NUL as `sun_path`
/// does: 104 bytes on macOS and the BSDs, 108 on Linux.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SOCKET_PATH_LIMIT: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SOCKET_PATH_LIMIT: usize = 104;

fn check_socket_path_length(paths: &StatePaths) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let dir = std::path::absolute(paths.dir())
        .with_context(|| format!("resolving {}", paths.dir().display()))?;
    let socket = StatePaths::new(dir).socket();
    let length = socket.as_os_str().as_bytes().len();
    if length >= SOCKET_PATH_LIMIT {
        anyhow::bail!(
            "the state directory path is too long for a unix socket: {} is {length} bytes, \
             the limit on this platform is {}; point LAZYAGENTS_STATE_DIR at a shorter path",
            socket.display(),
            SOCKET_PATH_LIMIT - 1
        );
    }
    Ok(())
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

/// Who holds the daemon lock: `None` when nobody does, `Some(None)` while the holder
/// has not written a readable pid yet.
fn lock_holder(paths: &StatePaths) -> Option<Option<Holder>> {
    use std::io::Read;

    let mut file = std::fs::File::open(paths.lock_file()).ok()?;
    if file.try_lock().is_ok() {
        return None;
    }
    let mut holder = String::new();
    Some(
        file.read_to_string(&mut holder)
            .ok()
            .and_then(|_| lockfile::parse(&holder)),
    )
}

/// The pid the running daemon recorded in its lock file; `None` unless another process
/// holds the lock and the recorded start time, if any, is still that pid's. A daemon
/// writes its pid just after taking the lock, so an empty, unreadable or outdated entry
/// is read again for up to `PID_WAIT`.
pub fn daemon_pid(paths: &StatePaths) -> Option<u32> {
    let deadline = Instant::now() + PID_WAIT;
    loop {
        match lock_holder(paths)? {
            Some(holder) if lockfile::is_current(&holder) => return Some(holder.pid),
            _ if Instant::now() >= deadline => return None,
            _ => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

const PID_WAIT: Duration = Duration::from_millis(500);

/// SIGTERMs the daemon holding the lock, which stops its agents before it exits, and waits
/// until it has released the lock. Never escalates to SIGKILL: that would orphan the
/// agents' process trees. A daemon that answers on the socket without holding the lock
/// cannot be identified, so it is an error rather than something stopped.
pub fn stop_daemon(paths: &StatePaths, timeout: Duration) -> anyhow::Result<()> {
    use rustix::process::{Pid, Signal, kill_process};

    let lock = paths.lock_file();
    let no_longer_the_daemon = |pid: u32| {
        anyhow::anyhow!(
            "{} names pid {pid}, which is no longer the daemon; try again",
            lock.display()
        )
    };
    let Some(pid) = daemon_pid(paths) else {
        return match lock_holder(paths) {
            None => unlocked_daemon(paths),
            Some(Some(holder)) => Err(no_longer_the_daemon(holder.pid)),
            Some(None) => anyhow::bail!(
                "{} is locked but names no daemon pid; stop the daemon by hand",
                lock.display()
            ),
        };
    };
    let target = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| anyhow::anyhow!("daemon.lock names an invalid pid {pid}"))?;
    match lock_holder(paths) {
        Some(Some(holder)) if holder.pid == pid && lockfile::is_current(&holder) => {}
        None => return unlocked_daemon(paths),
        Some(_) => return Err(no_longer_the_daemon(pid)),
    }
    if let Err(e) = kill_process(target, Signal::TERM)
        && e != rustix::io::Errno::SRCH
    {
        return Err(anyhow::Error::new(e).context(format!("signalling daemon {pid}")));
    }
    let deadline = Instant::now() + timeout;
    while lock_holder(paths).is_some() {
        if Instant::now() >= deadline {
            anyhow::bail!(
                "daemon {pid} did not stop within {}s",
                timeout.as_secs_f32()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

/// Nothing holds the lock: fine unless something still answers on the socket.
fn unlocked_daemon(paths: &StatePaths) -> anyhow::Result<()> {
    let socket = paths.socket();
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        anyhow::bail!(
            "a daemon answers on {} but holds no lock on {}, so its pid is unknown; \
             stop it by hand",
            socket.display(),
            paths.lock_file().display()
        );
    }
    Ok(())
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
