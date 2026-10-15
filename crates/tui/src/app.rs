use std::path::PathBuf;
use std::time::{Duration, Instant};

use protocol::{
    AgentId, AgentInfo, AgentKind, CreateAgent, InputEvent, Project, ProjectId, Request, Response,
    Screen, ServerMessage, Status,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::keymap::{Action, Keymap};
use crate::keys;

pub const MESSAGE_TTL: Duration = Duration::from_secs(5);
pub const MIN_PANE: (u16, u16) = (20, 5);
const PREFIX_HELP: &str = "ctrl-q: j/k switch · q sidebar · s scroll · ctrl-q send";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Agents,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Claude,
    Shell,
    Custom,
}

impl Preset {
    pub fn label(self) -> &'static str {
        match self {
            Preset::Claude => "claude",
            Preset::Shell => "shell",
            Preset::Custom => "custom",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Preset::Claude => Preset::Shell,
            Preset::Shell => Preset::Custom,
            Preset::Custom => Preset::Claude,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Preset::Claude => Preset::Custom,
            Preset::Shell => Preset::Claude,
            Preset::Custom => Preset::Shell,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Preset,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentForm {
    pub name: String,
    pub preset: Preset,
    pub command: String,
    pub field: Field,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    Kill(AgentId),
    Restart(AgentId),
    Remove(AgentId),
    StopDaemon,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Help,
    NewAgent(NewAgentForm),
    Rename { agent: AgentId, input: String },
    AddProject { input: String },
    Confirm { prompt: String, action: Confirmed },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send(Request),
    Quit,
    StopDaemonAndQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentView {
    Project,
    All,
}

struct PendingCreate {
    project: ProjectId,
    name: String,
    interactions: u64,
}

pub struct App {
    pub projects: Vec<Project>,
    pub agents: Vec<AgentInfo>,
    pub focus: Focus,
    pub view: AgentView,
    pub popup: Option<Popup>,
    pub project_index: usize,
    pub agent_index: usize,
    pub subscribed: Option<AgentId>,
    pub screen: Screen,
    pub pane_size: Option<(u16, u16)>,
    pub message: Option<(String, Instant)>,
    pending_select: Option<PendingCreate>,
    interactions: u64,
    subscribed_running: bool,
    too_small: bool,
    pub prefix_pending: bool,
    pub keymap: Keymap,
    cwd: PathBuf,
    home: Option<PathBuf>,
    shell: String,
}

impl App {
    pub fn new(cwd: PathBuf, shell: String, home: Option<PathBuf>, keymap: Keymap) -> Self {
        Self {
            projects: Vec::new(),
            agents: Vec::new(),
            focus: Focus::Agents,
            view: AgentView::Project,
            popup: None,
            project_index: 0,
            agent_index: 0,
            subscribed: None,
            screen: Screen::blank(0, 0),
            pane_size: None,
            message: None,
            pending_select: None,
            interactions: 0,
            subscribed_running: false,
            too_small: false,
            prefix_pending: false,
            keymap,
            cwd,
            home,
            shell,
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    pub fn visible_agents(&self) -> Vec<&AgentInfo> {
        let position = |id: ProjectId| self.projects.iter().position(|p| p.id == id);
        let mut agents: Vec<&AgentInfo> = match self.view {
            AgentView::All => self.agents.iter().collect(),
            AgentView::Project => {
                let Some(project) = self.selected_project() else {
                    return Vec::new();
                };
                self.agents
                    .iter()
                    .filter(|a| a.project == project.id)
                    .collect()
            }
        };
        agents.sort_by(|a, b| {
            (a.status != Status::NeedsInput, position(a.project), &a.name).cmp(&(
                b.status != Status::NeedsInput,
                position(b.project),
                &b.name,
            ))
        });
        agents
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        self.visible_agents().get(self.agent_index).copied()
    }

    pub fn attention(&self, project: ProjectId) -> usize {
        self.agents
            .iter()
            .filter(|a| a.project == project && a.status == Status::NeedsInput)
            .count()
    }

    pub fn current_message(&self, now: Instant) -> Option<&str> {
        self.message
            .as_ref()
            .filter(|(_, at)| now.saturating_duration_since(*at) < MESSAGE_TTL)
            .map(|(message, _)| message.as_str())
    }

    pub fn animating(&self) -> bool {
        self.agents
            .iter()
            .any(|a| matches!(a.status, Status::Working | Status::Starting))
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), Instant::now()));
    }

    pub fn request_failed(&mut self, message: impl Into<String>) {
        self.pending_select = None;
        self.fail(message);
    }

    pub fn apply_server(&mut self, message: ServerMessage) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        match message {
            ServerMessage::AgentUpdated(info) => self.upsert_agent(info),
            ServerMessage::AgentRemoved(id) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.agents.retain(|a| a.id != id);
                self.reselect(selected);
            }
            ServerMessage::ProjectsChanged(projects) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.replace_projects(projects);
                self.reselect(selected);
            }
            ServerMessage::Frame(frame) => {
                if Some(frame.agent) == self.subscribed {
                    self.screen.apply(&frame);
                }
                return Vec::new();
            }
            ServerMessage::Welcome { .. } | ServerMessage::Response { .. } => {}
        }
        self.sync_after_server(before, None)
    }

    pub fn apply_response(&mut self, response: Response) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        let mut created = None;
        match response {
            Response::Projects(projects) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.replace_projects(projects);
                self.reselect(selected);
            }
            Response::Agents(agents) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.agents = agents;
                self.reselect(selected);
            }
            Response::Project(project) => {
                if !self.projects.iter().any(|p| p.id == project.id) {
                    self.projects.push(project.clone());
                }
                if let Some(index) = self.projects.iter().position(|p| p.id == project.id) {
                    self.highlight_project(index);
                }
            }
            Response::Agent(info) => {
                let pending = self
                    .pending_select
                    .take_if(|p| p.project == info.project && p.name == info.name);
                let undisturbed = pending.is_some_and(|p| p.interactions == self.interactions)
                    && self.popup.is_none();
                if undisturbed && info.status.is_running() {
                    created = Some(info.id);
                }
                self.upsert_agent(info);
            }
            Response::Ok | Response::GitStat(_) | Response::Diff(_) => {}
        }
        self.sync_after_server(before, created)
    }

    /// Focus enters the pane only through an explicit user action, so a selection
    /// change caused by the daemon returns focus to the sidebar. `created` is the agent
    /// the user just created and has not navigated away from since.
    fn sync_after_server(
        &mut self,
        before: Option<AgentId>,
        created: Option<AgentId>,
    ) -> Vec<Effect> {
        self.clamp();
        if self.focus == Focus::Pane && self.selected_agent().map(|a| a.id) != before {
            self.leave_pane();
        }
        if let Some(id) = created {
            self.select_agent(id);
            self.focus = Focus::Pane;
        }
        self.sync()
    }

    /// Below `MIN_PANE` the size is forgotten, nothing is resized and keys are ignored
    /// except the ones that quit or leave the pane.
    pub fn set_pane_size(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
        let was_too_small =
            std::mem::replace(&mut self.too_small, cols < MIN_PANE.0 || rows < MIN_PANE.1);
        if self.too_small {
            self.pane_size = None;
            return Vec::new();
        }
        if was_too_small {
            self.prefix_pending = false;
        }
        if self.pane_size == Some((cols, rows)) {
            return Vec::new();
        }
        self.pane_size = Some((cols, rows));
        match self.selected_agent() {
            Some(agent) if agent.status.is_running() => {
                vec![Effect::Send(Request::Resize {
                    id: agent.id,
                    cols,
                    rows,
                })]
            }
            _ => Vec::new(),
        }
    }

    /// Keeps the highlighted project; if it is gone, highlights the selected agent's
    /// project, and otherwise leaves the index to be clamped.
    fn replace_projects(&mut self, projects: Vec<Project>) {
        let highlighted = self.selected_project().map(|p| p.id);
        let agent_project = self.selected_agent().map(|a| a.project);
        self.projects = projects;
        let position = |id: ProjectId| self.projects.iter().position(|p| p.id == id);
        if let Some(index) = highlighted
            .and_then(position)
            .or_else(|| agent_project.and_then(position))
        {
            self.project_index = index;
        }
    }

    fn upsert_agent(&mut self, info: AgentInfo) {
        let selected = self.selected_agent().map(|a| a.id);
        match self.agents.iter_mut().find(|a| a.id == info.id) {
            Some(existing) => *existing = info,
            None => self.agents.push(info),
        }
        self.reselect(selected);
    }

    /// Keeps `selected` selected. In the all-agents view the project highlight stays
    /// where the user put it, which may be a project without agents.
    fn reselect(&mut self, selected: Option<AgentId>) {
        let Some(id) = selected else {
            return;
        };
        match self.view {
            AgentView::Project => self.select_agent(id),
            AgentView::All => {
                if let Some(index) = self.visible_agents().iter().position(|a| a.id == id) {
                    self.agent_index = index;
                }
            }
        }
    }

    /// Highlights a project and selects its first agent; in the all-agents view a
    /// project without agents keeps the current agent selected.
    fn highlight_project(&mut self, index: usize) {
        let selected = self.selected_agent().map(|a| a.id);
        self.project_index = index;
        match self.view {
            AgentView::Project => self.agent_index = 0,
            AgentView::All => {
                let project = self.projects.get(index).map(|p| p.id);
                let first = self
                    .visible_agents()
                    .iter()
                    .position(|a| Some(a.project) == project);
                match first {
                    Some(first) => self.agent_index = first,
                    None => self.reselect(selected),
                }
            }
        }
    }

    fn select_agent(&mut self, id: AgentId) {
        let Some(project) = self.agents.iter().find(|a| a.id == id).map(|a| a.project) else {
            return;
        };
        if let Some(index) = self.projects.iter().position(|p| p.id == project) {
            self.project_index = index;
        }
        if let Some(index) = self.visible_agents().iter().position(|a| a.id == id) {
            self.agent_index = index;
        }
    }

    fn clamp(&mut self) {
        self.project_index = self
            .project_index
            .min(self.projects.len().saturating_sub(1));
        let count = self.visible_agents().len();
        self.agent_index = self.agent_index.min(count.saturating_sub(1));
    }

    fn sync(&mut self) -> Vec<Effect> {
        self.clamp();
        let wanted = self.selected_agent().map(|a| (a.id, a.status.is_running()));
        let running = wanted.is_some_and(|(_, running)| running);
        if !running && self.focus == Focus::Pane {
            self.leave_pane();
        }
        if wanted.map(|(id, _)| id) == self.subscribed {
            let started = running && !self.subscribed_running;
            self.subscribed_running = running;
            return match (wanted, self.pane_size) {
                (Some((id, _)), Some((cols, rows))) if started => {
                    vec![Effect::Send(Request::Resize { id, cols, rows })]
                }
                _ => Vec::new(),
            };
        }
        self.subscribed = wanted.map(|(id, _)| id);
        self.subscribed_running = running;
        self.screen = Screen::blank(0, 0);
        match wanted {
            Some((id, running)) => {
                let mut effects = vec![Effect::Send(Request::Subscribe { id })];
                if let (true, Some((cols, rows))) = (running, self.pane_size) {
                    effects.push(Effect::Send(Request::Resize { id, cols, rows }));
                }
                effects
            }
            None => vec![Effect::Send(Request::Unsubscribe)],
        }
    }
}

