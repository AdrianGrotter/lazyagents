use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use protocol::{
    AgentId, AgentInfo, AgentKind, ClientMessage, CreateAgent, DiffScope, HookKind, InputEvent,
    Key, KeyCode, MAX_MESSAGE_LEN, Modifiers, PROTOCOL_VERSION, Project, ProjectId, Request,
    Response, Screen, ServerMessage, StatePaths, Status, Usage, WorktreeInfo, WorktreeRequest,
    read_message, write_message,
};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::claude_settings;
use crate::encode::encode;
use crate::lock::InstanceLock;
use crate::reap::{self, ProcessTable, Tracked};
use crate::spawn::{Launch, SpawnContext, blocked_envrc_hint, hook_settings, spawn_spec};
use crate::status::{awaits_trust, on_hook, settle, trust_keys};
use crate::store::{self, AgentRecord, StoredState};
use crate::terminal::{Outcome, PaneSize, Proxy, Terminal, TerminalEvent, session_title};
use crate::{direnv, git};

pub struct Config {
    pub paths: StatePaths,
    pub exe: PathBuf,
    /// Exit after this long with no client connected and no agent running.
    pub idle_exit: Option<Duration>,
    /// The direnv binary that wraps agents in a directory with an `.envrc`, reverts a
    /// direnv-loaded environment the daemon inherited, and decides whether a new worktree's
    /// `.envrc` is allowed automatically.
    pub direnv: Option<PathBuf>,
    /// Claude Code's user config directory, whose `settings.json` may hold the user's status
    /// line; `None` reads no user settings.
    pub claude_config: Option<PathBuf>,
}

type ClientId = u64;

enum Command {
    Request {
        client: ClientId,
        id: u64,
        request: Request,
    },
    Disconnected {
        client: ClientId,
    },
}

#[derive(PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

enum Reply {
    Now(Box<Result<Response, String>>),
    /// Answered later, from another thread through the client's sender or from the event
    /// loop once a job finishes.
    Deferred,
}

fn now(result: Result<Response, String>) -> Reply {
    Reply::Now(Box::new(result))
}

/// The rest of a request, run on the event loop once its slow part finished on a thread.
type Finish = Box<dyn FnOnce(&mut Daemon) + Send>;

#[derive(Clone, Copy)]
enum GitQuery {
    Stat,
    Diff(DiffScope),
}

const TICK: Duration = Duration::from_millis(33);
const STOP_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const HOUSEKEEPING: Duration = Duration::from_secs(1);
const SNAPSHOT: Duration = Duration::from_secs(1);
const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
const SHUTDOWN_SNAPSHOT_WAIT: Duration = Duration::from_secs(10);
const WHEEL_LINES: i32 = 3;

pub async fn run(config: Config) -> anyhow::Result<()> {
    let paths = config.paths;
    paths
        .create_dir()
        .with_context(|| format!("creating {}", paths.dir().display()))?;
    let _lock = InstanceLock::acquire(&paths.lock_file())?;
    let mut terminate = signal(SignalKind::terminate()).context("handling SIGTERM")?;
    let mut interrupt = signal(SignalKind::interrupt()).context("handling SIGINT")?;
    let socket = paths.socket();
    if socket.exists() {
        if UnixStream::connect(&socket).await.is_ok() {
            anyhow::bail!("a daemon is already listening on {}", socket.display());
        }
        std::fs::remove_file(&socket)
            .with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let state = store::load(&paths.state_file())?;
    let listener =
        UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    let bound = socket_identity(&socket)
        .with_context(|| format!("{} vanished after binding", socket.display()))?;
    tracing::info!(socket = %socket.display(), "daemon listening");

    let (incoming_tx, mut incoming) = mpsc::unbounded_channel::<UnixStream>();
    let (commands_tx, mut commands) = mpsc::unbounded_channel::<Command>();
    let (events_tx, mut events) = mpsc::unbounded_channel::<TerminalEvent>();
    let accept = tokio::spawn(accept_loop(listener, incoming_tx));
    let mut readers = JoinSet::new();
    let mut writers = JoinSet::new();
    let mut daemon = Daemon::new(paths.clone(), config.exe, config.direnv, state, events_tx);
    daemon.claude_config = config.claude_config;
    daemon.revert_direnv = daemon.direnv.as_deref().is_some_and(inherits_direnv_env);
    let mut next_client: ClientId = 1;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut housekeeping = tokio::time::interval(HOUSEKEEPING);
    housekeeping.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut busy_at = Instant::now();

    loop {
        tokio::select! {
            Some(stream) = incoming.recv() => {
                busy_at = Instant::now();
                let client = next_client;
                next_client += 1;
                let (tx, rx) = mpsc::unbounded_channel();
                daemon.add_client(client, tx);
                let (read, write) = stream.into_split();
                readers.spawn(read_loop(client, read, commands_tx.clone()));
                writers.spawn(write_loop(write, rx));
            }
            Some(command) = commands.recv() => {
                if daemon.handle_command(command) == Flow::Stop {
                    break;
                }
            }
            Some(event) = events.recv() => daemon.handle_terminal_event(event),
            Some(_) = readers.join_next(), if !readers.is_empty() => {}
            Some(_) = writers.join_next(), if !writers.is_empty() => {}
            _ = tick.tick() => daemon.tick(),
            _ = terminate.recv() => {
                tracing::info!("SIGTERM; shutting down");
                break;
            }
            _ = interrupt.recv() => {
                tracing::info!("SIGINT; shutting down");
                break;
            }
            _ = housekeeping.tick() => {
                if socket_identity(&socket).ok() != Some(bound) {
                    tracing::warn!(socket = %socket.display(), "socket removed or replaced; shutting down");
                    break;
                }
                if daemon.busy() {
                    busy_at = Instant::now();
                } else if config.idle_exit.is_some_and(|limit| busy_at.elapsed() >= limit) {
                    tracing::info!("idle; shutting down");
                    break;
                }
            }
        }
    }

    accept.abort();
    daemon.stop_all();
    drop(daemon);
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        while writers.join_next().await.is_some() {}
    })
    .await;
    readers.abort_all();
    if socket_identity(&socket).ok() == Some(bound) {
        let _ = std::fs::remove_file(&socket);
    }
    tracing::info!("daemon stopped");
    Ok(())
}

fn socket_identity(socket: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(socket)?;
    Ok((metadata.dev(), metadata.ino()))
}

async fn accept_loop(listener: UnixListener, incoming: UnboundedSender<UnixStream>) {
    while !incoming.is_closed() {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(greet(stream, incoming.clone()));
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
    }
}

/// Sends the Welcome before the event loop takes the connection, so a client is greeted at
/// once however busy the loop is.
async fn greet(mut stream: UnixStream, incoming: UnboundedSender<UnixStream>) {
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if write_message(&mut stream, &welcome).await.is_ok() {
        let _ = incoming.send(stream);
    }
}

