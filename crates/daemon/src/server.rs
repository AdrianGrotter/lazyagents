use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Context;
use protocol::{
    AgentId, AgentInfo, AgentKind, ClientMessage, CreateAgent, HookKind, InputEvent,
    MAX_MESSAGE_LEN, PROTOCOL_VERSION, Project, ProjectId, Request, Response, Screen,
    ServerMessage, StatePaths, Status, WorktreeInfo, WorktreeRequest, read_message, write_message,
};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::encode::encode;
use crate::lock::InstanceLock;
use crate::reap::{self, ProcessTable, Tracked};
use crate::spawn::{Launch, SpawnContext, blocked_envrc_hint, hook_settings, spawn_spec};
use crate::status::{on_hook, settle};
use crate::store::{self, AgentRecord, StoredState};
use crate::terminal::{Outcome, PaneSize, Proxy, Terminal, TerminalEvent};
use crate::{direnv, git};

pub struct Config {
    pub paths: StatePaths,
    pub exe: PathBuf,
    /// Exit after this long with no client connected and no agent running.
    pub idle_exit: Option<Duration>,
    /// The direnv binary that wraps agents in a directory with an `.envrc`.
    pub direnv: Option<PathBuf>,
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
    /// Answered later from another thread through the client's sender.
    Deferred,
}

#[derive(Clone, Copy)]
enum GitQuery {
    Stat,
    Diff,
}

const TICK: Duration = Duration::from_millis(33);
const STOP_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const HOUSEKEEPING: Duration = Duration::from_secs(1);
const SNAPSHOT: Duration = Duration::from_secs(1);
const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
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
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                if incoming.send(stream).is_err() {
                    break;
                }
            }
            Err(e) => tracing::warn!(error = %e, "accept failed"),
        }
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
        }
    }

    fn set_status(&mut self, status: Status) {
        if status != self.status {
            self.status = status;
            self.status_since = Instant::now();
        }
    }

    /// A hook counts as activity even when it leaves the status unchanged.
    fn hooked(&mut self, kind: HookKind, now: Instant) {
        self.status = on_hook(kind);
        self.status_since = now;
    }
}

/// Tracked descendants computed off the event loop for one agent generation.
type Snapshot = Vec<(AgentId, u64, Vec<Tracked>)>;

