use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use protocol::{
    AgentId, AgentInfo, AgentKind, ClientMessage, CreateAgent, HookKind, InputEvent,
    PROTOCOL_VERSION, Project, ProjectId, Request, Response, Screen, ServerMessage, StatePaths,
    Status, read_message, write_message,
};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use crate::encode::encode;
use crate::lock::InstanceLock;
use crate::reap::terminate_tree;
use crate::spawn::{Launch, SpawnContext, find_in_path, hook_settings, spawn_spec};
use crate::status::{generic_status, on_hook};
use crate::store::{self, AgentRecord, StoredState};
use crate::terminal::{Outcome, PaneSize, Proxy, Terminal, TerminalEvent};

pub struct Config {
    pub paths: StatePaths,
    pub exe: PathBuf,
    /// Exit after this long with no client connected and no agent running.
    pub idle_exit: Option<Duration>,
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

const TICK: Duration = Duration::from_millis(33);
const STOP_GRACE: Duration = Duration::from_secs(1);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const HOUSEKEEPING: Duration = Duration::from_secs(1);
const WHEEL_LINES: i32 = 3;

pub async fn run(config: Config) -> anyhow::Result<()> {
    let paths = config.paths;
    paths
        .create_dir()
        .with_context(|| format!("creating {}", paths.dir().display()))?;
    let _lock = InstanceLock::acquire(&paths.lock_file())?;
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
    let mut daemon = Daemon::new(paths.clone(), config.exe, state, events_tx);
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
            Ok(Some(ClientMessage::Hello { .. })) => {}
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
    terminal: Option<Terminal>,
    generation: u64,
    last_output: Option<Instant>,
    dirty: bool,
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
        state: StoredState,
        events: UnboundedSender<TerminalEvent>,
    ) -> Self {
        let agents = state
            .agents
            .into_iter()
            .map(|record| {
                let agent = Agent {
                    record,
                    status: Status::Stopped,
                    terminal: None,
                    generation: 0,
                    last_output: None,
                    dirty: false,
                };
                (agent.record.id, agent)
            })
            .collect();
        Self {
            paths,
            exe,
            direnv: find_in_path("direnv"),
            projects: state.projects,
            agents,
            clients: HashMap::new(),
            events,
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
                let result = self.handle_request(client, request);
                if let Some(c) = self.clients.get(&client) {
                    let _ = c.tx.send(ServerMessage::Response { id, result });
                }
                if stop { Flow::Stop } else { Flow::Continue }
            }
            Command::Disconnected { client } => {
                self.clients.remove(&client);
                Flow::Continue
            }
        }
    }

    fn handle_request(&mut self, client: ClientId, request: Request) -> Result<Response, String> {
        match request {
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
            Request::RemoveAgent { id } => self.remove_agent(id).map(|()| Response::Ok),
            Request::RenameAgent { id, name } => self.rename_agent(id, name).map(Response::Agent),
            Request::Subscribe { id } => {
                self.agent(id)?;
                if let Some(c) = self.clients.get_mut(&client) {
                    c.subscription = Some(Subscription {
                        agent: id,
                        last: None,
                    });
                }
                Ok(Response::Ok)
            }
            Request::Unsubscribe => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.subscription = None;
                }
                Ok(Response::Ok)
            }
            Request::Input { id, event } => self.input(id, &event).map(|()| Response::Ok),
            Request::Resize { id, cols, rows } => {
                let agent = self.running_mut(id)?;
                if let Some(t) = agent.terminal.as_mut() {
                    t.resize(PaneSize::new(cols, rows));
                }
                agent.dirty = true;
                Ok(Response::Ok)
            }
            Request::Scroll { id, lines } => {
                let agent = self.running_mut(id)?;
                if let Some(t) = agent.terminal.as_ref() {
                    t.scroll(lines);
                }
                agent.dirty = true;
                Ok(Response::Ok)
            }
            Request::Hook {
                id,
                kind,
                session_id,
            } => self.hook(id, kind, session_id).map(|()| Response::Ok),
            Request::Shutdown => {
                self.stop_all();
                Ok(Response::Ok)
            }
        }
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

    fn info(agent: &Agent) -> AgentInfo {
        AgentInfo {
            id: agent.record.id,
            project: agent.record.project,
            name: agent.record.name.clone(),
            kind: agent.record.kind,
            command: agent.record.command.clone(),
            cwd: agent.record.cwd.clone(),
            status: agent.status,
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
        let record = AgentRecord {
            id: AgentId::random(),
            project: project.id,
            name: create.name,
            kind: create.kind,
            command: create.command,
            cwd: project.path.clone(),
            session_id: None,
            resumable: false,
        };
        let id = record.id;
        self.agents.insert(
            id,
            Agent {
                record,
                status: Status::Stopped,
                terminal: None,
                generation: 0,
                last_output: None,
                dirty: false,
            },
        );
        let started = self.start(id, Launch::Fresh, PaneSize::new(create.cols, create.rows));
        self.persist();
        self.announce(id);
        started?;
        self.info_of(id)
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
                agent.status = Status::Exited(127);
                return Err(format!("writing {}: {e}", settings.display()));
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
                agent.terminal = Some(terminal);
                agent.status = Status::Starting;
                agent.last_output = None;
                agent.dirty = true;
                Ok(())
            }
            Err(e) => {
                agent.status = Status::Exited(127);
                Err(format!("{e:#}"))
            }
        }
    }

    /// Terminates the process trees of the given running agents, then drops their
    /// terminals; returns once every process is gone.
    fn stop(&mut self, ids: &[AgentId], grace: Duration) {
        let roots: Vec<u32> = ids
            .iter()
            .filter_map(|id| self.agents.get(id))
            .filter(|agent| agent.status.is_running())
            .filter_map(|agent| agent.terminal.as_ref().map(Terminal::pid))
            .collect();
        terminate_tree(&roots, grace);
        for id in ids {
            if let Some(agent) = self.agents.get_mut(id)
                && agent.terminal.take().is_some()
            {
                agent.generation += 1;
            }
        }
    }

    fn stop_all(&mut self) {
        let ids: Vec<AgentId> = self.agents.keys().copied().collect();
        self.stop(&ids, SHUTDOWN_GRACE);
    }

    fn kill_agent(&mut self, id: AgentId) -> Result<AgentInfo, String> {
        self.agent(id)?;
        self.stop(&[id], STOP_GRACE);
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or_else(|| format!("unknown agent {id}"))?;
        agent.status = Status::Stopped;
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

    fn remove_agent(&mut self, id: AgentId) -> Result<(), String> {
        self.agent(id)?;
        self.stop(&[id], STOP_GRACE);
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
            agent.record.resumable = false;
            changed_record = true;
        }
        agent.status = on_hook(kind);
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
        match terminal.handle(event.event) {
            Outcome::Output => {
                agent.dirty = true;
                agent.last_output = Some(Instant::now());
            }
            Outcome::Exited(code) => {
                agent.status = Status::Exited(code);
                agent.dirty = true;
                self.announce(event.agent);
            }
            Outcome::Nothing => {}
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let mut changed = Vec::new();
        for (id, agent) in &mut self.agents {
            if agent.record.kind == AgentKind::Generic
                && agent.terminal.is_some()
                && agent.status.is_running()
                && agent.last_output.is_some()
            {
                let status = generic_status(agent.last_output, now);
                if status != agent.status {
                    agent.status = status;
                    changed.push(*id);
                }
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