async fn read_loop(client: ClientId, mut read: OwnedReadHalf, commands: UnboundedSender<Command>) {
    loop {
        match read_message::<_, ClientMessage>(&mut read).await {
            Ok(Some(ClientMessage::Request { id, request })) => {
                if commands
                    .send(Command::Request {
                        client,
                        id,
                        request,
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(Some(ClientMessage::Hello { protocol_version }))
                if protocol_version == PROTOCOL_VERSION => {}
            Ok(Some(ClientMessage::Hello { protocol_version })) => {
                tracing::warn!(
                    client,
                    protocol_version,
                    "client speaks another protocol; closing"
                );
                break;
            }
            Ok(None) | Err(_) => break,
        }
    }
    let _ = commands.send(Command::Disconnected { client });
}

async fn write_loop(mut write: OwnedWriteHalf, mut outgoing: UnboundedReceiver<ServerMessage>) {
    while let Some(message) = outgoing.recv().await {
        if write_message(&mut write, &message).await.is_err() {
            break;
        }
    }
}

struct Agent {
    record: AgentRecord,
    status: Status,
    last_error: Option<String>,
    terminal: Option<Terminal>,
    generation: u64,
    started: Instant,
    status_since: Instant,
    last_output: Option<Instant>,
    dirty: bool,
    /// The agent's descendants seen by snapshots and still running, so processes that
    /// detach from its tree can still be stopped once it exits or is killed.
    tracked: Vec<Tracked>,
    /// The process group the agent leads; kept after it exits while members remain.
    group: Option<u32>,
    /// The create request's prompt until it is delivered: a Claude agent gets it as an
    /// argument when it starts, a generic one has it typed once it first prints.
    pending_prompt: Option<String>,
    /// The create request's setup script until the first start runs it.
    pending_setup: Option<String>,
    /// What the agent waits for while it needs input; see `AgentInfo::waiting_for`.
    waiting_for: Option<String>,
    /// Set when a starting Claude agent shows the trust dialog, until it leaves `NeedsInput`
    /// or reports a hook.
    at_trust_dialog: bool,
    usage: Option<Usage>,
    /// What a stop or removal running off the event loop is doing to the agent; while set,
    /// kill, restart, remove, merge and rename are refused.
    busy: Option<&'static str>,
}

impl Agent {
    fn new(record: AgentRecord) -> Self {
        Self {
            record,
            status: Status::Stopped,
            last_error: None,
            terminal: None,
            generation: 0,
            started: Instant::now(),
            status_since: Instant::now(),
            last_output: None,
            dirty: false,
            tracked: Vec::new(),
            group: None,
            pending_prompt: None,
            pending_setup: None,
            waiting_for: None,
            at_trust_dialog: false,
            usage: None,
            busy: None,
        }
    }

    fn set_status(&mut self, status: Status) {
        if status != self.status {
            self.status = status;
            self.status_since = Instant::now();
        }
        if status != Status::NeedsInput {
            self.waiting_for = None;
            self.at_trust_dialog = false;
        }
    }

    /// A hook counts as activity even when it leaves the status unchanged. A
    /// `PermissionRequest` names the tool call, which beats the generic message of the
    /// `Notification` that follows it.
    fn hooked(&mut self, kind: HookKind, detail: Option<String>, now: Instant) {
        self.status = on_hook(kind);
        self.status_since = now;
        self.at_trust_dialog = false;
        self.waiting_for = match (self.status, kind) {
            (Status::NeedsInput, HookKind::PermissionRequest) => detail,
            (Status::NeedsInput, _) => self.waiting_for.take().or(detail),
            _ => None,
        };
    }
}

/// Tracked descendants computed off the event loop for one agent generation.
type Snapshot = Vec<(AgentId, u64, Vec<Tracked>)>;

/// A snapshot running on its own thread, and the agents it reads.
struct PendingSnapshot {
    rx: std::sync::mpsc::Receiver<Snapshot>,
    agents: Vec<AgentId>,
    /// Stops running off the event loop that wait for what the snapshot finds for these agents.
    waiters: Vec<(Vec<AgentId>, Sender<Vec<Tracked>>)>,
}

/// A reap running on its own thread. Its processes stay visible here until the thread
/// finishes, so a stop of the same agent, or shutdown, covers them too.
struct Reaper {
    agent: AgentId,
    groups: Arc<[u32]>,
    tracked: Arc<[Tracked]>,
    thread: JoinHandle<Vec<Tracked>>,
}

struct Subscription {
    agent: AgentId,
    last: Option<Screen>,
}

struct Client {
    tx: UnboundedSender<ServerMessage>,
    subscription: Option<Subscription>,
}

struct Daemon {
    paths: StatePaths,
    exe: PathBuf,
    direnv: Option<PathBuf>,
    projects: Vec<Project>,
    agents: HashMap<AgentId, Agent>,
    clients: HashMap<ClientId, Client>,
    events: UnboundedSender<TerminalEvent>,
    last_snapshot: Instant,
    snapshot: Option<PendingSnapshot>,
    reapers: Vec<Reaper>,
    leftovers: Vec<(AgentId, Tracked)>,
    leftovers_retried: Option<Instant>,
    claude_config: Option<PathBuf>,
    revert_direnv: bool,
    /// Requests whose slow part runs on its own thread; `tick` finishes them.
    jobs: Vec<JoinHandle<Option<Finish>>>,
    /// Names reserved by worktree agents being created.
    creating: Vec<(ProjectId, String)>,
    shutting_down: bool,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop_all();
    }
}

impl Daemon {
    fn new(
        paths: StatePaths,
        exe: PathBuf,
        direnv: Option<PathBuf>,
        state: StoredState,
        events: UnboundedSender<TerminalEvent>,
    ) -> Self {
        let agents = state
            .agents
            .into_iter()
            .map(|record| (record.id, Agent::new(record)))
            .collect();
        Self {
            paths,
            exe,
            direnv,
            projects: state.projects,
            agents,
            clients: HashMap::new(),
            events,
            last_snapshot: Instant::now(),
            snapshot: None,
            reapers: Vec::new(),
            leftovers: Vec::new(),
            leftovers_retried: None,
            claude_config: None,
            revert_direnv: false,
            jobs: Vec::new(),
            creating: Vec::new(),
            shutting_down: false,
        }
    }

    fn add_client(&mut self, client: ClientId, tx: UnboundedSender<ServerMessage>) {
        self.clients.insert(
            client,
            Client {
                tx,
                subscription: None,
            },
        );
    }

    fn busy(&self) -> bool {
        !self.clients.is_empty()
            || self
                .agents
                .values()
                .any(|agent| agent.terminal.is_some() && agent.status.is_running())
            || !self.jobs.is_empty()
    }

    fn handle_command(&mut self, command: Command) -> Flow {
        match command {
            Command::Request {
                client,
                id,
                request,
            } => {
                let stop = matches!(request, Request::Shutdown);
                if let Reply::Now(result) = self.handle_request(client, id, request) {
                    self.reply(client, id, *result);
                }
                if stop { Flow::Stop } else { Flow::Continue }
            }
            Command::Disconnected { client } => {
                self.clients.remove(&client);
                Flow::Continue
            }
        }
    }

    fn handle_request(&mut self, client: ClientId, request_id: u64, request: Request) -> Reply {
        let result = match request {
            Request::ListProjects => Ok(Response::Projects(self.projects.clone())),
            Request::AddProject { path, name } => {
                self.add_project(&path, name).map(Response::Project)
            }
            Request::RemoveProject { id } => self.remove_project(id).map(|()| Response::Ok),
            Request::ListAgents => Ok(Response::Agents(self.list_agents())),
            Request::CreateAgent(create) => return self.create_agent(client, request_id, create),
            Request::KillAgent { id } => return self.kill_agent(client, request_id, id),
            Request::RestartAgent { id, cols, rows } => {
                return self.restart_agent(client, request_id, id, PaneSize::new(cols, rows));
            }
            Request::RemoveAgent {
                id,
                remove_worktree,
                force,
                delete_branch,
            } => {
                let removal = Removal {
                    worktree: remove_worktree,
                    force,
                    delete_branch,
                };
                return self.remove_agent(client, request_id, id, removal);
            }
            Request::MergeAgent { id } => return self.merge_agent(client, request_id, id),
            Request::GitStat { id } => {
                return self.git_query(client, request_id, id, GitQuery::Stat);
            }
            Request::GitDiff { id, scope } => {
                return self.git_query(client, request_id, id, GitQuery::Diff(scope));
            }
            Request::RenameAgent { id, name } => self.rename_agent(id, name).map(Response::Agent),
            Request::Subscribe { id } => self.subscribe(client, id).map(|()| Response::Ok),
            Request::Unsubscribe => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.subscription = None;
                }
                Ok(Response::Ok)
            }
            Request::Input { id, event } => self.input(id, &event).map(|()| Response::Ok),
            Request::Approve { id } => self.approve(id).map(|()| Response::Ok),
            Request::Resize { id, cols, rows } => self
                .resize(id, PaneSize::new(cols, rows))
                .map(|()| Response::Ok),
            Request::Scroll { id, lines } => self.scroll(id, lines).map(|()| Response::Ok),
            Request::Hook {
                id,
                kind,
                session_id,
                source,
                detail,
            } => self
                .hook(id, kind, session_id, source.as_deref(), detail)
                .map(|()| Response::Ok),
            Request::Usage { id, usage } => self.usage(id, usage).map(|()| Response::Ok),
            Request::Shutdown => {
                self.stop_all();
                Ok(Response::Ok)
            }
        };
        now(result)
    }

    fn reply(&self, client: ClientId, id: u64, result: Result<Response, String>) {
        if let Some(c) = self.clients.get(&client) {
            let _ = c.tx.send(ServerMessage::Response { id, result });
        }
    }

    fn subscribe(&mut self, client: ClientId, id: AgentId) -> Result<(), String> {
        self.agent(id)?;
        if let Some(c) = self.clients.get_mut(&client) {
            c.subscription = Some(Subscription {
                agent: id,
                last: None,
            });
        }
        Ok(())
    }

    fn resize(&mut self, id: AgentId, size: PaneSize) -> Result<(), String> {
        let agent = self.with_terminal_mut(id)?;
        if let Some(t) = agent.terminal.as_mut() {
            t.resize(size);
        }
        agent.dirty = true;
        Ok(())
    }

    fn scroll(&mut self, id: AgentId, lines: i32) -> Result<(), String> {
        let agent = self.with_terminal_mut(id)?;
        if let Some(t) = agent.terminal.as_ref() {
            t.scroll(lines);
        }
        agent.dirty = true;
        Ok(())
    }

    fn agent(&self, id: AgentId) -> Result<&Agent, String> {
        self.agents
            .get(&id)
            .ok_or_else(|| format!("unknown agent {id}"))
    }

    fn running_mut(&mut self, id: AgentId) -> Result<&mut Agent, String> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        if agent.terminal.is_none() || !agent.status.is_running() {
            return Err(format!("{} is not running", agent.record.name));
        }
        Ok(agent)
    }

    fn with_terminal_mut(&mut self, id: AgentId) -> Result<&mut Agent, String> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        if agent.terminal.is_none() {
            return Err(format!("{} has no terminal", agent.record.name));
        }
        Ok(agent)
    }

    fn info(agent: &Agent) -> AgentInfo {
        AgentInfo {
            id: agent.record.id,
            project: agent.record.project,
            name: agent.record.name.clone(),
            kind: agent.record.kind,
            command: agent.record.command.clone(),
            cwd: agent.record.cwd.clone(),
            status: agent.status,
            worktree: agent.record.worktree.clone().map(Box::new),
            last_error: agent.last_error.clone(),
            title: agent.record.title.clone(),
            waiting_for: agent.waiting_for.clone(),
            usage: agent.usage.clone().map(Box::new),
        }
    }

    fn info_of(&self, id: AgentId) -> Result<AgentInfo, String> {
        self.agent(id).map(Self::info)
    }

    fn list_agents(&self) -> Vec<AgentInfo> {
        let position = |project: ProjectId| self.projects.iter().position(|p| p.id == project);
        let mut infos: Vec<AgentInfo> = self.agents.values().map(Self::info).collect();
        infos.sort_by(|a, b| (position(a.project), &a.name).cmp(&(position(b.project), &b.name)));
        infos
    }

    fn broadcast(&self, message: &ServerMessage) {
        for client in self.clients.values() {
            let _ = client.tx.send(message.clone());
        }
    }

    fn announce(&self, id: AgentId) {
        if let Ok(info) = self.info_of(id) {
            self.broadcast(&ServerMessage::AgentUpdated(info));
        }
    }

    fn persist(&self) {
        let mut agents: Vec<AgentRecord> = self.agents.values().map(|a| a.record.clone()).collect();
        agents.sort_by(|a, b| (a.project, &a.name).cmp(&(b.project, &b.name)));
        let state = StoredState {
            projects: self.projects.clone(),
            agents,
        };
        if let Err(e) = store::save(&self.paths.state_file(), &state) {
            tracing::error!(error = %format!("{e:#}"), "persisting state failed");
        }
    }

    fn add_project(&mut self, path: &Path, name: Option<String>) -> Result<Project, String> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !path.is_dir() {
            return Err(format!("{} is not a directory", path.display()));
        }
        if let Some(existing) = self.projects.iter().find(|p| p.path == path) {
            return Ok(existing.clone());
        }
        let name = name
            .filter(|n| !n.trim().is_empty())
            .or_else(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| path.display().to_string());
        let project = Project {
            id: ProjectId::random(),
            name,
            path,
        };
        self.projects.push(project.clone());
        self.persist();
        self.broadcast(&ServerMessage::ProjectsChanged(self.projects.clone()));
        Ok(project)
    }

    fn remove_project(&mut self, id: ProjectId) -> Result<(), String> {
        if self.agents.values().any(|a| a.record.project == id)
            || self.creating.iter().any(|(project, _)| *project == id)
        {
            return Err("remove the project's agents first".into());
        }
        let before = self.projects.len();
        self.projects.retain(|p| p.id != id);
        if self.projects.len() == before {
            return Err(format!("unknown project {id}"));
        }
        self.persist();
        self.broadcast(&ServerMessage::ProjectsChanged(self.projects.clone()));
        Ok(())
    }

    /// Names that differ only in case count as the same: they name worktree directories and
    /// branches, which a case-insensitive file system cannot tell apart.
    fn validate_name(
        &self,
        project: ProjectId,
        name: &str,
        except: Option<AgentId>,
    ) -> Result<(), String> {
        let valid = !name.is_empty()
            && name.len() <= 64
            && !name.starts_with(['-', '.'])
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !valid {
            return Err(format!(
                "invalid agent name {name:?}: use 1-64 of A-Z a-z 0-9 . _ -, not starting with - or ."
            ));
        }
        let taken = self.agents.values().any(|a| {
            a.record.project == project
                && a.record.name.eq_ignore_ascii_case(name)
                && Some(a.record.id) != except
        });
        if taken {
            return Err(format!(
                "an agent named {name} already exists in this project"
            ));
        }
        if self.creating.iter().any(|(reserved, reserved_name)| {
            *reserved == project && reserved_name.eq_ignore_ascii_case(name)
        }) {
            return Err(format!(
                "an agent named {name} is being created in this project"
            ));
        }
        Ok(())
    }

    /// Creates an agent in its project's checkout at once; a worktree agent's worktree is
    /// made off the event loop, with its name reserved meanwhile.
    fn create_agent(&mut self, client: ClientId, request_id: u64, create: CreateAgent) -> Reply {
        let Some(project) = self
            .projects
            .iter()
            .find(|p| p.id == create.project)
            .cloned()
        else {
            return now(Err(format!("unknown project {}", create.project)));
        };
        if let Err(e) = self.validate_name(project.id, &create.name, None) {
            return now(Err(e));
        }
        if create.kind == AgentKind::Generic && create.command.is_empty() {
            return now(Err("a generic agent needs a command".into()));
        }
        let Some(request) = create.worktree.clone() else {
            return now(self
                .finish_create(&project, create, None)
                .map(Response::Agent));
        };
        let reservation = (project.id, create.name.clone());
        self.creating.push(reservation.clone());
        let state = self.paths.dir().to_path_buf();
        let direnv = self.direnv.clone();
        let (job_project, name) = (project.clone(), create.name.clone());
        self.off_loop(
            "worktree",
            move || prepare_worktree(&state, &job_project, &name, request, direnv.as_deref()),
            move |daemon, prepared| {
                daemon.creating.retain(|reserved| *reserved != reservation);
                let result = match prepared {
                    None => Err(format!("creating the worktree for {} failed", create.name)),
                    Some(Err(e)) => Err(e),
                    Some(Ok(worktree)) if daemon.shutting_down => {
                        git::discard_worktree(&project.path, &worktree.path, &worktree.branch);
                        Err(format!(
                            "the daemon shut down before {} started; its worktree was removed",
                            create.name
                        ))
                    }
                    Some(Ok(worktree)) => daemon.finish_create(&project, create, Some(worktree)),
                };
                daemon.reply(client, request_id, result.map(Response::Agent));
            },
        );
        Reply::Deferred
    }

    fn finish_create(
        &mut self,
        project: &Project,
        create: CreateAgent,
        worktree: Option<WorktreeInfo>,
    ) -> Result<AgentInfo, String> {
        let record = AgentRecord {
            id: AgentId::random(),
            project: project.id,
            name: create.name,
            kind: create.kind,
            command: create.command,
            cwd: worktree
                .as_ref()
                .map_or_else(|| project.path.clone(), |w| w.path.clone()),
            session_id: None,
            resumable: false,
            worktree,
            title: None,
        };
        let id = record.id;
        let mut agent = Agent::new(record);
        agent.pending_prompt = create.prompt.filter(|p| !p.trim().is_empty());
        agent.pending_setup = create.setup.filter(|s| !s.trim().is_empty());
        self.agents.insert(id, agent);
        let started = self.spawn_agent(id, Launch::Fresh, PaneSize::new(create.cols, create.rows));
        self.persist();
        self.announce(id);
        started?;
        self.info_of(id)
    }

    /// Starts the agent's process; whatever ran before must be stopped already.
    fn spawn_agent(&mut self, id: AgentId, launch: Launch, size: PaneSize) -> Result<(), String> {
        let project_root = {
            let agent = self.agent(id)?;
            self.projects
                .iter()
                .find(|p| p.id == agent.record.project)
                .map(|p| p.path.clone())
                .unwrap_or_else(|| agent.record.cwd.clone())
        };
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        let resume = launch == Launch::Resume && agent.record.resumable;
        if agent.record.kind == AgentKind::Claude {
            if !resume {
                agent.record.session_id = Some(Uuid::new_v4());
                agent.record.resumable = false;
                agent.record.title = None;
            }
            let settings = self.paths.hook_settings(id);
            let theirs =
                claude_settings::status_line(&agent.record.cwd, self.claude_config.as_deref());
            let generated = hook_settings(&self.exe, theirs.as_ref()).to_string();
            let written = std::fs::create_dir_all(self.paths.hooks_dir())
                .and_then(|()| std::fs::write(&settings, generated));
            if let Err(e) = written {
                agent.set_status(Status::Exited(127));
                let message = format!("writing {}: {e}", settings.display());
                agent.last_error = Some(message.clone());
                return Err(message);
            }
        }
        let setup = agent.pending_setup.take();
        let claude_prompt = match agent.record.kind {
            AgentKind::Claude => agent.pending_prompt.take(),
            AgentKind::Generic => None,
        };
        let ctx = SpawnContext {
            paths: &self.paths,
            project_root: &project_root,
            direnv: self.direnv.as_deref(),
            revert_direnv: self.revert_direnv,
            prompt: claude_prompt.as_deref(),
            setup: setup.as_deref(),
        };
        let spec = spawn_spec(
            &agent.record,
            if resume {
                Launch::Resume
            } else {
                Launch::Fresh
            },
            &ctx,
        );
        match Terminal::spawn(
            &spec,
            size,
            Proxy::new(id, agent.generation, self.events.clone()),
        ) {
            Ok(terminal) => {
                agent.group = Some(terminal.pid());
                agent.terminal = Some(terminal);
                agent.set_status(Status::Starting);
                agent.last_error = None;
                agent.started = Instant::now();
                agent.status_since = agent.started;
                agent.last_output = None;
                agent.dirty = true;
                Ok(())
            }
            Err(e) => {
                agent.set_status(Status::Exited(127));
                let message = format!("{e:#}");
                agent.last_error = Some(message.clone());
                Err(message)
            }
        }
    }

    /// Runs `work` on its own thread, then `then` with its result on the event loop, from
    /// `tick`; `then` gets `None` when `work` panicked. Both run inline when no thread can
    /// be spawned.
    fn off_loop<T: Send + 'static>(
        &mut self,
        name: &str,
        work: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&mut Daemon, Option<T>) + Send + 'static,
    ) {
        let job = Arc::new(Mutex::new(Some((work, then))));
        let theirs = Arc::clone(&job);
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let (work, then) = theirs.lock().ok()?.take()?;
                let result = catch_unwind(AssertUnwindSafe(work)).ok();
                let finish: Finish = Box::new(move |daemon: &mut Daemon| then(daemon, result));
                Some(finish)
            });
        match spawned {
            Ok(thread) => self.jobs.push(thread),
            Err(e) => {
                tracing::error!(error = %e, "spawning a {name} thread failed; running it inline");
                let taken = job.lock().ok().and_then(|mut job| job.take());
                if let Some((work, then)) = taken {
                    let result = catch_unwind(AssertUnwindSafe(work)).ok();
                    then(self, result);
                }
            }
        }
    }

    fn finish_job(&mut self, job: JoinHandle<Option<Finish>>) {
        match job.join() {
            Ok(Some(finish)) => finish(self),
            _ => tracing::error!("a request's thread ended without a result"),
        }
    }

    fn finish_jobs(&mut self) {
        let (finished, running) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition::<Vec<_>, _>(|job| job.is_finished());
        self.jobs.extend(running);
        for job in finished {
            self.finish_job(job);
        }
    }

    /// Waits for every job, including those a finishing job starts; true when there was any.
    fn drain_jobs(&mut self) -> bool {
        let mut drained = false;
        while !self.jobs.is_empty() {
            drained = true;
            for job in std::mem::take(&mut self.jobs) {
                self.finish_job(job);
            }
        }
        drained
    }

    /// The processes a stop of `ids` ends: their trees, process groups and tracked
    /// descendants, with those of their running reapers and their leftovers.
    fn capture(&mut self, ids: &[AgentId]) -> (Vec<u32>, Vec<u32>, Vec<Tracked>) {
        let mut trees = Vec::new();
        let mut groups = Vec::new();
        let mut tracked = Vec::new();
        for id in ids {
            if let Some(agent) = self.agents.get_mut(id) {
                if agent.status.is_running()
                    && let Some(terminal) = agent.terminal.as_ref()
                {
                    trees.push(terminal.pid());
                }
                groups.extend(agent.group);
                tracked.append(&mut agent.tracked);
            }
        }
        for reaper in self.reapers.iter().filter(|r| ids.contains(&r.agent)) {
            groups.extend(reaper.groups.iter().copied());
            tracked.extend(reaper.tracked.iter().cloned());
        }
        tracked.extend(
            self.leftovers
                .extract_if(.., |(agent, _)| ids.contains(agent))
                .map(|(_, tracked)| tracked),
        );
        (trees, groups, tracked)
    }

    /// Records a finished stop of `ids`: keeps what it could not verify and the groups that
    /// still have members, and drops the terminals. What a snapshot found for them while
    /// the stop ran is reaped too.
    fn stopped(&mut self, ids: &[AgentId], unverified: Vec<Tracked>) {
        if let Some(&first) = ids.first() {
            self.keep_leftovers(first, unverified);
        }
        for id in ids {
            let Some(agent) = self.agents.get_mut(id) else {
                continue;
            };
            agent.group = agent.group.filter(|&leader| reap::group_exists(leader));
            if agent.terminal.take().is_some() {
                agent.generation += 1;
            }
            let late = std::mem::take(&mut agent.tracked);
            self.reap_off_loop(*id, Vec::new(), late);
        }
    }

    /// Terminates the given agents' process trees, process groups and tracked
    /// descendants on the event loop, then drops their terminals; only shutdown, which
    /// ends the loop, stops this way. A snapshot still running is waited for only when it
    /// covers an agent being stopped, so what it found is stopped too; its results for
    /// other agents are applied, or reaped as orphans, when they arrive.
    fn stop(&mut self, ids: &[AgentId], grace: Duration) {
        self.finish_snapshot(Some(ids));
        let (trees, groups, tracked) = self.capture(ids);
        let unverified = reap::terminate(&trees, &groups, &tracked, grace);
        self.stopped(ids, unverified);
    }

    /// Stops the agent off the event loop, as `stop` does, then runs `after` on the same
    /// thread and `then` with its result on the loop. Until then the agent is busy `doing`.
    fn begin_stop<T: Send + 'static>(
        &mut self,
        id: AgentId,
        doing: &'static str,
        after: impl FnOnce() -> T + Send + 'static,
        then: impl FnOnce(&mut Daemon, Option<T>) + Send + 'static,
    ) {
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.busy = Some(doing);
        }
        let snapshot = self.snapshot_waiter(&[id]);
        let (trees, groups, mut tracked) = self.capture(&[id]);
        let handed = tracked.clone();
        self.off_loop(
            "stop",
            move || {
                if let Some(snapshot) = snapshot {
                    tracked.extend(await_snapshot(&snapshot));
                }
                let unverified = reap::terminate(&trees, &groups, &tracked, STOP_GRACE);
                (unverified, after())
            },
            move |daemon, done| {
                let (unverified, value) = match done {
                    Some((unverified, value)) => (unverified, Some(value)),
                    None => {
                        tracing::error!(
                            agent = %id,
                            "stopping the agent panicked; its tracked processes are kept to retry"
                        );
                        (handed, None)
                    }
                };
                daemon.stopped(&[id], unverified);
                if let Some(agent) = daemon.agents.get_mut(&id) {
                    agent.busy = None;
                }
                then(daemon, value);
            },
        );
    }

    /// Stops every agent and finishes every job, without starting anything new; the event
    /// loop ends after it.
    fn stop_all(&mut self) {
        self.shutting_down = true;
        self.finish_snapshot(None);
        let ids = self.stoppable();
        self.stop(&ids, SHUTDOWN_GRACE);
        if self.drain_jobs() {
            let ids = self.stoppable();
            self.stop(&ids, SHUTDOWN_GRACE);
        }
        if !self.leftovers.is_empty() {
            let pids: Vec<(u32, &str)> = self
                .leftovers
                .iter()
                .map(|(_, tracked)| (tracked.pid, tracked.start.as_str()))
                .collect();
            tracing::error!(
                ?pids,
                "could not verify these processes before exiting; they were not signalled"
            );
        }
    }

    fn stoppable(&self) -> Vec<AgentId> {
        let mut ids: Vec<AgentId> = self.agents.keys().copied().collect();
        ids.extend(self.reapers.iter().map(|r| r.agent));
        ids.extend(self.leftovers.iter().map(|(agent, _)| *agent));
        ids
    }

    /// Records processes a stop could not verify, so they are retried and never
    /// forgotten; a process already recorded is not recorded twice.
    fn keep_leftovers(&mut self, agent: AgentId, unverified: Vec<Tracked>) {
        for tracked in unverified {
            if !self.leftovers.iter().any(|(_, kept)| *kept == tracked) {
                self.leftovers.push((agent, tracked));
            }
        }
    }

    /// A receiver for what the running snapshot finds for `ids`, when it reads any of them.
    fn snapshot_waiter(&mut self, ids: &[AgentId]) -> Option<Receiver<Vec<Tracked>>> {
        let pending = self
            .snapshot
            .as_mut()
            .filter(|pending| pending.agents.iter().any(|agent| ids.contains(agent)))?;
        let (tx, rx) = std::sync::mpsc::channel();
        pending.waiters.push((ids.to_vec(), tx));
        Some(rx)
    }

    /// Hands the stops waiting for the snapshot what it found for their agents, then
    /// applies it.
    fn snapshot_finished(
        &mut self,
        waiters: Vec<(Vec<AgentId>, Sender<Vec<Tracked>>)>,
        results: Snapshot,
    ) {
        for (ids, tx) in waiters {
            let found = results
                .iter()
                .filter(|(id, _, _)| ids.contains(id))
                .flat_map(|(_, _, tracked)| tracked.iter().cloned())
                .collect();
            let _ = tx.send(found);
        }
        self.apply_snapshot(results);
    }

    /// Waits for the running snapshot and applies it, unless `covering` names only
    /// agents the snapshot does not read; `None` waits whatever it covers. A snapshot
    /// slower than a stop's wait stays pending, so `refresh_tracked` applies or reaps
    /// its results later; at shutdown nothing comes later, so the wait is longer.
    fn finish_snapshot(&mut self, covering: Option<&[AgentId]>) {
        let Some(pending) = self.snapshot.take_if(|pending| {
            covering.is_none_or(|ids| pending.agents.iter().any(|agent| ids.contains(agent)))
        }) else {
            return;
        };
        let wait = if covering.is_some() {
            SNAPSHOT_WAIT
        } else {
            SHUTDOWN_SNAPSHOT_WAIT
        };
        match pending.rx.recv_timeout(wait) {
            Ok(results) => self.snapshot_finished(pending.waiters, results),
            Err(RecvTimeoutError::Timeout) if covering.is_some() => {
                tracing::warn!(
                    "stopping before the running snapshot finished; its results are applied when it does"
                );
                self.snapshot = Some(pending);
            }
            Err(RecvTimeoutError::Timeout) => tracing::error!(
                "shutting down without the running snapshot; the processes it would report could not be checked"
            ),
            Err(RecvTimeoutError::Disconnected) => {
                tracing::warn!("the snapshot thread ended without a result")
            }
        }
    }

    /// The agent, unless a stop or removal of it is running.
    fn idle(&self, id: AgentId) -> Result<&Agent, String> {
        let agent = self.agent(id)?;
        match agent.busy {
            Some(doing) => Err(format!(
                "{} is {doing}; try again once that is done",
                agent.record.name
            )),
            None => Ok(agent),
        }
    }

    fn kill_agent(&mut self, client: ClientId, request_id: u64, id: AgentId) -> Reply {
        if let Err(e) = self.idle(id) {
            return now(Err(e));
        }
        self.begin_stop(
            id,
            "stopping",
            || (),
            move |daemon, _| {
                if let Some(agent) = daemon.agents.get_mut(&id) {
                    agent.set_status(Status::Stopped);
                    agent.dirty = true;
                }
                daemon.announce(id);
                let result = daemon.info_of(id).map(Response::Agent);
                daemon.reply(client, request_id, result);
            },
        );
        Reply::Deferred
    }

    fn restart_agent(
        &mut self,
        client: ClientId,
        request_id: u64,
        id: AgentId,
        size: PaneSize,
    ) -> Reply {
        if let Err(e) = self.idle(id) {
            return now(Err(e));
        }
        self.begin_stop(
            id,
            "restarting",
            || (),
            move |daemon, _| {
                let started = if daemon.shutting_down {
                    if let Some(agent) = daemon.agents.get_mut(&id) {
                        agent.set_status(Status::Stopped);
                        agent.dirty = true;
                    }
                    Err("the daemon is shutting down".to_string())
                } else {
                    daemon.spawn_agent(id, Launch::Resume, size)
                };
                daemon.persist();
                daemon.announce(id);
                let result = started.and_then(|()| daemon.info_of(id).map(Response::Agent));
                daemon.reply(client, request_id, result);
            },
        );
        Reply::Deferred
    }

    /// Checks off the event loop that the removal may go ahead, then stops the agent and
    /// removes its worktree and branch, also off the loop; the agent is busy throughout.
    fn remove_agent(
        &mut self,
        client: ClientId,
        request_id: u64,
        id: AgentId,
        removal: Removal,
    ) -> Reply {
        let agent = match self.idle(id) {
            Ok(agent) => agent,
            Err(e) => return now(Err(e)),
        };
        let branch = agent
            .record
            .worktree
            .as_ref()
            .filter(|_| removal.delete_branch)
            .map(|w| (w.branch.clone(), w.base.clone()));
        let worktree = match agent.record.worktree.as_ref().filter(|_| removal.worktree) {
            Some(worktree) => {
                let Some(project) = self
                    .projects
                    .iter()
                    .find(|p| p.id == agent.record.project)
                    .map(|p| p.path.clone())
                else {
                    return now(Err(format!("unknown project {}", agent.record.project)));
                };
                Some((project, worktree.path.clone()))
            }
            None => None,
        };
        if removal.delete_branch && (worktree.is_none() || branch.is_none()) {
            return now(Err(
                "only a worktree agent removed with its worktree can lose its branch".into(),
            ));
        }
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.busy = Some("being removed");
        }
        let (checked_worktree, checked_branch) = (worktree.clone(), branch.clone());
        self.off_loop(
            "git",
            move || removable(checked_worktree, checked_branch, removal.force),
            move |daemon, checked| match checked {
                Some(Ok(())) => {
                    daemon.finish_remove(client, request_id, id, worktree, branch, removal)
                }
                refused => {
                    if let Some(agent) = daemon.agents.get_mut(&id) {
                        agent.busy = None;
                    }
                    let message = match refused {
                        Some(Err(e)) => e,
                        _ => "checking whether the agent can be removed failed".into(),
                    };
                    daemon.reply(client, request_id, Err(message));
                }
            },
        );
        Reply::Deferred
    }

    fn finish_remove(
        &mut self,
        client: ClientId,
        request_id: u64,
        id: AgentId,
        worktree: Option<(PathBuf, PathBuf)>,
        branch: Option<(String, String)>,
        removal: Removal,
    ) {
        self.begin_stop(
            id,
            "being removed",
            move || remove_worktree_and_branch(worktree, branch, removal.force),
            move |daemon, removed| {
                let (worktree, branch) =
                    removed.unwrap_or_else(|| (Err("removing the worktree failed".into()), Ok(())));
                if let Err(e) = worktree {
                    if let Some(agent) = daemon.agents.get_mut(&id) {
                        agent.set_status(Status::Stopped);
                        agent.dirty = true;
                    }
                    daemon.announce(id);
                    daemon.reply(client, request_id, Err(e));
                    return;
                }
                daemon.agents.remove(&id);
                let _ = std::fs::remove_file(daemon.paths.hook_settings(id));
                for c in daemon.clients.values_mut() {
                    if c.subscription.as_ref().is_some_and(|s| s.agent == id) {
                        c.subscription = None;
                    }
                }
                daemon.persist();
                daemon.broadcast(&ServerMessage::AgentRemoved(id));
                daemon.reply(client, request_id, branch.map(|()| Response::Ok));
            },
        );
    }

    /// Merges on its own thread, like a git query, since the user's merge hooks may take
    /// their time.
    fn merge_agent(&self, client: ClientId, request_id: u64, id: AgentId) -> Reply {
        let agent = match self.idle(id) {
            Ok(agent) => agent,
            Err(e) => return Reply::Now(Box::new(Err(e))),
        };
        let Some(worktree) = agent.record.worktree.clone() else {
            let message = format!("{} has no worktree branch to merge", agent.record.name);
            return Reply::Now(Box::new(Err(message)));
        };
        let Some(project) = self
            .projects
            .iter()
            .find(|p| p.id == agent.record.project)
            .map(|p| p.path.clone())
        else {
            let message = format!("unknown project {}", agent.record.project);
            return Reply::Now(Box::new(Err(message)));
        };
        let Some(tx) = self.clients.get(&client).map(|c| c.tx.clone()) else {
            return Reply::Deferred;
        };
        let spawned = std::thread::Builder::new()
            .name("git-merge".into())
            .spawn(move || {
                let result = git::merge(&project, &worktree.path, &worktree.branch, &worktree.base)
                    .map(Response::Done)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(ServerMessage::Response {
                    id: request_id,
                    result,
                });
            });
        match spawned {
            Ok(_) => Reply::Deferred,
            Err(e) => Reply::Now(Box::new(Err(format!("spawning a git thread: {e}")))),
        }
    }

    /// Runs the query on its own thread and answers the client from there; an answer
    /// that arrives after the client left or the daemon stopped is dropped.
    fn git_query(&self, client: ClientId, request_id: u64, id: AgentId, query: GitQuery) -> Reply {
        let agent = match self.agent(id) {
            Ok(agent) => agent,
            Err(e) => return Reply::Now(Box::new(Err(e))),
        };
        let Some(tx) = self.clients.get(&client).map(|c| c.tx.clone()) else {
            return Reply::Deferred;
        };
        let dir = agent.record.cwd.clone();
        let base = agent.record.worktree.as_ref().map(|w| w.base.clone());
        let spawned = std::thread::Builder::new()
            .name("git".into())
            .spawn(move || {
                let result = run_git_query(&dir, base.as_deref(), query);
                let _ = tx.send(ServerMessage::Response {
                    id: request_id,
                    result,
                });
            });
        match spawned {
            Ok(_) => Reply::Deferred,
            Err(e) => Reply::Now(Box::new(Err(format!("spawning a git thread: {e}")))),
        }
    }

    fn rename_agent(&mut self, id: AgentId, name: String) -> Result<AgentInfo, String> {
        let project = self.idle(id)?.record.project;
        self.validate_name(project, &name, Some(id))?;
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.record.name = name;
        }
        self.persist();
        self.announce(id);
        self.info_of(id)
    }

    fn input(&mut self, id: AgentId, event: &InputEvent) -> Result<(), String> {
        let agent = self.running_mut(id)?;
        let Some(terminal) = agent.terminal.as_ref() else {
            return Ok(());
        };
        match (encode(event, terminal.mode()), event) {
            (Some(bytes), InputEvent::Wheel { .. }) => terminal.write(bytes),
            (Some(bytes), _) => {
                terminal.scroll_to_bottom();
                terminal.write(bytes);
            }
            (None, InputEvent::Wheel { up, .. }) => {
                terminal.scroll(if *up { WHEEL_LINES } else { -WHEEL_LINES });
            }
            (None, _) => {}
        }
        agent.dirty = true;
        Ok(())
    }

    fn approve(&mut self, id: AgentId) -> Result<(), String> {
        let agent = self.running_mut(id)?;
        let Some(terminal) = agent.terminal.as_ref() else {
            return Ok(());
        };
        terminal.scroll_to_bottom();
        let screen = terminal.screen().text();
        let keys = if agent.at_trust_dialog && awaits_trust(&screen) {
            trust_keys(&screen).ok_or("cannot find the option that trusts this folder")?
        } else {
            vec![KeyCode::Enter]
        };
        let mode = terminal.mode();
        let bytes: Vec<u8> = keys
            .into_iter()
            .filter_map(|code| encode(&InputEvent::Key(Key::new(code, Modifiers::NONE)), mode))
            .flatten()
            .collect();
        terminal.write(bytes);
        agent.dirty = true;
        Ok(())
    }

    /// Ignored, like a hook, for an agent that is not a running Claude agent.
    fn usage(&mut self, id: AgentId, usage: Usage) -> Result<(), String> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        if agent.record.kind != AgentKind::Claude
            || !agent.status.is_running()
            || agent.usage.as_ref() == Some(&usage)
        {
            return Ok(());
        }
        agent.usage = Some(usage);
        self.announce(id);
        Ok(())
    }

    fn hook(
        &mut self,
        id: AgentId,
        kind: HookKind,
        session_id: Option<String>,
        source: Option<&str>,
        detail: Option<String>,
    ) -> Result<(), String> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        if agent.record.kind != AgentKind::Claude
            || agent.terminal.is_none()
            || !agent.status.is_running()
        {
            return Ok(());
        }
        let mut changed_record = false;
        if kind == HookKind::UserPromptSubmit && !agent.record.resumable {
            agent.record.resumable = true;
            changed_record = true;
        }
        if kind == HookKind::SessionStart
            && let Some(session) = session_id.and_then(|s| s.parse::<Uuid>().ok())
            && agent.record.session_id != Some(session)
        {
            agent.record.session_id = Some(session);
            agent.record.resumable = matches!(source, Some("resume" | "compact"));
            if !agent.record.resumable {
                agent.record.title = None;
            }
            changed_record = true;
        }
        agent.hooked(kind, detail, Instant::now());
        if changed_record {
            self.persist();
        }
        self.announce(id);
        Ok(())
    }

    fn handle_terminal_event(&mut self, event: TerminalEvent) {
        let Some(agent) = self.agents.get_mut(&event.agent) else {
            return;
        };
        if agent.generation != event.generation {
            return;
        }
        let Some(terminal) = agent.terminal.as_mut() else {
            return;
        };
        let root = terminal.pid();
        match terminal.handle(event.event) {
            Outcome::Output => {
                agent.dirty = true;
                agent.last_output = Some(Instant::now());
                if let Some(prompt) = agent.pending_prompt.take() {
                    terminal.write(format!("{prompt}\r").into_bytes());
                }
                if agent.record.kind == AgentKind::Claude
                    && agent.status == Status::Starting
                    && awaits_trust(&terminal.screen().text())
                {
                    agent.set_status(Status::NeedsInput);
                    agent.waiting_for = Some("trusting this folder".into());
                    agent.at_trust_dialog = true;
                    self.announce(event.agent);
                } else if note_blocked_envrc(agent) {
                    self.announce(event.agent);
                }
            }
            Outcome::Exited(code) => {
                agent.set_status(Status::Exited(code));
                agent.dirty = true;
                note_blocked_envrc(agent);
                let tracked = std::mem::take(&mut agent.tracked);
                agent.group = reap::group_exists(root).then_some(root);
                self.reap_off_loop(event.agent, vec![root], tracked);
                self.announce(event.agent);
            }
            Outcome::Title(raw) => {
                let title = session_title(&raw);
                if agent.record.kind == AgentKind::Claude
                    && title.is_some()
                    && agent.record.title != title
                {
                    agent.record.title = title;
                    self.persist();
                    self.announce(event.agent);
                }
            }
            Outcome::Bell => self.broadcast(&ServerMessage::AgentBell(event.agent)),
            Outcome::Nothing => {}
        }
    }

    /// Applies a finished snapshot and, once per `SNAPSHOT`, starts the next one on a
    /// separate thread so `ps` never blocks the event loop.
    fn refresh_tracked(&mut self, now: Instant) {
        if let Some(pending) = &self.snapshot {
            match pending.rx.try_recv() {
                Ok(results) => {
                    if let Some(pending) = self.snapshot.take() {
                        self.snapshot_finished(pending.waiters, results);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.snapshot = None,
            }
        }
        if now.duration_since(self.last_snapshot) < SNAPSHOT {
            return;
        }
        self.last_snapshot = now;
        for agent in self.agents.values_mut() {
            if !agent.status.is_running() {
                agent.group = agent.group.filter(|&leader| reap::group_exists(leader));
            }
        }
        let requests: Vec<(AgentId, u64, u32, Vec<Tracked>)> = self
            .agents
            .values()
            .filter(|agent| agent.status.is_running())
            .filter_map(|agent| {
                let root = agent.terminal.as_ref()?.pid();
                Some((
                    agent.record.id,
                    agent.generation,
                    root,
                    agent.tracked.clone(),
                ))
            })
            .collect();
        if requests.is_empty() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let job = requests.clone();
        let spawned = std::thread::Builder::new()
            .name("snapshot".into())
            .spawn(move || {
                let _ = tx.send(take_snapshot(ProcessTable::read(), job));
            });
        match spawned {
            Ok(_) => {
                self.snapshot = Some(PendingSnapshot {
                    rx,
                    agents: requests.iter().map(|r| r.0).collect(),
                    waiters: Vec::new(),
                })
            }
            Err(e) => {
                tracing::error!(error = %e, "spawning the snapshot thread failed; reading inline");
                self.apply_snapshot(take_snapshot(ProcessTable::read(), requests));
            }
        }
    }

    /// Stores each agent's new tracked list; descendants of an agent that exited, was
    /// stopped or was removed while the snapshot ran are stopped right away.
    fn apply_snapshot(&mut self, results: Snapshot) {
        let mut orphaned = Vec::new();
        for (id, generation, tracked) in results {
            match self.agents.get_mut(&id) {
                Some(agent) if agent.generation == generation && agent.status.is_running() => {
                    agent.tracked = tracked;
                }
                _ => orphaned.push((id, tracked)),
            }
        }
        for (id, tracked) in orphaned {
            self.reap_off_loop(id, Vec::new(), tracked);
        }
    }

    /// Stops the process groups and tracked processes an exited agent left behind on a
    /// separate thread, so waiting out their grace period never blocks the event loop.
    fn reap_off_loop(&mut self, agent: AgentId, groups: Vec<u32>, tracked: Vec<Tracked>) {
        if groups.is_empty() && tracked.is_empty() {
            return;
        }
        let groups: Arc<[u32]> = groups.into();
        let tracked: Arc<[Tracked]> = tracked.into();
        let (job_groups, job_tracked) = (Arc::clone(&groups), Arc::clone(&tracked));
        let spawned = std::thread::Builder::new()
            .name("reap".into())
            .spawn(move || reap::terminate(&[], &job_groups, &job_tracked, STOP_GRACE));
        match spawned {
            Ok(thread) => self.reapers.push(Reaper {
                agent,
                groups,
                tracked,
                thread,
            }),
            Err(e) => {
                tracing::error!(error = %e, "spawning a reaper thread failed; reaping inline");
                let unverified = reap::terminate(&[], &groups, &tracked, STOP_GRACE);
                self.keep_leftovers(agent, unverified);
            }
        }
    }

    /// Joins the finished reapers and keeps what they could not verify.
    fn collect_reapers(&mut self) {
        let (finished, running) = std::mem::take(&mut self.reapers)
            .into_iter()
            .partition::<Vec<_>, _>(|reaper| reaper.thread.is_finished());
        self.reapers = running;
        for reaper in finished {
            let unverified = reaper.thread.join().unwrap_or_else(|_| {
                tracing::error!(agent = %reaper.agent, "a reaper thread panicked");
                reaper.tracked.to_vec()
            });
            self.keep_leftovers(reaper.agent, unverified);
        }
    }

    /// Reaps the leftovers again, at most once per `SNAPSHOT` so a `ps` that keeps
    /// failing does not start a reaper on every tick.
    fn retry_leftovers(&mut self, now: Instant) {
        if self.leftovers.is_empty()
            || self
                .leftovers_retried
                .is_some_and(|at| now.duration_since(at) < SNAPSHOT)
        {
            return;
        }
        self.leftovers_retried = Some(now);
        let mut by_agent: HashMap<AgentId, Vec<Tracked>> = HashMap::new();
        for (agent, tracked) in self.leftovers.drain(..) {
            by_agent.entry(agent).or_default().push(tracked);
        }
        for (agent, tracked) in by_agent {
            self.reap_off_loop(agent, Vec::new(), tracked);
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.finish_jobs();
        self.collect_reapers();
        self.retry_leftovers(now);
        self.refresh_tracked(now);
        let mut changed = Vec::new();
        for (id, agent) in &mut self.agents {
            if agent.terminal.is_none() {
                continue;
            }
            if let Some(status) = settle(
                agent.record.kind,
                agent.status,
                agent.started,
                agent.status_since,
                agent.last_output,
                now,
            ) {
                agent.set_status(status);
                changed.push(*id);
            }
        }
        for id in changed {
            self.announce(id);
        }

        let mut screens: HashMap<AgentId, Screen> = HashMap::new();
        for client in self.clients.values_mut() {
            let Some(sub) = client.subscription.as_mut() else {
                continue;
            };
            let Some(agent) = self.agents.get(&sub.agent) else {
                continue;
            };
            if !agent.dirty && sub.last.is_some() {
                continue;
            }
            let Some(terminal) = agent.terminal.as_ref() else {
                continue;
            };
            let screen = screens
                .entry(sub.agent)
                .or_insert_with(|| terminal.screen());
            if let Some(frame) = screen.diff(sub.last.as_ref(), sub.agent) {
                let _ = client.tx.send(ServerMessage::Frame(frame));
            }
            sub.last = Some(screen.clone());
        }
        for agent in self.agents.values_mut() {
            agent.dirty = false;
        }
    }
}

const EARLY_EXIT: Duration = Duration::from_secs(10);

/// Whether the agent exited with an error within `EARLY_EXIT` of starting.
fn failed_early(agent: &Agent) -> bool {
    matches!(agent.status, Status::Exited(code) if code != 0)
        && agent.status_since.duration_since(agent.started) < EARLY_EXIT
}

/// Records why a worktree agent that direnv refused to start exited; true when it did.
/// The refusal can reach the screen after the exit, as the PTY is drained afterwards,
/// so this runs again on output that follows the exit.
fn note_blocked_envrc(agent: &mut Agent) -> bool {
    if !failed_early(agent) || agent.last_error.is_some() {
        return false;
    }
    let (Some(worktree), Some(terminal)) = (&agent.record.worktree, &agent.terminal) else {
        return false;
    };
    agent.last_error = blocked_envrc_hint(&terminal.screen().text(), &worktree.path);
    agent.last_error.is_some()
}

/// Room left in a frame for everything in a diff response but the text.
const RESPONSE_OVERHEAD: usize = 1024;

/// What a `RemoveAgent` removes besides the agent.
#[derive(Clone, Copy)]
struct Removal {
    worktree: bool,
    force: bool,
    delete_branch: bool,
}

/// Refuses a removal that would lose work: a branch its base does not contain, or, unless
/// `force`, a worktree with changes.
fn removable(
    worktree: Option<(PathBuf, PathBuf)>,
    branch: Option<(String, String)>,
    force: bool,
) -> Result<(), String> {
    if let (Some((project, _)), Some((branch, base))) = (&worktree, &branch)
        && !git::is_merged(project, branch, base).map_err(|e| format!("{e:#}"))?
    {
        return Err(format!(
            "{branch} is not merged into {base}; merge it first, or keep the branch"
        ));
    }
    if let Some((_, path)) = &worktree
        && !force
    {
        let dirty = git::dirty_files(path).map_err(|e| format!("{e:#}"))?;
        if !dirty.is_empty() {
            return Err(format!(
                "worktree has uncommitted changes:\n{}",
                dirty.join("\n")
            ));
        }
    }
    Ok(())
}

/// Removes the worktree, then the branch; the first result says whether the agent may go.
fn remove_worktree_and_branch(
    worktree: Option<(PathBuf, PathBuf)>,
    branch: Option<(String, String)>,
    force: bool,
) -> (Result<(), String>, Result<(), String>) {
    let Some((project, path)) = worktree else {
        return (Ok(()), Ok(()));
    };
    if let Err(e) = git::remove_worktree(&project, &path, force) {
        return (Err(format!("{e:#}")), Ok(()));
    }
    let branch = match branch {
        Some((branch, base)) => git::delete_merged_branch(&project, &branch, &base)
            .map_err(|e| format!("removed the agent but kept {branch}: {e:#}")),
        None => Ok(()),
    };
    (Ok(()), branch)
}

/// What the running snapshot found for a stop's agents; nothing once `SNAPSHOT_WAIT`
/// passed, and the snapshot's results are then applied, or reaped, when it finishes.
fn await_snapshot(snapshot: &Receiver<Vec<Tracked>>) -> Vec<Tracked> {
    match snapshot.recv_timeout(SNAPSHOT_WAIT) {
        Ok(found) => found,
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!(
                "stopping before the running snapshot finished; its results are applied when it does"
            );
            Vec::new()
        }
        Err(RecvTimeoutError::Disconnected) => Vec::new(),
    }
}

/// Makes a new agent's worktree, copies the `.worktreeinclude` files into it and allows its
/// `.envrc` when direnv allows the project's identical one.
fn prepare_worktree(
    state: &Path,
    project: &Project,
    name: &str,
    request: WorktreeRequest,
    direnv: Option<&Path>,
) -> Result<WorktreeInfo, String> {
    let worktree = add_worktree(state, project, name, request)?;
    match git::copy_worktree_includes(&project.path, &worktree.path) {
        Ok(copied) if !copied.is_empty() => {
            tracing::info!(files = ?copied, "copied .worktreeinclude files")
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "copying .worktreeinclude files failed")
        }
    }
    if let Some(direnv) = direnv {
        match direnv::auto_allow(direnv, &project.path, &worktree.path, &[]) {
            Ok(true) => {
                tracing::info!(path = %worktree.path.display(), "allowed the worktree's .envrc")
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "checking the worktree's .envrc failed"),
        }
    }
    Ok(worktree)
}