/// A reap running on its own thread. Its processes stay visible here until the thread
/// finishes, so a stop of the same agent, or shutdown, covers them too.
struct Reaper {
    agent: AgentId,
    groups: Arc<[u32]>,
    tracked: Arc<[Tracked]>,
    thread: JoinHandle<()>,
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
    snapshot: Option<std::sync::mpsc::Receiver<Snapshot>>,
    reapers: Vec<Reaper>,
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
        }
    }

    fn add_client(&mut self, client: ClientId, tx: UnboundedSender<ServerMessage>) {
        let _ = tx.send(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        });
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
    }

    fn handle_command(&mut self, command: Command) -> Flow {
        match command {
            Command::Request {
                client,
                id,
                request,
            } => {
                let stop = matches!(request, Request::Shutdown);
                if let Reply::Now(result) = self.handle_request(client, id, request)
                    && let Some(c) = self.clients.get(&client)
                {
                    let _ = c.tx.send(ServerMessage::Response {
                        id,
                        result: *result,
                    });
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
            Request::CreateAgent(create) => self.create_agent(create).map(Response::Agent),
            Request::KillAgent { id } => self.kill_agent(id).map(Response::Agent),
            Request::RestartAgent { id, cols, rows } => self
                .restart_agent(id, PaneSize::new(cols, rows))
                .map(Response::Agent),
            Request::RemoveAgent {
                id,
                remove_worktree,
                force,
            } => self
                .remove_agent(id, remove_worktree, force)
                .map(|()| Response::Ok),
            Request::GitStat { id } => {
                return self.git_query(client, request_id, id, GitQuery::Stat);
            }
            Request::GitDiff { id } => {
                return self.git_query(client, request_id, id, GitQuery::Diff);
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
            Request::Resize { id, cols, rows } => self
                .resize(id, PaneSize::new(cols, rows))
                .map(|()| Response::Ok),
            Request::Scroll { id, lines } => self.scroll(id, lines).map(|()| Response::Ok),
            Request::Hook {
                id,
                kind,
                session_id,
                source,
            } => self
                .hook(id, kind, session_id, source.as_deref())
                .map(|()| Response::Ok),
            Request::Shutdown => {
                self.stop_all();
                Ok(Response::Ok)
            }
        };
        Reply::Now(Box::new(result))
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
            worktree: agent.record.worktree.clone(),
            last_error: agent.last_error.clone(),
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
        if self.agents.values().any(|a| a.record.project == id) {
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
            a.record.project == project && a.record.name == name && Some(a.record.id) != except
        });
        if taken {
            return Err(format!(
                "an agent named {name} already exists in this project"
            ));
        }
        Ok(())
    }

    fn create_agent(&mut self, create: CreateAgent) -> Result<AgentInfo, String> {
        let project = self
            .projects
            .iter()
            .find(|p| p.id == create.project)
            .cloned()
            .ok_or_else(|| format!("unknown project {}", create.project))?;
        self.validate_name(project.id, &create.name, None)?;
        if create.kind == AgentKind::Generic && create.command.is_empty() {
            return Err("a generic agent needs a command".into());
        }
        let worktree = match create.worktree {
            Some(request) => Some(self.add_worktree(&project, &create.name, request)?),
            None => None,
        };
        if let (Some(direnv), Some(worktree)) = (self.direnv.as_deref(), worktree.as_ref()) {
            match direnv::auto_allow(direnv, &project.path, &worktree.path, &[]) {
                Ok(true) => {
                    tracing::info!(path = %worktree.path.display(), "allowed the worktree's .envrc")
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "checking the worktree's .envrc failed"),
            }
        }
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
        };
        let id = record.id;
        self.agents.insert(id, Agent::new(record));
        let started = self.start(id, Launch::Fresh, PaneSize::new(create.cols, create.rows));
        self.persist();
        self.announce(id);
        started?;
        self.info_of(id)
    }

    fn add_worktree(
        &self,
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
        let state = self
            .paths
            .dir()
            .canonicalize()
            .map_err(|e| format!("{}: {e}", self.paths.dir().display()))?;
        let id = project.id.to_string();
        let path = state
            .join("worktrees")
            .join(format!("{}-{}", project.name.replace('/', "-"), &id[..8]))
            .join(name);
        git::add_worktree(&project.path, &path, &branch, &base).map_err(|e| format!("{e:#}"))?;
        Ok(WorktreeInfo { path, branch, base })
    }

    fn start(&mut self, id: AgentId, launch: Launch, size: PaneSize) -> Result<(), String> {
        let project_root = {
            let agent = self.agent(id)?;
            self.projects
                .iter()
                .find(|p| p.id == agent.record.project)
                .map(|p| p.path.clone())
                .unwrap_or_else(|| agent.record.cwd.clone())
        };
        self.stop(&[id], STOP_GRACE);
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        let resume = launch == Launch::Resume && agent.record.resumable;
        if agent.record.kind == AgentKind::Claude {
            if !resume {
                agent.record.session_id = Some(Uuid::new_v4());
                agent.record.resumable = false;
            }
            let settings = self.paths.hook_settings(id);
            let written = std::fs::create_dir_all(self.paths.hooks_dir())
                .and_then(|()| std::fs::write(&settings, hook_settings(&self.exe).to_string()));
            if let Err(e) = written {
                agent.set_status(Status::Exited(127));
                let message = format!("writing {}: {e}", settings.display());
                agent.last_error = Some(message.clone());
                return Err(message);
            }
        }
        let ctx = SpawnContext {
            paths: &self.paths,
            project_root: &project_root,
            direnv: self.direnv.as_deref(),
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

    /// Terminates the given agents' process trees, process groups and tracked
    /// descendants, including those a snapshot still running reports, then drops their
    /// terminals.
    fn stop(&mut self, ids: &[AgentId], grace: Duration) {
        self.finish_snapshot();
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
        reap::terminate(&trees, &groups, &tracked, grace);
        for id in ids {
            if let Some(agent) = self.agents.get_mut(id) {
                agent.group = agent.group.filter(|&leader| reap::group_exists(leader));
                if agent.terminal.take().is_some() {
                    agent.generation += 1;
                }
            }
        }
    }

    fn stop_all(&mut self) {
        let mut ids: Vec<AgentId> = self.agents.keys().copied().collect();
        ids.extend(self.reapers.iter().map(|r| r.agent));
        self.stop(&ids, SHUTDOWN_GRACE);
    }

    fn finish_snapshot(&mut self) {
        let Some(pending) = self.snapshot.take() else {
            return;
        };
        match pending.recv_timeout(SNAPSHOT_WAIT) {
            Ok(results) => self.apply_snapshot(results),
            Err(e) => tracing::warn!(error = %e, "stopping without the running snapshot"),
        }
    }

    fn kill_agent(&mut self, id: AgentId) -> Result<AgentInfo, String> {
        self.agent(id)?;
        self.stop(&[id], STOP_GRACE);
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        agent.set_status(Status::Stopped);
        agent.dirty = true;
        self.announce(id);
        self.info_of(id)
    }

    fn restart_agent(&mut self, id: AgentId, size: PaneSize) -> Result<AgentInfo, String> {
        let started = self.start(id, Launch::Resume, size);
        self.persist();
        self.announce(id);
        started?;
        self.info_of(id)
    }

    fn remove_agent(
        &mut self,
        id: AgentId,
        remove_worktree: bool,
        force: bool,
    ) -> Result<(), String> {
        let agent = self.agent(id)?;
        let removal = match agent.record.worktree.as_ref().filter(|_| remove_worktree) {
            Some(worktree) => {
                let project = self
                    .projects
                    .iter()
                    .find(|p| p.id == agent.record.project)
                    .map(|p| p.path.clone())
                    .ok_or_else(|| format!("unknown project {}", agent.record.project))?;
                Some((project, worktree.path.clone()))
            }
            None => None,
        };
        if let Some((_, path)) = &removal
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
        self.stop(&[id], STOP_GRACE);
        if let Some((project, path)) = &removal
            && let Err(e) = git::remove_worktree(project, path, force)
        {
            if let Some(agent) = self.agents.get_mut(&id) {
                agent.set_status(Status::Stopped);
                agent.dirty = true;
            }
            self.announce(id);
            return Err(format!("{e:#}"));
        }
        self.agents
            .remove(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        let _ = std::fs::remove_file(self.paths.hook_settings(id));
        for client in self.clients.values_mut() {
            if client.subscription.as_ref().is_some_and(|s| s.agent == id) {
                client.subscription = None;
            }
        }
        self.persist();
        self.broadcast(&ServerMessage::AgentRemoved(id));
        Ok(())
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
        let project = self.agent(id)?.record.project;
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

    fn hook(
        &mut self,
        id: AgentId,
        kind: HookKind,
        session_id: Option<String>,
        source: Option<&str>,
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
            changed_record = true;
        }
        agent.hooked(kind, Instant::now());
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
                if note_blocked_envrc(agent) {
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
            Outcome::Nothing => {}
        }
    }

    /// Applies a finished snapshot and, once per `SNAPSHOT`, starts the next one on a
    /// separate thread so `ps` never blocks the event loop.
    fn refresh_tracked(&mut self, now: Instant) {
        if let Some(pending) = &self.snapshot {
            match pending.try_recv() {
                Ok(results) => {
                    self.snapshot = None;
                    self.apply_snapshot(results);
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
            Ok(_) => self.snapshot = Some(rx),
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
                reap::terminate(&[], &groups, &tracked, STOP_GRACE);
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        self.reapers.retain(|reaper| !reaper.thread.is_finished());
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

fn run_git_query(dir: &Path, base: Option<&str>, query: GitQuery) -> Result<Response, String> {
    match query {
        GitQuery::Stat => git::stat(dir, base).map(Response::GitStat),
        GitQuery::Diff => git::diff(dir, base).map(Response::Diff),
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
        daemon.snapshot = Some(rx);
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let _ = tx.send(vec![(id, 0, vec![tracked])]);
        });
        daemon.kill_agent(id).expect("kill");
        late.join().expect("snapshot thread");
        assert!(child.exited(), "kill answered while the child still ran");
    }

    #[test]
    fn a_snapshot_for_a_removed_agent_is_reaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut daemon = test_daemon(dir.path());
        let (mut child, tracked) = Stubborn::spawn();
        daemon.apply_snapshot(vec![(AgentId::random(), 0, vec![tracked])]);
        assert!(child.exits_within(Duration::from_secs(3)), "child survived");
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
        assert!(child.exits_within(Duration::from_secs(3)), "child survived");
        assert!(daemon.agents[&id].tracked.is_empty());
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
        agent.hooked(HookKind::PreToolUse, started);
        let again = started + Duration::from_secs(9);
        agent.hooked(HookKind::PreToolUse, again);
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