const DEFAULT_SIZE: (u16, u16) = (120, 40);
const WHEEL_LINES: i32 = 3;

#[derive(Debug, PartialEq, Eq)]
enum Edit {
    Continue,
    Submit,
    Cancel,
}

fn edit(input: &mut String, key: &KeyEvent) -> Edit {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Edit::Submit,
        KeyCode::Esc => Edit::Cancel,
        KeyCode::Backspace => {
            input.pop();
            Edit::Continue
        }
        KeyCode::Char('u') if ctrl => {
            input.clear();
            Edit::Continue
        }
        KeyCode::Char(c) if !ctrl => {
            input.push(c);
            Edit::Continue
        }
        _ => Edit::Continue,
    }
}

impl App {
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.interactions += 1;
        if self.too_small {
            return self.on_too_small_key(key);
        }
        if self.popup.is_some() {
            return self.on_popup_key(key);
        }
        match self.focus {
            Focus::Pane => self.on_pane_key(key),
            Focus::Projects | Focus::Agents => self.on_sidebar_key(key),
        }
    }

    /// Nothing is visible, so only `q`/`ctrl-c` (quit from the sidebar) and the prefix
    /// followed by a leave key (leave the pane) act; no key reaches an agent.
    fn on_too_small_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.focus == Focus::Pane {
            if std::mem::take(&mut self.prefix_pending) {
                if self.keymap.after_prefix(&key) == Some(Action::PaneLeave) {
                    self.leave_pane();
                }
            } else {
                self.prefix_pending = self.keymap.is_prefix(&key);
            }
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') if !ctrl => vec![Effect::Quit],
            KeyCode::Char('c') if ctrl => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    pub fn on_paste(&mut self, text: String) -> Vec<Effect> {
        if self.too_small {
            return Vec::new();
        }
        if self.popup.is_none() {
            return match (self.focus, self.selected_agent()) {
                (Focus::Pane, Some(agent)) => {
                    vec![Effect::Send(Request::Input {
                        id: agent.id,
                        event: InputEvent::Paste(text),
                    })]
                }
                _ => Vec::new(),
            };
        }
        let line = text
            .split(['\r', '\n'])
            .next()
            .unwrap_or_default()
            .to_string();
        match &mut self.popup {
            Some(Popup::Rename { input, .. } | Popup::AddProject { input }) => {
                input.push_str(&line)
            }
            Some(Popup::NewAgent(form)) => match form.field {
                Field::Name => form.name.push_str(&line),
                Field::Command => form.command.push_str(&line),
                Field::Preset => {}
            },
            Some(Popup::Help | Popup::Confirm { .. }) | None => {}
        }
        Vec::new()
    }

    pub fn on_wheel(&mut self, up: bool, col: u16, row: u16) -> Vec<Effect> {
        if self.popup.is_some() {
            return Vec::new();
        }
        let Some(agent) = self.selected_agent() else {
            return Vec::new();
        };
        let request = if agent.status.is_running() {
            Request::Input {
                id: agent.id,
                event: InputEvent::Wheel { up, col, row },
            }
        } else {
            Request::Scroll {
                id: agent.id,
                lines: if up { WHEEL_LINES } else { -WHEEL_LINES },
            }
        };
        vec![Effect::Send(request)]
    }

    fn on_pane_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(id) = self.selected_agent().map(|a| a.id) else {
            self.leave_pane();
            return Vec::new();
        };
        if std::mem::take(&mut self.prefix_pending) {
            return match self.keymap.after_prefix(&key) {
                Some(Action::PaneNext) => self.cycle_in_pane(1),
                Some(Action::PanePrevious) => self.cycle_in_pane(-1),
                Some(Action::PaneLeave) => {
                    self.leave_pane();
                    Vec::new()
                }
                Some(Action::PaneSendPrefix) => self.input(id, &self.keymap.prefix().event()),
                Some(_) => Vec::new(),
                None => {
                    self.fail(PREFIX_HELP);
                    Vec::new()
                }
            };
        }
        if self.keymap.is_prefix(&key) {
            self.prefix_pending = true;
            return Vec::new();
        }
        if self.keymap.is_direct_leave(&key) {
            self.leave_pane();
            return Vec::new();
        }
        self.input(id, &key)
    }

    fn input(&self, id: AgentId, key: &KeyEvent) -> Vec<Effect> {
        match keys::to_protocol(key) {
            Some(key) => vec![Effect::Send(Request::Input {
                id,
                event: InputEvent::Key(key),
            })],
            None => Vec::new(),
        }
    }

    fn cycle_in_pane(&mut self, step: i32) -> Vec<Effect> {
        self.move_agent(step);
        self.sync()
    }

    fn leave_pane(&mut self) {
        self.focus = Focus::Agents;
        self.prefix_pending = false;
    }

    fn on_sidebar_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(action) = self.keymap.sidebar(&key) else {
            return Vec::new();
        };
        match action {
            Action::Quit => return vec![Effect::Quit],
            Action::StopDaemon => {
                self.popup = Some(Popup::Confirm {
                    prompt: "Stop the daemon and every agent?".into(),
                    action: Confirmed::StopDaemon,
                });
            }
            Action::Help => self.popup = Some(Popup::Help),
            Action::Down => self.step(1),
            Action::Up => self.step(-1),
            Action::FocusProjects => self.focus = Focus::Projects,
            Action::FocusAgents => self.focus = Focus::Agents,
            Action::ToggleFocus => {
                self.focus = if self.focus == Focus::Projects {
                    Focus::Agents
                } else {
                    Focus::Projects
                };
            }
            Action::Open => match (self.focus, self.selected_agent()) {
                (Focus::Projects, _) => self.focus = Focus::Agents,
                (_, Some(agent)) if agent.status.is_running() => self.focus = Focus::Pane,
                (_, Some(agent)) => {
                    let message = format!("{} is not running — r restarts", agent.name);
                    self.fail(message);
                }
                (_, None) => {}
            },
            Action::NewAgent => self.open_new_agent(),
            Action::AddProject => {
                self.popup = Some(Popup::AddProject {
                    input: self.cwd.display().to_string(),
                });
            }
            Action::Stop => self.confirm_for_selected("Stop", Confirmed::Kill),
            Action::Remove => self.confirm_for_selected("Remove", Confirmed::Remove),
            Action::Restart => match self.selected_agent() {
                Some(agent) if agent.status.is_running() => {
                    self.confirm_for_selected("Restart", Confirmed::Restart)
                }
                Some(agent) => return vec![self.restart(agent.id)],
                None => {}
            },
            Action::Rename => {
                if let Some(agent) = self.selected_agent() {
                    self.popup = Some(Popup::Rename {
                        agent: agent.id,
                        input: agent.name.clone(),
                    });
                }
            }
            Action::NextAttention => self.next_attention(),
            Action::ToggleAllAgents => {
                let selected = self.selected_agent().map(|a| a.id);
                self.view = match self.view {
                    AgentView::Project => AgentView::All,
                    AgentView::All => AgentView::Project,
                };
                self.agent_index = 0;
                if let Some(id) = selected {
                    self.select_agent(id);
                }
            }
            Action::Diff
            | Action::Scroll
            | Action::PaneNext
            | Action::PanePrevious
            | Action::PaneLeave
            | Action::PaneScroll
            | Action::PaneSendPrefix => {}
        }
        self.sync()
    }

    fn restart(&self, id: AgentId) -> Effect {
        let (cols, rows) = self.pane_size.unwrap_or(DEFAULT_SIZE);
        Effect::Send(Request::RestartAgent { id, cols, rows })
    }

    fn confirm_for_selected(&mut self, verb: &str, action: fn(AgentId) -> Confirmed) {
        if let Some(agent) = self.selected_agent() {
            let prompt = format!("{verb} {}?", agent.name);
            let action = action(agent.id);
            self.popup = Some(Popup::Confirm { prompt, action });
        }
    }

    fn step(&mut self, step: i32) {
        match self.focus {
            Focus::Projects => {
                let count = self.projects.len();
                if count > 0 {
                    self.highlight_project(wrap(self.project_index, step, count));
                }
            }
            Focus::Agents | Focus::Pane => self.move_agent(step),
        }
    }

    fn move_agent(&mut self, step: i32) {
        let count = self.visible_agents().len();
        if count == 0 {
            return;
        }
        self.agent_index = wrap(self.agent_index, step, count);
        if self.view == AgentView::All
            && let Some(project) = self.selected_agent().map(|a| a.project)
            && let Some(index) = self.projects.iter().position(|p| p.id == project)
        {
            self.project_index = index;
        }
    }

    fn open_new_agent(&mut self) {
        if self.selected_project().is_none() {
            self.fail("add a project first (a)");
            return;
        }
        self.popup = Some(Popup::NewAgent(NewAgentForm {
            name: String::new(),
            preset: Preset::Claude,
            command: String::new(),
            field: Field::Name,
        }));
    }

    fn next_attention(&mut self) {
        let position = |id: ProjectId| self.projects.iter().position(|p| p.id == id);
        let mut ordered: Vec<&AgentInfo> = self.agents.iter().collect();
        ordered.sort_by(|a, b| (position(a.project), &a.name).cmp(&(position(b.project), &b.name)));
        let current = self.selected_agent().map(|a| a.id);
        let start = current
            .and_then(|id| ordered.iter().position(|a| a.id == id))
            .map_or(0, |i| i + 1);
        let next = (0..ordered.len())
            .map(|offset| ordered[(start + offset) % ordered.len()])
            .find(|a| a.status == Status::NeedsInput)
            .map(|a| a.id);
        match next {
            Some(id) => self.select_agent(id),
            None => self.fail("no agent needs input"),
        }
    }

    fn on_popup_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(popup) = self.popup.take() else {
            return Vec::new();
        };
        match popup {
            Popup::Help => Vec::new(),
            Popup::Confirm { prompt, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => match action {
                    Confirmed::Kill(id) => vec![Effect::Send(Request::KillAgent { id })],
                    Confirmed::Restart(id) => vec![self.restart(id)],
                    Confirmed::Remove(id) => vec![Effect::Send(Request::RemoveAgent {
                        id,
                        remove_worktree: false,
                        force: false,
                    })],
                    Confirmed::StopDaemon => vec![Effect::StopDaemonAndQuit],
                },
                KeyCode::Char('n') | KeyCode::Esc => Vec::new(),
                _ => {
                    self.popup = Some(Popup::Confirm { prompt, action });
                    Vec::new()
                }
            },
            Popup::Rename { agent, mut input } => match edit(&mut input, &key) {
                Edit::Submit => vec![Effect::Send(Request::RenameAgent {
                    id: agent,
                    name: input.trim().to_string(),
                })],
                Edit::Cancel => Vec::new(),
                Edit::Continue => {
                    self.popup = Some(Popup::Rename { agent, input });
                    Vec::new()
                }
            },
            Popup::AddProject { mut input } => match edit(&mut input, &key) {
                Edit::Submit => vec![Effect::Send(Request::AddProject {
                    path: self.project_path(input.trim()),
                    name: None,
                })],
                Edit::Cancel => Vec::new(),
                Edit::Continue => {
                    self.popup = Some(Popup::AddProject { input });
                    Vec::new()
                }
            },
            Popup::NewAgent(form) => self.on_form_key(form, key),
        }
    }

    fn project_path(&self, input: &str) -> PathBuf {
        let home = self.home.as_ref();
        match (input, input.strip_prefix("~/"), home) {
            ("~", _, Some(home)) => home.clone(),
            (_, Some(rest), Some(home)) => home.join(rest),
            _ => self.cwd.join(input),
        }
    }

    fn on_form_key(&mut self, mut form: NewAgentForm, key: KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Esc => return Vec::new(),
            KeyCode::Enter => return self.submit_form(form),
            KeyCode::Tab | KeyCode::Down => form.field = next_field(form.field, form.preset),
            KeyCode::BackTab | KeyCode::Up => form.field = previous_field(form.field, form.preset),
            KeyCode::Left if form.field == Field::Preset => form.preset = form.preset.previous(),
            KeyCode::Right | KeyCode::Char(' ') if form.field == Field::Preset => {
                form.preset = form.preset.next()
            }
            _ => {
                let target = match form.field {
                    Field::Name => Some(&mut form.name),
                    Field::Command => Some(&mut form.command),
                    Field::Preset => None,
                };
                if let Some(target) = target {
                    edit(target, &key);
                }
            }
        }
        self.popup = Some(Popup::NewAgent(form));
        Vec::new()
    }

    fn submit_form(&mut self, form: NewAgentForm) -> Vec<Effect> {
        let Some(project) = self.selected_project().map(|p| p.id) else {
            return Vec::new();
        };
        let name = form.name.trim().to_string();
        if name.is_empty() {
            self.fail("name the agent first");
            self.popup = Some(Popup::NewAgent(form));
            return Vec::new();
        }
        let (kind, command) = match form.preset {
            Preset::Claude => (AgentKind::Claude, Vec::new()),
            Preset::Shell => (AgentKind::Generic, vec![self.shell.clone()]),
            Preset::Custom => {
                let command = form.command.trim().to_string();
                if command.is_empty() {
                    self.fail("enter a command");
                    self.popup = Some(Popup::NewAgent(form));
                    return Vec::new();
                }
                (AgentKind::Generic, vec!["sh".into(), "-c".into(), command])
            }
        };
        let (cols, rows) = self.pane_size.unwrap_or(DEFAULT_SIZE);
        self.pending_select = Some(PendingCreate {
            project,
            name: name.clone(),
            interactions: self.interactions,
        });
        vec![Effect::Send(Request::CreateAgent(CreateAgent {
            project,
            name,
            kind,
            command,
            cols,
            rows,
            worktree: None,
        }))]
    }
}