fn add_worktree(
    state: &Path,
    project: &Project,
    name: &str,
    request: WorktreeRequest,
) -> Result<WorktreeInfo, String> {
    let branch = format!("lazyagents/{name}");
    git::check_branch(&branch).map_err(|e| format!("{e:#}"))?;
    let base = match request.base.filter(|b| !b.trim().is_empty()) {
        Some(base) => base.trim().to_string(),
        None => match git::current_branch(&project.path).map_err(|e| format!("{e:#}"))? {
            detached if detached == "HEAD" => {
                git::head_commit(&project.path).map_err(|e| format!("{e:#}"))?
            }
            branch => branch,
        },
    };
    let state = state
        .canonicalize()
        .map_err(|e| format!("{}: {e}", state.display()))?;
    let id = project.id.to_string();
    let path = state
        .join("worktrees")
        .join(format!("{}-{}", project.name.replace('/', "-"), &id[..8]))
        .join(name);
    git::add_worktree(&project.path, &path, &branch, &base).map_err(|e| format!("{e:#}"))?;
    Ok(WorktreeInfo { path, branch, base })
}
fn inherits_direnv_env(direnv: &Path) -> bool {
    match direnv::inherited_env(direnv) {
        Ok(inherited) => {
            if inherited {
                tracing::info!("reverting the inherited direnv environment for agents");
            }
            inherited
        }
        Err(e) => {
            tracing::warn!("leaving the inherited environment as is: {e:#}");
            false
        }
    }
}