fn wrap(index: usize, step: i32, count: usize) -> usize {
    (index as i64 + i64::from(step)).rem_euclid(count as i64) as usize
}

fn next_field(field: Field, preset: Preset) -> Field {
    match (field, preset) {
        (Field::Name, _) => Field::Preset,
        (Field::Preset, Preset::Custom) => Field::Command,
        (Field::Preset, _) | (Field::Command, _) => Field::Name,
    }
}

fn previous_field(field: Field, preset: Preset) -> Field {
    match (field, preset) {
        (Field::Name, Preset::Custom) => Field::Command,
        (Field::Name, _) => Field::Preset,
        (Field::Preset, _) => Field::Name,
        (Field::Command, _) => Field::Preset,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use protocol::{AgentKind, Cell, CreateAgent, Frame, InputEvent, Row};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    pub(crate) fn project(name: &str) -> Project {
        Project {
            id: ProjectId::random(),
            name: name.into(),
            path: format!("/p/{name}").into(),
        }
    }

    pub(crate) fn agent(project: &Project, name: &str, status: Status) -> AgentInfo {
        AgentInfo {
            id: AgentId::random(),
            project: project.id,
            name: name.into(),
            kind: AgentKind::Generic,
            command: vec!["sh".into()],
            cwd: project.path.clone(),
            status,
            worktree: None,
            last_error: None,
        }
    }

    pub(crate) fn loaded(projects: Vec<Project>, agents: Vec<AgentInfo>) -> (App, Vec<Effect>) {
        let mut app = App::new(
            "/work".into(),
            "/bin/sh".into(),
            Some("/home/u".into()),
            Keymap::defaults(),
        );
        let mut effects = app.apply_response(Response::Projects(projects));
        effects.extend(app.apply_response(Response::Agents(agents)));
        (app, effects)
    }

    #[test]
    fn loading_selects_and_subscribes_to_the_first_agent() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (app, effects) = loaded(vec![p], vec![a.clone()]);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: a.id })]);
        assert_eq!(app.subscribed, Some(a.id));
    }

    #[test]
    fn needs_input_sorts_first_within_the_project() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::NeedsInput);
        let (app, _) = loaded(vec![p], vec![a, b]);
        let names: Vec<&str> = app
            .visible_agents()
            .iter()
            .map(|x| x.name.as_str())
            .collect();
        assert_eq!(names, ["beta", "alpha"]);
    }

    #[test]
    fn selection_follows_the_agent_when_sorting_changes() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let mut b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        app.agent_index = 1;
        let _ = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        let before = app.subscribed;
        b.status = Status::NeedsInput;
        let effects = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.subscribed, before);
        assert!(effects.is_empty());
        let _ = a;
    }

    #[test]
    fn all_view_lists_every_project_needing_input_first() {
        let p = project("p");
        let q = project("q");
        let (mut app, _) = loaded(
            vec![p.clone(), q.clone()],
            vec![
                agent(&p, "b", Status::Idle),
                agent(&q, "a", Status::NeedsInput),
                agent(&p, "a", Status::Idle),
            ],
        );
        assert_eq!(app.visible_agents().len(), 2);
        app.view = AgentView::All;
        let rows: Vec<(ProjectId, &str)> = app
            .visible_agents()
            .iter()
            .map(|x| (x.project, x.name.as_str()))
            .collect();
        assert_eq!(rows, [(q.id, "a"), (p.id, "a"), (p.id, "b")]);
    }

    #[test]
    fn attention_counts_agents_needing_input() {
        let p = project("p");
        let q = project("q");
        let (app, _) = loaded(
            vec![p.clone(), q.clone()],
            vec![
                agent(&p, "a", Status::NeedsInput),
                agent(&p, "b", Status::NeedsInput),
                agent(&q, "c", Status::Idle),
            ],
        );
        assert_eq!(app.attention(p.id), 2);
        assert_eq!(app.attention(q.id), 0);
    }

    #[test]
    fn frames_for_other_agents_are_ignored() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let cells = vec![Cell {
            text: "x".into(),
            ..Cell::default()
        }];
        let frame = |agent| Frame {
            agent,
            cols: 1,
            rows: 1,
            full: true,
            lines: vec![Row {
                index: 0,
                cells: cells.clone(),
            }],
            cursor: None,
        };
        let _ = app.apply_server(ServerMessage::Frame(frame(AgentId::random())));
        assert_eq!(app.screen.text(), "");
        let _ = app.apply_server(ServerMessage::Frame(frame(a.id)));
        assert_eq!(app.screen.text(), "x");
    }

    #[test]
    fn removing_the_selected_agent_moves_the_subscription() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        app.focus = Focus::Pane;
        let effects = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!(app.subscribed, Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        let effects = app.apply_server(ServerMessage::AgentRemoved(b.id));
        assert_eq!(app.subscribed, None);
        assert_eq!(effects, vec![Effect::Send(Request::Unsubscribe)]);
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn pane_size_resizes_running_agents_only_when_it_changes() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone()], vec![a.clone()]);
        assert_eq!(
            app.set_pane_size(80, 20),
            vec![Effect::Send(Request::Resize {
                id: a.id,
                cols: 80,
                rows: 20
            })]
        );
        assert!(app.set_pane_size(80, 20).is_empty());
        let stopped = AgentInfo {
            status: Status::Stopped,
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(stopped));
        assert!(app.set_pane_size(90, 20).is_empty());
    }

    #[test]
    fn pane_sizes_below_the_floor_never_resize() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        assert!(app.set_pane_size(19, 30).is_empty());
        assert!(app.set_pane_size(80, 4).is_empty());
        assert!(app.set_pane_size(0, 0).is_empty());
        assert_eq!(
            app.set_pane_size(20, 5),
            vec![Effect::Send(Request::Resize {
                id: a.id,
                cols: 20,
                rows: 5
            })]
        );
    }

    #[test]
    fn subscribing_sends_the_known_pane_size() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let mut app = App::new(
            "/work".into(),
            "/bin/sh".into(),
            Some("/home/u".into()),
            Keymap::defaults(),
        );
        let _ = app.set_pane_size(70, 15);
        let _ = app.apply_response(Response::Projects(vec![p]));
        let effects = app.apply_response(Response::Agents(vec![a.clone()]));
        assert_eq!(
            effects,
            vec![
                Effect::Send(Request::Subscribe { id: a.id }),
                Effect::Send(Request::Resize {
                    id: a.id,
                    cols: 70,
                    rows: 15
                }),
            ]
        );
    }

    #[test]
    fn added_project_becomes_selected() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p], vec![]);
        let q = project("q");
        let _ = app.apply_response(Response::Project(q.clone()));
        assert_eq!(app.selected_project().map(|x| x.id), Some(q.id));
    }

    #[test]
    fn messages_expire() {
        let mut app = App::new(
            "/work".into(),
            "/bin/sh".into(),
            Some("/home/u".into()),
            Keymap::defaults(),
        );
        app.fail("boom");
        let now = Instant::now();
        assert_eq!(app.current_message(now), Some("boom"));
        assert_eq!(
            app.current_message(now + MESSAGE_TTL + Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn removing_another_agent_keeps_the_selection() {
        let p = project("p");
        let a = agent(&p, "a", Status::Idle);
        let b = agent(&p, "b", Status::Idle);
        let c = agent(&p, "c", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone(), c]);
        app.agent_index = 1;
        let _ = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        let effects = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.subscribed, Some(b.id));
        assert!(effects.is_empty());
    }

    #[test]
    fn all_view_selection_moves_the_project_highlight() {
        let p = project("p");
        let q = project("q");
        let b = agent(&q, "b", Status::Idle);
        let (mut app, _) = loaded(
            vec![p.clone(), q.clone()],
            vec![agent(&p, "a", Status::Idle), b.clone()],
        );
        app.view = AgentView::All;
        app.select_agent(b.id);
        assert_eq!(app.project_index, 1);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn projects_changed_reorder_keeps_the_selected_agent() {
        let p = project("p");
        let q = project("q");
        let b = agent(&q, "b", Status::Idle);
        let (mut app, _) = loaded(
            vec![p.clone(), q.clone()],
            vec![agent(&p, "a", Status::Idle), b.clone()],
        );
        app.view = AgentView::All;
        app.select_agent(b.id);
        let _ = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        let effects = app.apply_server(ServerMessage::ProjectsChanged(vec![q, p]));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert!(effects.is_empty());
    }

    #[test]
    fn selected_agent_starting_again_gets_resized() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let _ = app.set_pane_size(80, 20);
        let restarted = AgentInfo {
            status: Status::Starting,
            ..a.clone()
        };
        let effects = app.apply_server(ServerMessage::AgentUpdated(restarted));
        assert_eq!(
            effects,
            vec![Effect::Send(Request::Resize {
                id: a.id,
                cols: 80,
                rows: 20
            })]
        );
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    fn input_key(id: AgentId, k: protocol::Key) -> Effect {
        Effect::Send(Request::Input {
            id,
            event: InputEvent::Key(k),
        })
    }

    fn two_agents() -> (App, AgentInfo, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        (app, a, b)
    }

    #[test]
    fn j_and_k_move_and_resubscribe() {
        let (mut app, a, b) = two_agents();
        assert_eq!(
            app.on_key(key('j')),
            vec![Effect::Send(Request::Subscribe { id: b.id })]
        );
        assert_eq!(
            app.on_key(key('k')),
            vec![Effect::Send(Request::Subscribe { id: a.id })]
        );
    }

    #[test]
    fn enter_focuses_the_pane_and_detach_returns() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        let _ = app.on_key(ctrl(']'));
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn pane_focus_sends_letters_to_the_agent() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        for (event, expected) in [
            (
                key('q'),
                protocol::Key::new(protocol::KeyCode::Char('q'), protocol::Modifiers::NONE),
            ),
            (
                key('x'),
                protocol::Key::new(protocol::KeyCode::Char('x'), protocol::Modifiers::NONE),
            ),
            (
                key('n'),
                protocol::Key::new(protocol::KeyCode::Char('n'), protocol::Modifiers::NONE),
            ),
            (
                ctrl('c'),
                protocol::Key::new(protocol::KeyCode::Char('c'), protocol::Modifiers::CTRL),
            ),
        ] {
            assert_eq!(app.on_key(event), vec![input_key(a.id, expected)]);
            assert_eq!(app.focus, Focus::Pane);
            assert_eq!(app.popup, None);
        }
    }

    #[test]
    fn pane_focus_never_quits() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        for event in [key('q'), key('Q'), ctrl('c'), code(KeyCode::Esc)] {
            assert!(!app.on_key(event).contains(&Effect::Quit));
        }
    }

    fn char_input(id: AgentId, c: char, modifiers: protocol::Modifiers) -> Effect {
        input_key(
            id,
            protocol::Key::new(protocol::KeyCode::Char(c), modifiers),
        )
    }

    #[test]
    fn prefix_then_j_switches_and_stays_in_the_pane() {
        let (mut app, a, b) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert!(app.on_key(ctrl('q')).is_empty());
        assert!(app.prefix_pending);
        let effects = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Pane);
        assert!(!app.prefix_pending);
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        assert!(app.on_key(ctrl('q')).is_empty());
        let effects = app.on_key(key('k'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: a.id })]);
    }

    #[test]
    fn prefix_survives_the_redraw_between_keys() {
        let (mut app, _, b) = two_agents();
        let _ = app.set_pane_size(80, 20);
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        assert!(app.set_pane_size(80, 20).is_empty());
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn prefix_onto_a_stopped_agent_returns_to_the_sidebar() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a, b.clone()]);
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Agents);
        assert!(!app.prefix_pending);
    }

    #[test]
    fn prefix_then_q_and_esc_leave() {
        for leave in [key('q'), code(KeyCode::Esc)] {
            let (mut app, _, _) = two_agents();
            let _ = app.on_key(code(KeyCode::Enter));
            let _ = app.on_key(ctrl('q'));
            assert!(app.on_key(leave).is_empty(), "{leave:?}");
            assert_eq!(app.focus, Focus::Agents, "{leave:?}");
            assert!(!app.prefix_pending, "{leave:?}");
        }
    }

    #[test]
    fn prefix_twice_sends_one_ctrl_q() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert!(app.on_key(ctrl('q')).is_empty());
        assert_eq!(
            app.on_key(ctrl('q')),
            vec![char_input(a.id, 'q', protocol::Modifiers::CTRL)]
        );
        assert_eq!(app.focus, Focus::Pane);
        assert!(!app.prefix_pending);
        assert_eq!(
            app.on_key(key('j')),
            vec![char_input(a.id, 'j', protocol::Modifiers::NONE)]
        );
    }

    #[test]
    fn prefix_then_unknown_sends_nothing() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        assert!(app.on_key(key('x')).is_empty());
        assert!(!app.prefix_pending);
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("ctrl-q: j/k switch · q sidebar · s scroll · ctrl-q send")
        );
        assert_eq!(
            app.on_key(key('x')),
            vec![char_input(a.id, 'x', protocol::Modifiers::NONE)]
        );
    }

    #[test]
    fn prefix_clears_when_the_agent_exits() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        let exited = AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(exited));
        assert_eq!(app.focus, Focus::Agents);
        assert!(!app.prefix_pending);
        let _ = app.apply_server(ServerMessage::AgentUpdated(a.clone()));
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.on_key(key('j')),
            vec![char_input(a.id, 'j', protocol::Modifiers::NONE)]
        );
    }

    #[test]
    fn ctrl_bracket_still_leaves() {
        for leave in [ctrl(']'), ctrl('5')] {
            let (mut app, _, _) = two_agents();
            let _ = app.on_key(code(KeyCode::Enter));
            assert!(app.on_key(leave).is_empty(), "{leave:?}");
            assert_eq!(app.focus, Focus::Agents, "{leave:?}");
        }
    }

    #[test]
    fn alt_brackets_are_plain_input_by_default() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        for c in ['[', ']'] {
            assert_eq!(
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)),
                vec![char_input(a.id, c, protocol::Modifiers::ALT)]
            );
            assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
            assert_eq!(app.focus, Focus::Pane);
        }
    }

    #[test]
    fn q_quits_from_the_sidebar() {
        let (mut app, _, _) = two_agents();
        assert_eq!(app.on_key(key('q')), vec![Effect::Quit]);
    }

    #[test]
    fn stop_needs_confirmation() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(key('x'));
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                prompt: "Stop alpha?".into(),
                action: Confirmed::Kill(a.id)
            })
        );
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('x'));
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::Send(Request::KillAgent { id: a.id })]
        );
    }

    #[test]
    fn remove_needs_confirmation() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(key('d'));
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::RemoveAgent {
                id: a.id,
                remove_worktree: false,
                force: false,
            })]
        );
    }

    #[test]
    fn big_q_stops_the_daemon_after_confirmation() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('Q'));
        assert_eq!(app.on_key(key('y')), vec![Effect::StopDaemonAndQuit]);
    }

    #[test]
    fn restarting_a_running_agent_needs_confirmation() {
        let (mut app, a, _) = two_agents();
        let _ = app.set_pane_size(100, 30);
        assert!(app.on_key(key('r')).is_empty());
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                prompt: "Restart alpha?".into(),
                action: Confirmed::Restart(a.id)
            })
        );
        assert!(app.on_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('r'));
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::Send(Request::RestartAgent {
                id: a.id,
                cols: 100,
                rows: 30
            })]
        );
    }

    #[test]
    fn restarting_a_stopped_agent_uses_the_pane_size_at_once() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let _ = app.set_pane_size(100, 30);
        assert_eq!(
            app.on_key(key('r')),
            vec![Effect::Send(Request::RestartAgent {
                id: a.id,
                cols: 100,
                rows: 30
            })]
        );
        assert_eq!(app.popup, None);
    }

    #[test]
    fn rename_popup_edits_and_submits() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(key('R'));
        for _ in 0..5 {
            let _ = app.on_key(code(KeyCode::Backspace));
        }
        type_text(&mut app, "omega");
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::RenameAgent {
                id: a.id,
                name: "omega".into()
            })]
        );
    }

    #[test]
    fn add_project_resolves_against_the_cwd() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('a'));
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "repo");
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::AddProject {
                path: "/work/repo".into(),
                name: None
            })]
        );
    }

    #[test]
    fn add_project_expands_a_leading_tilde() {
        for (typed, expected) in [
            ("~", "/home/u"),
            ("~/repo", "/home/u/repo"),
            ("x~", "/work/x~"),
            ("~other/repo", "/work/~other/repo"),
        ] {
            let (mut app, _, _) = two_agents();
            let _ = app.on_key(key('a'));
            let _ = app.on_key(ctrl('u'));
            type_text(&mut app, typed);
            assert_eq!(
                app.on_key(code(KeyCode::Enter)),
                vec![Effect::Send(Request::AddProject {
                    path: expected.into(),
                    name: None
                })],
                "{typed}"
            );
        }
    }

    #[test]
    fn paste_into_a_field_takes_the_first_line_of_any_line_ending() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('R'));
        let _ = app.on_key(ctrl('u'));
        let _ = app.on_paste("one\rtwo".into());
        assert!(
            matches!(&app.popup, Some(Popup::Rename { input, .. }) if input == "one"),
            "{:?}",
            app.popup
        );
        let _ = app.on_key(code(KeyCode::Esc));
        let _ = app.on_key(key('a'));
        let _ = app.on_key(ctrl('u'));
        let _ = app.on_paste("/tmp/x\r\n/tmp/y".into());
        assert!(
            matches!(&app.popup, Some(Popup::AddProject { input }) if input == "/tmp/x"),
            "{:?}",
            app.popup
        );
    }

    fn created(effects: Vec<Effect>) -> CreateAgent {
        match effects.as_slice() {
            [Effect::Send(Request::CreateAgent(c))] => c.clone(),
            other => panic!("unexpected effects {other:?}"),
        }
    }

    #[test]
    fn new_agent_form_creates_each_preset() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "api");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            (c.name.as_str(), c.kind, c.command),
            ("api", AgentKind::Claude, Vec::<String>::new())
        );
        assert_eq!((c.cols, c.rows), (120, 40));

        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "sh1");
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            (c.kind, c.command),
            (AgentKind::Generic, vec!["/bin/sh".to_string()])
        );

        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "job");
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        let _ = app.on_key(code(KeyCode::Right));
        let _ = app.on_key(code(KeyCode::Tab));
        type_text(&mut app, "make test");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            (c.kind, c.command),
            (
                AgentKind::Generic,
                vec!["sh".to_string(), "-c".to_string(), "make test".to_string()]
            )
        );
    }

    #[test]
    fn new_agent_form_validates() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert!(matches!(app.popup, Some(Popup::NewAgent(_))));
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("name the agent first")
        );
    }

    #[test]
    fn new_agent_without_projects_says_so() {
        let (mut app, _) = loaded(vec![], vec![]);
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("add a project first (a)")
        );
    }

    fn submit_shell_agent(app: &mut App, name: &str) -> AgentInfo {
        let project = app.selected_project().map(|p| p.id).expect("project");
        let _ = app.on_key(key('n'));
        type_text(app, name);
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        let _ = created(app.on_key(code(KeyCode::Enter)));
        AgentInfo {
            id: AgentId::random(),
            project,
            name: name.into(),
            kind: AgentKind::Generic,
            command: vec!["/bin/sh".into()],
            cwd: "/p/p".into(),
            status: Status::Starting,
            worktree: None,
            last_error: None,
        }
    }

    #[test]
    fn created_agent_is_selected_and_focused_by_the_create_response() {
        let (mut app, a, _) = two_agents();
        let info = submit_shell_agent(&mut app, "zed");
        let _ = app.apply_server(ServerMessage::AgentUpdated(info.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.focus, Focus::Agents);
        let effects = app.apply_response(Response::Agent(info.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(info.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            effects,
            vec![Effect::Send(Request::Subscribe { id: info.id })]
        );
    }

    #[test]
    fn failed_create_never_selects_a_same_named_agent_later() {
        let (mut app, a, b) = two_agents();
        let _ = app.on_key(key('j'));
        let _ = submit_shell_agent(&mut app, "alpha");
        app.request_failed("an agent named alpha already exists");
        let waiting = AgentInfo {
            status: Status::NeedsInput,
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(waiting.clone()));
        let _ = app.apply_response(Response::Agent(waiting));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("an agent named alpha already exists")
        );
    }

    #[test]
    fn create_response_after_moving_does_not_steal_the_selection_or_focus() {
        let (mut app, _, b) = two_agents();
        let info = submit_shell_agent(&mut app, "zed");
        let _ = app.on_key(key('j'));
        let _ = app.apply_server(ServerMessage::AgentUpdated(info.clone()));
        let _ = app.apply_response(Response::Agent(info));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn removing_the_focused_agent_returns_focus_to_the_sidebar() {
        let (mut app, a, b) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        let effects = app.on_key(key('x'));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Send(Request::Input { .. }))),
            "{effects:?}"
        );
    }

    #[test]
    fn enter_on_a_stopped_agent_stays_in_the_sidebar() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Exited(1));
        let (mut app, _) = loaded(vec![p], vec![a]);
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("alpha is not running — r restarts")
        );
    }

    #[test]
    fn focused_agent_exiting_returns_focus_to_the_sidebar() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        let exited = AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        };
        let effects = app.apply_server(ServerMessage::AgentUpdated(exited));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.subscribed, Some(a.id));
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn g_jumps_to_the_next_agent_needing_input() {
        let p = project("p");
        let q = project("q");
        let waiting = agent(&q, "w", Status::NeedsInput);
        let (mut app, _) = loaded(
            vec![p.clone(), q.clone()],
            vec![agent(&p, "a", Status::Idle), waiting.clone()],
        );
        let _ = app.on_key(key('g'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(waiting.id));
        assert_eq!(app.selected_project().map(|x| x.id), Some(q.id));
    }

    #[test]
    fn paste_goes_to_the_agent_or_the_popup() {
        let (mut app, a, _) = two_agents();
        assert!(app.on_paste("x".into()).is_empty());
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.on_paste("hello\nworld".into()),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Paste("hello\nworld".into())
            })]
        );
        let _ = app.on_key(ctrl(']'));
        let _ = app.on_key(key('n'));
        assert!(app.on_paste("pasted\nignored".into()).is_empty());
        match &app.popup {
            Some(Popup::NewAgent(form)) => assert_eq!(form.name, "pasted"),
            other => panic!("unexpected popup {other:?}"),
        }
    }

    #[test]
    fn wheel_inputs_running_agents_and_scrolls_exited_ones() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        assert_eq!(
            app.on_wheel(true, 3, 4),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Wheel {
                    up: true,
                    col: 3,
                    row: 4
                }
            })]
        );
        let stopped = AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(stopped));
        assert_eq!(
            app.on_wheel(true, 0, 0),
            vec![Effect::Send(Request::Scroll { id: a.id, lines: 3 })]
        );
        assert_eq!(
            app.on_wheel(false, 0, 0),
            vec![Effect::Send(Request::Scroll {
                id: a.id,
                lines: -3
            })]
        );
        let _ = app.on_key(key('?'));
        assert!(app.on_wheel(true, 0, 0).is_empty());
    }

    #[test]
    fn help_closes_on_any_key() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('?'));
        assert_eq!(app.popup, Some(Popup::Help));
        let _ = app.on_key(key('z'));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn big_a_toggles_the_all_agents_view() {
        let p = project("p");
        let q = project("q");
        let a = agent(&p, "a", Status::Idle);
        let b = agent(&q, "b", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone(), q.clone()], vec![a.clone(), b.clone()]);
        let _ = app.on_key(key('A'));
        assert_eq!(app.view, AgentView::All);
        assert_eq!(app.visible_agents().len(), 2);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.selected_project().map(|x| x.id), Some(q.id));
        let _ = app.on_key(key('n'));
        type_text(&mut app, "new");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(c.project, q.id);
        let _ = app.on_key(key('A'));
        assert_eq!(app.view, AgentView::Project);
    }

    fn all_view_with_three_projects() -> (App, Vec<Project>, AgentInfo, AgentInfo) {
        let p = project("p");
        let q = project("q");
        let r = project("r");
        let a = agent(&p, "a", Status::Idle);
        let b = agent(&q, "b", Status::Idle);
        let (mut app, _) = loaded(
            vec![p.clone(), q.clone(), r.clone()],
            vec![a.clone(), b.clone()],
        );
        let _ = app.on_key(key('A'));
        (app, vec![p, q, r], a, b)
    }

    #[test]
    fn all_view_project_highlight_selects_its_first_agent_or_keeps_the_current() {
        let (mut app, projects, _, b) = all_view_with_three_projects();
        let _ = app.on_key(key('h'));
        let effects = app.on_key(key('j'));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[1].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        let effects = app.on_key(key('j'));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[2].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert!(effects.is_empty(), "{effects:?}");
        let _ = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[2].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn all_view_added_project_keeps_the_current_agent_unless_it_has_agents() {
        let (mut app, projects, a, b) = all_view_with_three_projects();
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        let s = project("s");
        let effects = app.apply_response(Response::Project(s.clone()));
        assert_eq!(app.selected_project().map(|x| x.id), Some(s.id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert!(effects.is_empty(), "{effects:?}");
        let _ = app.apply_response(Response::Project(projects[0].clone()));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[0].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
    }

    #[test]
    fn all_view_removing_the_highlighted_project_snaps_to_the_selected_agent() {
        let (mut app, projects, a, _) = all_view_with_three_projects();
        let _ = app.on_key(key('h'));
        let _ = app.on_key(key('k'));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[2].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        let effects = app.apply_server(ServerMessage::ProjectsChanged(vec![
            projects[0].clone(),
            projects[1].clone(),
        ]));
        assert_eq!(app.selected_project().map(|x| x.id), Some(projects[0].id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn all_view_removing_the_highlighted_project_without_agents_clamps() {
        let p = project("p");
        let q = project("q");
        let r = project("r");
        let (mut app, _) = loaded(vec![p.clone(), q.clone(), r], vec![]);
        let _ = app.on_key(key('A'));
        let _ = app.on_key(key('h'));
        let _ = app.on_key(key('k'));
        assert_eq!(app.project_index, 2);
        let _ = app.apply_server(ServerMessage::ProjectsChanged(vec![p, q.clone()]));
        assert_eq!(app.selected_project().map(|x| x.id), Some(q.id));
    }

    #[test]
    fn too_small_terminal_ignores_keys_except_the_way_out() {
        let (mut app, a, _) = two_agents();
        let _ = app.set_pane_size(10, 3);
        for event in [key('x'), key('n'), key('?'), key('j'), code(KeyCode::Enter)] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!(app.popup, None, "{event:?}");
        }
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.on_key(key('q')), vec![Effect::Quit]);
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);

        let _ = app.set_pane_size(80, 20);
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        let _ = app.set_pane_size(10, 3);
        assert!(app.on_paste("text".into()).is_empty());
        for event in [key('q'), ctrl('c'), key('x'), ctrl('q'), key('x'), key('q')] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!(app.focus, Focus::Pane, "{event:?}");
        }
        assert!(app.on_key(ctrl('q')).is_empty());
        assert!(app.on_key(key('q')).is_empty());
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.popup, None);
    }

    #[test]
    fn key_releases_are_ignored() {
        let (mut app, _, _) = two_agents();
        let mut release = key('q');
        release.kind = KeyEventKind::Release;
        assert!(app.on_key(release).is_empty());
    }
}