fn run_git_query(dir: &Path, base: Option<&str>, query: GitQuery) -> Result<Response, String> {
    match query {
        GitQuery::Stat => git::stat(dir, base).map(Response::GitStat),
        GitQuery::Diff(scope) => git::diff(dir, base, scope).map(Response::Diff),
    }
    .map_err(|e| format!("{e:#}"))
    .and_then(fit_in_a_message)
}

/// A response too large to frame would close the client's connection instead of
/// reaching it, so it becomes an error the client can show.
fn fit_in_a_message(response: Response) -> Result<Response, String> {
    match &response {
        Response::Diff(text) if text.len() + RESPONSE_OVERHEAD > MAX_MESSAGE_LEN => Err(format!(
            "diff too large to send ({} MiB)",
            text.len() / (1024 * 1024)
        )),
        _ => Ok(response),
    }
}

/// Without a reading every agent keeps its previous list.
fn take_snapshot(
    table: Option<ProcessTable>,
    requests: Vec<(AgentId, u64, u32, Vec<Tracked>)>,
) -> Snapshot {
    requests
        .into_iter()
        .map(|(id, generation, root, previous)| {
            let tracked = match &table {
                Some(table) => table.track(root, &previous),
                None => previous,
            };
            (id, generation, tracked)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::CLAUDE_SILENCE;

    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn shutdown_fits_in_the_clients_wait() {
        assert!(
            SHUTDOWN_SNAPSHOT_WAIT + SHUTDOWN_GRACE + Duration::from_secs(5)
                <= protocol::SHUTDOWN_WAIT
        );
    }

    fn claude_agent() -> Agent {
        Agent::new(AgentRecord {
            id: AgentId::random(),
            project: ProjectId::random(),
            name: "claude".into(),
            kind: AgentKind::Claude,
            command: Vec::new(),
            cwd: PathBuf::from("/p"),
            session_id: None,
            resumable: false,
            worktree: None,
            title: None,
        })
    }

    #[test]
    fn blocked_text_after_a_long_run_is_not_a_start_failure() {
        let mut agent = claude_agent();
        agent.started = Instant::now() - Duration::from_secs(120);
        agent.set_status(Status::Exited(1));
        assert!(!failed_early(&agent));
    }

    #[test]
    fn a_clean_exit_is_never_a_start_failure() {
        let mut agent = claude_agent();
        agent.started = Instant::now() - Duration::from_secs(1);
        agent.set_status(Status::Exited(0));
        assert!(!failed_early(&agent));
    }

    #[test]
    fn a_quick_error_exit_is_a_start_failure() {
        let mut agent = claude_agent();
        agent.started = Instant::now() - Duration::from_secs(1);
        agent.set_status(Status::Exited(1));
        assert!(failed_early(&agent));
    }

    struct Stubborn(std::process::Child);

    impl Stubborn {
        fn spawn() -> (Self, Tracked) {
            let child = std::process::Command::new("sh")
                .args(["-c", "trap '' TERM HUP; exec sleep 300"])
                .spawn()
                .expect("spawn");
            let pid = child.id();
            let stubborn = Self(child);
            let tracked = reap::snapshot(std::process::id())
                .into_iter()
                .find(|t| t.pid == pid)
                .expect("ps lists the child");
            (stubborn, tracked)
        }

        fn exited(&mut self) -> bool {
            self.0.try_wait().expect("try_wait").is_some()
        }

        fn exits_within(&mut self, limit: Duration) -> bool {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if self.exited() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            self.exited()
        }
    }

    impl Drop for Stubborn {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn test_daemon(dir: &Path) -> Daemon {
        let (events, _) = tokio::sync::mpsc::unbounded_channel();
        Daemon::new(
            StatePaths::new(dir),
            PathBuf::from("/usr/bin/true"),
            None,
            StoredState::default(),
            events,
        )
    }

    /// Handles the request and ticks until its job, if any, has finished.
    fn request(daemon: &mut Daemon, request: Request) {
        if let Reply::Now(result) = daemon.handle_request(0, 0, request) {
            result.expect("request");
        }
        let deadline = Instant::now() + WAIT;
        while !daemon.jobs.is_empty() {
            assert!(Instant::now() < deadline, "the request never finished");
            std::thread::sleep(Duration::from_millis(10));
            daemon.tick();
        }
    }

    fn add_agent(daemon: &mut Daemon, status: Status) -> AgentId {
        let mut agent = claude_agent();
        agent.status = status;
        let id = agent.record.id;
        daemon.agents.insert(id, agent);
        id
    }

    #[test]
    fn kill_waits_for_a_running_snapshot_and_stops_what_it_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let id = add_agent(&mut daemon, Status::Exited(0));
        let (mut child, tracked) = Stubborn::spawn();
        let (tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![id],
            waiters: Vec::new(),
        });
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = tx.send(vec![(id, 0, vec![tracked])]);
        });
        request(&mut daemon, Request::KillAgent { id });
        late.join().expect("snapshot thread");
        assert!(child.exited(), "kill answered while the child still ran");
    }

    fn daemon_with_two_agents() -> (tempfile::TempDir, Daemon, AgentId, AgentId) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let a = claude_agent();
        let b = claude_agent();
        let (a_id, b_id) = (a.record.id, b.record.id);
        daemon.agents.insert(a_id, a);
        daemon.agents.insert(b_id, b);
        (dir, daemon, a_id, b_id)
    }

    #[test]
    fn stopping_an_uncovered_agent_does_not_wait_for_the_snapshot() {
        let (_dir, mut daemon, a, b) = daemon_with_two_agents();
        let (_tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![a],
            waiters: Vec::new(),
        });
        let started = Instant::now();
        daemon.stop(&[b], STOP_GRACE);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert!(
            daemon.snapshot.is_some(),
            "the snapshot for {a} must stay pending"
        );
    }

    #[test]
    fn stopping_a_covered_agent_waits_for_the_snapshot() {
        let (_dir, mut daemon, a, _b) = daemon_with_two_agents();
        let (tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![a],
            waiters: Vec::new(),
        });
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = tx.send(Vec::new());
        });
        let started = Instant::now();
        daemon.stop(&[a], STOP_GRACE);
        assert!(started.elapsed() >= Duration::from_millis(250));
        assert!(daemon.snapshot.is_none());
    }

    #[test]
    fn a_snapshot_slower_than_a_stop_is_reaped_when_it_arrives() {
        let (_dir, mut daemon, a, _b) = daemon_with_two_agents();
        let (mut child, tracked) = Stubborn::spawn();
        let (tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![a],
            waiters: Vec::new(),
        });
        let late = std::thread::spawn(move || {
            std::thread::sleep(SNAPSHOT_WAIT + Duration::from_millis(500));
            let _ = tx.send(vec![(a, 0, vec![tracked])]);
        });
        request(&mut daemon, Request::KillAgent { id: a });
        assert!(
            daemon.snapshot.is_some(),
            "the slow snapshot must stay pending"
        );
        late.join().expect("snapshot thread");
        daemon.refresh_tracked(Instant::now());
        assert!(daemon.snapshot.is_none());
        assert!(child.exits_within(WAIT), "child survived");
    }

    #[test]
    fn shutdown_applies_a_snapshot_slower_than_a_stop() {
        let (_dir, mut daemon, a, _b) = daemon_with_two_agents();
        let (mut child, tracked) = Stubborn::spawn();
        let (tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![a],
            waiters: Vec::new(),
        });
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            let _ = tx.send(vec![(a, 0, vec![tracked])]);
        });
        daemon.stop_all();
        late.join().expect("snapshot thread");
        assert!(daemon.snapshot.is_none());
        assert!(
            child.exited(),
            "shutdown finished while the child still ran"
        );
    }

    #[test]
    fn shutdown_stops_what_a_snapshot_of_a_removed_agent_finds() {
        let (_dir, mut daemon, _a, _b) = daemon_with_two_agents();
        let removed = AgentId::random();
        let (mut child, tracked) = Stubborn::spawn();
        let (tx, rx) = std::sync::mpsc::channel();
        daemon.snapshot = Some(PendingSnapshot {
            rx,
            agents: vec![removed],
            waiters: Vec::new(),
        });
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = tx.send(vec![(removed, 0, vec![tracked])]);
        });
        daemon.stop_all();
        late.join().expect("snapshot thread");
        assert!(
            child.exited(),
            "shutdown finished while the child still ran"
        );
    }

    #[test]
    fn a_snapshot_for_a_removed_agent_is_reaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let (mut child, tracked) = Stubborn::spawn();
        daemon.apply_snapshot(vec![(AgentId::random(), 0, vec![tracked])]);
        assert!(child.exits_within(WAIT), "child survived");
    }

    #[test]
    fn a_snapshot_for_an_earlier_generation_is_reaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let id = add_agent(&mut daemon, Status::Idle);
        if let Some(agent) = daemon.agents.get_mut(&id) {
            agent.generation = 1;
        }
        let (mut child, tracked) = Stubborn::spawn();
        daemon.apply_snapshot(vec![(id, 0, vec![tracked])]);
        assert!(child.exits_within(WAIT), "child survived");
        assert!(daemon.agents[&id].tracked.is_empty());
    }

    #[test]
    fn a_stop_that_panics_keeps_its_processes_as_leftovers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let id = add_agent(&mut daemon, Status::Exited(0));
        let tracked = Tracked {
            pid: 4242,
            start: "Thu Oct 15 10:00:00 2026".into(),
        };
        if let Some(agent) = daemon.agents.get_mut(&id) {
            agent.tracked = vec![tracked.clone()];
        }
        daemon.begin_stop(
            id,
            "stopping",
            || panic!("stop failed"),
            |_, _: Option<()>| {},
        );
        let deadline = Instant::now() + WAIT;
        while !daemon.jobs.iter().all(|job| job.is_finished()) {
            assert!(Instant::now() < deadline, "the stop never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        daemon.finish_jobs();
        assert_eq!(daemon.leftovers, vec![(id, tracked)]);
        assert_eq!(daemon.agents[&id].busy, None);
    }

    #[test]
    fn leftovers_are_retried_until_verified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let id = add_agent(&mut daemon, Status::Exited(0));
        let (mut child, tracked) = Stubborn::spawn();
        daemon.leftovers.push((id, tracked));
        daemon.tick();
        let deadline = Instant::now() + WAIT;
        while !daemon.reapers.is_empty() {
            assert!(Instant::now() < deadline, "the reaper did not finish");
            std::thread::sleep(Duration::from_millis(25));
            daemon.tick();
        }
        assert!(child.exited(), "the leftover survived");
        assert!(daemon.leftovers.is_empty());
    }

    #[test]
    fn shutdown_stops_the_leftovers_of_a_removed_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let (mut child, tracked) = Stubborn::spawn();
        daemon.leftovers.push((AgentId::random(), tracked));
        daemon.stop_all();
        assert!(
            child.exited(),
            "shutdown finished while the leftover still ran"
        );
        assert!(daemon.leftovers.is_empty());
    }

    #[test]
    fn a_failed_process_reading_keeps_the_tracked_lists() {
        let id = AgentId::random();
        let previous = vec![Tracked {
            pid: 4242,
            start: "Thu Oct 15 10:00:00 2026".into(),
        }];
        assert_eq!(
            take_snapshot(None, vec![(id, 3, 4241, previous.clone())]),
            vec![(id, 3, previous)]
        );
    }

    #[test]
    fn a_diff_too_large_for_one_message_is_an_error() {
        let fits = "x".repeat(MAX_MESSAGE_LEN - RESPONSE_OVERHEAD);
        assert!(fit_in_a_message(Response::Diff(fits)).is_ok());
        let huge = "x".repeat(MAX_MESSAGE_LEN);
        let err = fit_in_a_message(Response::Diff(huge)).expect_err("too large");
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn a_hook_that_keeps_the_status_still_counts_as_activity() {
        let mut agent = claude_agent();
        let started = agent.started;
        agent.hooked(HookKind::PreToolUse, None, started);
        let again = started + Duration::from_secs(9);
        agent.hooked(HookKind::PreToolUse, None, again);
        assert_eq!(agent.status, Status::Working);
        let after_first_silence = started + CLAUDE_SILENCE + Duration::from_secs(1);
        assert_eq!(
            settle(
                agent.record.kind,
                agent.status,
                agent.started,
                agent.status_since,
                agent.last_output,
                after_first_silence,
            ),
            None
        );
        assert_eq!(
            settle(
                agent.record.kind,
                agent.status,
                agent.started,
                agent.status_since,
                agent.last_output,
                again + CLAUDE_SILENCE,
            ),
            Some(Status::Idle)
        );
    }
}
