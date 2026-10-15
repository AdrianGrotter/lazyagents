use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use protocol::{
    AgentId, AgentInfo, AgentKind, CreateAgent, GitStat, InputEvent, Project, ProjectId, Request,
    Response, Screen, ServerMessage, Status, WorktreeRequest,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::config::{Config, PresetDef, Theme};
use crate::keymap::{Action, Keymap};
use crate::keys;

pub const MESSAGE_TTL: Duration = Duration::from_secs(5);
pub const MIN_PANE: (u16, u16) = (20, 5);
pub const STAT_INTERVAL: Duration = Duration::from_secs(5);
pub const DIFF_LINE_CAP: usize = 20_000;
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const DIRTY_WORKTREE: &str = "worktree has uncommitted changes";
const DIRTY_FILES_SHOWN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Agents,
    Pane,
    Diff,
    Scroll,
}

/// `Configured(i)` is the i-th preset from the config file; they follow the built-ins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Claude,
    Shell,
    Custom,
    Configured(usize),
}

const BUILTIN_PRESETS: [Preset; 3] = [Preset::Claude, Preset::Shell, Preset::Custom];

impl Preset {
    pub fn all(configured: usize) -> Vec<Preset> {
        BUILTIN_PRESETS
            .into_iter()
            .chain((0..configured).map(Preset::Configured))
            .collect()
    }

    pub fn label(self, presets: &[PresetDef]) -> &str {
        match self {
            Preset::Claude => "claude",
            Preset::Shell => "shell",
            Preset::Custom => "custom",
            Preset::Configured(index) => presets.get(index).map_or("?", |p| p.name.as_str()),
        }
    }

    pub fn next(self, configured: usize) -> Self {
        self.step(1, configured)
    }

    pub fn previous(self, configured: usize) -> Self {
        self.step(-1, configured)
    }

    fn step(self, step: i32, configured: usize) -> Self {
        let all = Preset::all(configured);
        let index = all.iter().position(|p| *p == self).unwrap_or(0);
        all[wrap(index, step, all.len())]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Preset,
    Command,
    Worktree,
    Base,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentForm {
    pub name: String,
    pub preset: Preset,
    pub command: String,
    pub worktree: bool,
    pub base: String,
    pub field: Field,
}

impl NewAgentForm {
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Name, Field::Preset];
        if self.preset == Preset::Custom {
            fields.push(Field::Command);
        }
        fields.push(Field::Worktree);
        if self.worktree {
            fields.push(Field::Base);
        }
        fields
    }

    fn step_field(&mut self, step: i32) {
        let fields = self.fields();
        let index = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[wrap(index, step, fields.len())];
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    Kill(AgentId),
    Restart(AgentId),
    Remove(AgentId),
    RemoveWorktree(AgentId),
    ForceRemove(AgentId),
    StopDaemon,
}

impl Confirmed {
    /// Prompts for these actions name their own keys.
    pub fn names_its_keys(&self) -> bool {
        matches!(
            self,
            Confirmed::RemoveWorktree(_) | Confirmed::ForceRemove(_)
        )
    }
}

/// `lines` is `None` until the daemon's diff arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub agent: AgentId,
    pub lines: Option<Vec<String>>,
    pub scroll: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMode {
    pub agent: AgentId,
    pub from_pane: bool,
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
    pub git_stats: HashMap<AgentId, GitStat>,
    pub diff: Option<DiffView>,
    pub scroll: Option<ScrollMode>,
    stats_in_flight: HashMap<AgentId, Instant>,
    diffs_in_flight: HashMap<AgentId, Instant>,
    stat_polled: Option<(AgentId, Instant)>,
    pending_remove: Option<AgentId>,
    pending_select: Option<PendingCreate>,
    interactions: u64,
    subscribed_running: bool,
    too_small: bool,
    pub prefix_pending: bool,
    pub keymap: Keymap,
    pub presets: Vec<PresetDef>,
    pub theme: Theme,
    cwd: PathBuf,
    home: Option<PathBuf>,
    shell: String,
}

impl App {
    pub fn new(cwd: PathBuf, shell: String, home: Option<PathBuf>, config: Config) -> Self {
        let message =
            (!config.warnings.is_empty()).then(|| (config.warnings.join("; "), Instant::now()));
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
            message,
            git_stats: HashMap::new(),
            diff: None,
            scroll: None,
            stats_in_flight: HashMap::new(),
            diffs_in_flight: HashMap::new(),
            stat_polled: None,
            pending_remove: None,
            pending_select: None,
            interactions: 0,
            subscribed_running: false,
            too_small: false,
            prefix_pending: false,
            keymap: config.keymap,
            presets: config.presets,
            theme: config.theme,
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
        let message = message.into();
        self.pending_select = None;
        if let Some(id) = self.pending_remove.take()
            && let Some(files) = message.strip_prefix(DIRTY_WORKTREE)
            && self.popup.is_none()
            && self.agents.iter().any(|a| a.id == id)
        {
            self.popup = Some(Popup::Confirm {
                prompt: force_prompt(files),
                action: Confirmed::ForceRemove(id),
            });
            return;
        }
        self.fail(message);
    }

    /// Asks for the selected agent's git stat when the selection changed, and then every
    /// `STAT_INTERVAL`, with at most one request per agent in flight. A git request
    /// unanswered for `GIT_TIMEOUT` no longer counts as in flight.
    pub fn on_tick(&mut self, now: Instant) -> Vec<Effect> {
        let fresh = |sent: &mut Instant| now.saturating_duration_since(*sent) < GIT_TIMEOUT;
        self.stats_in_flight.retain(|_, sent| fresh(sent));
        self.diffs_in_flight.retain(|_, sent| fresh(sent));
        let Some(id) = self.selected_agent().map(|a| a.id) else {
            self.stat_polled = None;
            return Vec::new();
        };
        let due = match self.stat_polled {
            Some((polled, at)) if polled == id => {
                now.saturating_duration_since(at) >= STAT_INTERVAL
            }
            _ => true,
        };
        if !due {
            return Vec::new();
        }
        self.stat_polled = Some((id, now));
        if self.stats_in_flight.contains_key(&id) {
            return Vec::new();
        }
        self.stats_in_flight.insert(id, now);
        vec![Effect::Send(Request::GitStat { id })]
    }

    /// An error means no stat to show, such as a cwd outside any git repository.
    pub fn apply_git_stat(&mut self, id: AgentId, result: Result<GitStat, String>) {
        self.stats_in_flight.remove(&id);
        if !self.agents.iter().any(|a| a.id == id) {
            return;
        }
        match result {
            Ok(stat) => {
                self.git_stats.insert(id, stat);
            }
            Err(_) => {
                self.git_stats.remove(&id);
            }
        }
    }

    pub fn apply_diff(&mut self, id: AgentId, result: Result<String, String>) {
        self.diffs_in_flight.remove(&id);
        let Some(view) = self
            .diff
            .as_mut()
            .filter(|view| view.agent == id && view.lines.is_none())
        else {
            return;
        };
        match result {
            Ok(text) => view.lines = Some(diff_lines(&text)),
            Err(message) => {
                self.close_diff();
                self.fail(message);
            }
        }
    }

    fn close_diff(&mut self) {
        self.diff = None;
        self.focus = Focus::Agents;
    }

    fn forget_gone_agents(&mut self) {
        let exists = |id: &AgentId| self.agents.iter().any(|a| a.id == *id);
        self.git_stats.retain(|id, _| exists(id));
        self.stats_in_flight.retain(|id, _| exists(id));
        self.diffs_in_flight.retain(|id, _| exists(id));
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
        self.pending_remove = None;
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
        self.forget_gone_agents();
        let selected = self.selected_agent().map(|a| a.id);
        if self.focus == Focus::Pane && selected != before {
            self.leave_pane();
        }
        if self.focus == Focus::Diff && selected != self.diff.as_ref().map(|d| d.agent) {
            self.close_diff();
        }
        if self.focus == Focus::Scroll && selected != self.scroll.map(|s| s.agent) {
            self.scroll = None;
            self.focus = Focus::Agents;
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
const SCROLL_TOP: i32 = i32::MAX / 2;
const SCROLL_BOTTOM: i32 = i32::MIN / 2;

fn scroll_request(id: AgentId, lines: i32) -> Effect {
    Effect::Send(Request::Scroll { id, lines })
}

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
            Focus::Diff => self.on_diff_key(key),
            Focus::Scroll => self.on_scroll_key(key),
            Focus::Projects | Focus::Agents => self.on_sidebar_key(key),
        }
    }

    fn closes_diff(&self, key: &KeyEvent) -> bool {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let fixed = matches!(key.code, KeyCode::Char('q' | 'D') | KeyCode::Esc);
        (plain && fixed) || self.keymap.sidebar(key) == Some(Action::Diff)
    }

    fn diff_page(&self) -> usize {
        usize::from(self.pane_size.map_or(DEFAULT_SIZE.1, |(_, rows)| rows)).max(1)
    }

    /// Closes the view or quits; `None` for any other key.
    fn leave_diff(&mut self, key: &KeyEvent) -> Option<Vec<Effect>> {
        if self.closes_diff(key) {
            self.close_diff();
            return Some(Vec::new());
        }
        (self.keymap.sidebar(key) == Some(Action::Quit)).then(|| vec![Effect::Quit])
    }

    fn on_diff_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.leave_diff(&key) {
            return effects;
        }
        let page = self.diff_page() as i64;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let step = match (key.code, ctrl) {
            (KeyCode::Char('j') | KeyCode::Down, false) => 1,
            (KeyCode::Char('k') | KeyCode::Up, false) => -1,
            (KeyCode::Char('d'), true) => page / 2,
            (KeyCode::Char('u'), true) => -page / 2,
            (KeyCode::PageDown, _) => page,
            (KeyCode::PageUp, _) => -page,
            (KeyCode::Char('g'), false) => i64::MIN / 2,
            (KeyCode::Char('G'), false) => i64::MAX / 2,
            _ => return Vec::new(),
        };
        self.scroll_diff(step);
        Vec::new()
    }

    fn scroll_diff(&mut self, step: i64) {
        let page = self.diff_page();
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        let last = view
            .lines
            .as_ref()
            .map_or(0, |lines| lines.len().saturating_sub(page)) as i64;
        view.scroll = (view.scroll as i64).saturating_add(step).clamp(0, last) as usize;
    }

    /// Leaves scroll mode, returning to the pane only if it was entered from there and
    /// the agent still runs, and scrolls the agent back to the bottom.
    fn exit_scroll(&mut self) -> Vec<Effect> {
        let Some(mode) = self.scroll.take() else {
            self.focus = Focus::Agents;
            return Vec::new();
        };
        let running = self
            .agents
            .iter()
            .any(|a| a.id == mode.agent && a.status.is_running());
        self.focus = if mode.from_pane && running {
            Focus::Pane
        } else {
            Focus::Agents
        };
        self.prefix_pending = false;
        vec![scroll_request(mode.agent, SCROLL_BOTTOM)]
    }

    fn enter_scroll(&mut self, from_pane: bool) {
        match self.selected_agent() {
            Some(agent) if agent.status != Status::Stopped => {
                self.scroll = Some(ScrollMode {
                    agent: agent.id,
                    from_pane,
                });
                self.focus = Focus::Scroll;
                self.prefix_pending = false;
            }
            Some(_) => self.fail("nothing to scroll"),
            None => {}
        }
    }

    /// Exits or quits; `None` for any other key.
    fn leave_scroll(&mut self, key: &KeyEvent) -> Option<Vec<Effect>> {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if plain && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
            return Some(self.exit_scroll());
        }
        (self.keymap.sidebar(key) == Some(Action::Quit)).then(|| vec![Effect::Quit])
    }

    fn on_scroll_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.leave_scroll(&key) {
            return effects;
        }
        let Some(mode) = self.scroll else {
            return Vec::new();
        };
        let page = self.diff_page() as i32;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let lines = match (key.code, ctrl) {
            (KeyCode::Char('j'), false) => -1,
            (KeyCode::Char('k'), false) => 1,
            (KeyCode::Char('d'), true) => -(page / 2),
            (KeyCode::Char('u'), true) => page / 2,
            (KeyCode::PageDown, _) => -page,
            (KeyCode::PageUp, _) => page,
            (KeyCode::Char('g'), false) => SCROLL_TOP,
            (KeyCode::Char('G'), false) => SCROLL_BOTTOM,
            _ => return Vec::new(),
        };
        vec![scroll_request(mode.agent, lines)]
    }

    /// Nothing is visible, so only the quit keys (from the sidebar) and the prefix
    /// followed by a leave key (leave the pane) act; no key reaches an agent.
    fn on_too_small_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.focus == Focus::Diff {
            return self.leave_diff(&key).unwrap_or_default();
        }
        if self.focus == Focus::Scroll {
            return self.leave_scroll(&key).unwrap_or_default();
        }
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
        match self.keymap.sidebar(&key) {
            Some(Action::Quit) => vec![Effect::Quit],
            _ => Vec::new(),
        }
    }

    pub fn on_paste(&mut self, text: String) -> Vec<Effect> {
        if self.too_small {
            return Vec::new();
        }
        if self.popup.is_none() {
            self.prefix_pending = false;
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
                Field::Base => form.base.push_str(&line),
                Field::Preset | Field::Worktree => {}
            },
            Some(Popup::Help | Popup::Confirm { .. }) | None => {}
        }
        Vec::new()
    }

    pub fn on_wheel(&mut self, up: bool, col: u16, row: u16) -> Vec<Effect> {
        if self.popup.is_some() {
            return Vec::new();
        }
        if self.focus == Focus::Diff {
            self.scroll_diff(if up {
                -i64::from(WHEEL_LINES)
            } else {
                i64::from(WHEEL_LINES)
            });
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
                Some(Action::PaneScroll) => {
                    self.enter_scroll(true);
                    Vec::new()
                }
                Some(Action::PaneSendPrefix) => self.input(id, &self.keymap.prefix().event()),
                Some(_) => Vec::new(),
                None => {
                    self.fail(prefix_help(&self.keymap));
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
                    let message = format!(
                        "{} is not running — {} restarts",
                        agent.name,
                        self.keymap.first_key(Action::Restart)
                    );
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
            Action::Remove => match self.selected_agent() {
                Some(agent) if agent.worktree.is_some() => {
                    self.popup = Some(Popup::Confirm {
                        prompt: format!(
                            "Remove {}? y: agent + worktree · k: keep worktree · n: cancel",
                            agent.name
                        ),
                        action: Confirmed::RemoveWorktree(agent.id),
                    });
                }
                _ => self.confirm_for_selected("Remove", Confirmed::Remove),
            },
            Action::Diff => {
                if let Some(id) = self.selected_agent().map(|a| a.id) {
                    self.diff = Some(DiffView {
                        agent: id,
                        lines: None,
                        scroll: 0,
                    });
                    self.focus = Focus::Diff;
                    if self.diffs_in_flight.contains_key(&id) {
                        return Vec::new();
                    }
                    self.diffs_in_flight.insert(id, Instant::now());
                    return vec![Effect::Send(Request::GitDiff { id })];
                }
            }
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
            Action::Scroll => self.enter_scroll(false),
            Action::PaneNext
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
            Focus::Agents | Focus::Pane | Focus::Diff | Focus::Scroll => self.move_agent(step),
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
            let add = self.keymap.first_key(Action::AddProject);
            self.fail(format!("add a project first ({add})"));
            return;
        }
        self.popup = Some(Popup::NewAgent(NewAgentForm {
            name: String::new(),
            preset: Preset::Claude,
            command: String::new(),
            worktree: false,
            base: String::new(),
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
            Popup::Confirm { prompt, action } => match self.on_confirm_key(&action, &key) {
                Some(effects) => effects,
                None => {
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

    /// `None` keeps the prompt open.
    fn on_confirm_key(&mut self, action: &Confirmed, key: &KeyEvent) -> Option<Vec<Effect>> {
        let effects = match (action, key.code) {
            (_, KeyCode::Char('n') | KeyCode::Esc) => Vec::new(),
            (Confirmed::RemoveWorktree(id), KeyCode::Char('y')) => self.remove(*id, true, false),
            (Confirmed::RemoveWorktree(id), KeyCode::Char('k')) => self.remove(*id, false, false),
            (Confirmed::ForceRemove(id), KeyCode::Char('y')) => self.remove(*id, true, true),
            (Confirmed::RemoveWorktree(_) | Confirmed::ForceRemove(_), _) => return None,
            (Confirmed::Kill(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::Send(Request::KillAgent { id: *id })]
            }
            (Confirmed::Restart(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![self.restart(*id)]
            }
            (Confirmed::Remove(id), KeyCode::Char('y') | KeyCode::Enter) => {
                self.remove(*id, false, false)
            }
            (Confirmed::StopDaemon, KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::StopDaemonAndQuit]
            }
            _ => return None,
        };
        Some(effects)
    }

    /// Remembers a remove that the daemon may refuse for a dirty worktree, so the refusal
    /// can offer to force it.
    fn remove(&mut self, id: AgentId, remove_worktree: bool, force: bool) -> Vec<Effect> {
        self.pending_remove = (remove_worktree && !force).then_some(id);
        vec![Effect::Send(Request::RemoveAgent {
            id,
            remove_worktree,
            force,
        })]
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
            KeyCode::Tab | KeyCode::Down => form.step_field(1),
            KeyCode::BackTab | KeyCode::Up => form.step_field(-1),
            KeyCode::Left if form.field == Field::Preset => {
                form.preset = form.preset.previous(self.presets.len())
            }
            KeyCode::Right | KeyCode::Char(' ') if form.field == Field::Preset => {
                form.preset = form.preset.next(self.presets.len())
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if form.field == Field::Worktree =>
            {
                form.worktree = !form.worktree
            }
            _ => {
                let target = match form.field {
                    Field::Name => Some(&mut form.name),
                    Field::Command => Some(&mut form.command),
                    Field::Base => Some(&mut form.base),
                    Field::Preset | Field::Worktree => None,
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
            Preset::Configured(index) => match self.presets.get(index) {
                Some(preset) => (preset.kind, preset.command.clone()),
                None => return Vec::new(),
            },
        };
        let worktree = form.worktree.then(|| {
            let base = form.base.trim();
            WorktreeRequest {
                base: (!base.is_empty()).then(|| base.to_string()),
            }
        });
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
            worktree,
        }))]
    }
}

fn prefix_help(keymap: &Keymap) -> String {
    let keys = |actions: &[Action]| {
        let keys: Vec<_> = actions.iter().flat_map(|a| keymap.keys(*a)).collect();
        crate::keymap::join(&keys)
    };
    let parts: Vec<String> = [
        (keys(&[Action::PaneNext, Action::PanePrevious]), "switch"),
        (keys(&[Action::PaneLeave]), "sidebar"),
        (keys(&[Action::PaneScroll]), "scroll"),
        (keys(&[Action::PaneSendPrefix]), "send"),
    ]
    .into_iter()
    .filter(|(keys, _)| !keys.is_empty())
    .map(|(keys, label)| format!("{keys} {label}"))
    .collect();
    format!("{}: {}", keymap.prefix(), parts.join(" · "))
}

fn wrap(index: usize, step: i32, count: usize) -> usize {
    (index as i64 + i64::from(step)).rem_euclid(count as i64) as usize
}

/// `files` is the daemon's refusal after `DIRTY_WORKTREE`: a colon, then one path per line.
fn force_prompt(files: &str) -> String {
    let files: Vec<&str> = files
        .strip_prefix(':')
        .unwrap_or(files)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut lines = vec!["Worktree has uncommitted changes:".to_string()];
    lines.extend(files.iter().take(DIRTY_FILES_SHOWN).map(|f| printable(f)));
    if files.len() > DIRTY_FILES_SHOWN {
        lines.push(format!("… and {} more", files.len() - DIRTY_FILES_SHOWN));
    }
    lines.push("Remove anyway? y/n".to_string());
    lines.join("\n")
}

/// Tabs become spaces and other control characters are dropped, so text from files
/// cannot move the cursor or restyle the terminal.
fn printable(line: &str) -> String {
    line.replace('\t', "    ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// Splits a diff into printable display lines, at most `DIFF_LINE_CAP` plus a truncation
/// notice.
fn diff_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().take(DIFF_LINE_CAP).map(printable).collect();
    let rest = text.lines().skip(DIFF_LINE_CAP).count();
    if rest > 0 {
        lines.push(format!("… diff truncated ({rest} more lines)"));
    }
    lines
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
        loaded_with(Config::default(), projects, agents)
    }

    pub(crate) fn loaded_with(
        config: Config,
        projects: Vec<Project>,
        agents: Vec<AgentInfo>,
    ) -> (App, Vec<Effect>) {
        let mut app = App::new(
            "/work".into(),
            "/bin/sh".into(),
            Some("/home/u".into()),
            config,
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
            Config::default(),
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
            Config::default(),
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
    fn arrows_switch_boxes_and_move_within_the_list() {
        let p = project("p");
        let q = project("q");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone(), q.clone()], vec![a.clone(), b.clone()]);

        let _ = app.on_key(code(KeyCode::Left));
        assert_eq!(app.focus, Focus::Projects);
        let _ = app.on_key(code(KeyCode::Down));
        assert_eq!(
            (app.focus, app.selected_project().map(|x| x.id)),
            (Focus::Projects, Some(q.id))
        );
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(
            (app.focus, app.selected_project().map(|x| x.id)),
            (Focus::Projects, Some(p.id))
        );

        let _ = app.on_key(code(KeyCode::Right));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        let _ = app.on_key(code(KeyCode::Down));
        assert_eq!(
            (app.focus, app.selected_agent().map(|x| x.id)),
            (Focus::Agents, Some(b.id))
        );
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(
            (app.focus, app.selected_agent().map(|x| x.id)),
            (Focus::Agents, Some(a.id))
        );
        assert_eq!(app.selected_project().map(|x| x.id), Some(p.id));
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
            Some("ctrl-q: j/k switch · q/esc sidebar · s scroll · ctrl-q send")
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
    fn bare_esc_and_ctrl_c_reach_the_agent() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.on_key(code(KeyCode::Esc)),
            vec![input_key(
                a.id,
                protocol::Key::new(protocol::KeyCode::Esc, protocol::Modifiers::NONE)
            )]
        );
        assert_eq!(
            app.on_key(ctrl('c')),
            vec![char_input(a.id, 'c', protocol::Modifiers::CTRL)]
        );
        assert_eq!(app.focus, Focus::Pane);
        assert!(!app.prefix_pending);
    }

    #[test]
    fn paste_with_the_prefix_pending_goes_to_the_agent_and_clears_it() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        assert_eq!(
            app.on_paste("text".into()),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Paste("text".into())
            })]
        );
        assert!(!app.prefix_pending);
        assert_eq!(
            app.on_key(key('j')),
            vec![char_input(a.id, 'j', protocol::Modifiers::NONE)]
        );
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn ctrl_shift_q_reaches_the_agent() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('q'), ctrl_shift)),
            vec![char_input(
                a.id,
                'q',
                protocol::Modifiers {
                    shift: true,
                    alt: false,
                    ctrl: true
                }
            )]
        );
        assert!(!app.prefix_pending);
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

    fn configured(text: &str) -> Config {
        crate::config::parse(text).expect("config parses")
    }

    #[test]
    fn custom_preset_creates_with_its_command() {
        let config = configured(
            r#"
            [[presets]]
            name = "codex"
            command = ["codex", "--full-auto"]
            "#,
        );
        let (mut app, _) = loaded_with(config, vec![project("p")], vec![]);
        let _ = app.on_key(key('n'));
        type_text(&mut app, "cx");
        let _ = app.on_key(code(KeyCode::Tab));
        let preset = |app: &App| match &app.popup {
            Some(Popup::NewAgent(form)) => form.preset,
            other => panic!("no form: {other:?}"),
        };
        let _ = app.on_key(code(KeyCode::Left));
        assert_eq!(preset(&app), Preset::Configured(0));
        let _ = app.on_key(code(KeyCode::Right));
        assert_eq!(preset(&app), Preset::Claude);
        for _ in 0..3 {
            let _ = app.on_key(code(KeyCode::Right));
        }
        assert_eq!(preset(&app), Preset::Configured(0));
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            (c.name.as_str(), c.kind, c.command),
            (
                "cx",
                AgentKind::Generic,
                vec!["codex".to_string(), "--full-auto".to_string()]
            )
        );
    }

    #[test]
    fn config_warnings_become_the_startup_message() {
        let config = Config {
            warnings: vec!["unknown table [x]".into(), "theme.accent: bad".into()],
            ..Config::default()
        };
        let app = App::new("/w".into(), "/bin/sh".into(), None, config);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("unknown table [x]; theme.accent: bad")
        );
        let app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        assert_eq!(app.message, None);
    }

    #[test]
    fn prefix_help_reflects_configured_keys() {
        let config = configured(
            r#"
            [keys.pane]
            prefix = "ctrl-a"
            next = "n"
            previous = "p"
            leave = ["x"]
            "#,
        );
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded_with(config, vec![p], vec![a]);
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('a'));
        assert!(app.on_key(key('z')).is_empty());
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("ctrl-a: n/p switch · x sidebar · s scroll · ctrl-a send")
        );
    }

    #[test]
    fn quit_while_too_small_follows_the_keymap() {
        let config = configured("[keys.sidebar]\nquit = \"ctrl-x\"\n");
        let (mut app, _) = loaded_with(config, vec![project("p")], vec![]);
        let _ = app.set_pane_size(5, 2);
        assert!(app.on_key(key('q')).is_empty());
        assert_eq!(app.on_key(ctrl('x')), vec![Effect::Quit]);
    }

    #[test]
    fn hints_in_messages_name_the_configured_keys() {
        let config = configured(
            r#"
            [keys.sidebar]
            restart = "ctrl-r"
            add_project = "P"
            "#,
        );
        let (mut app, _) = loaded_with(config, vec![], vec![]);
        let _ = app.on_key(key('n'));
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("add a project first (P)")
        );
        let config = configured("[keys.sidebar]\nrestart = \"ctrl-r\"\n");
        let p = project("p");
        let (mut app, _) = loaded_with(
            config,
            vec![p.clone()],
            vec![agent(&p, "a", Status::Stopped)],
        );
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.message.as_ref().map(|(m, _)| m.as_str()),
            Some("a is not running — ctrl-r restarts")
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

    pub(crate) fn worktree_agent(project: &Project, name: &str) -> AgentInfo {
        AgentInfo {
            cwd: format!("/s/worktrees/{name}").into(),
            worktree: Some(protocol::WorktreeInfo {
                path: format!("/s/worktrees/{name}").into(),
                branch: format!("lazyagents/{name}"),
                base: "main".into(),
            }),
            ..agent(project, name, Status::Idle)
        }
    }

    fn form(app: &App) -> NewAgentForm {
        match &app.popup {
            Some(Popup::NewAgent(form)) => form.clone(),
            other => panic!("no form: {other:?}"),
        }
    }

    #[test]
    fn form_worktree_toggle_and_base_field() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let mut order = vec![form(&app).field];
        for _ in 0..3 {
            let _ = app.on_key(code(KeyCode::Tab));
            order.push(form(&app).field);
        }
        assert_eq!(
            order,
            [Field::Name, Field::Preset, Field::Worktree, Field::Name]
        );
        let _ = app.on_key(code(KeyCode::BackTab));
        assert_eq!(form(&app).field, Field::Worktree);
        for (event, on) in [
            (key(' '), true),
            (code(KeyCode::Left), false),
            (code(KeyCode::Right), true),
        ] {
            let _ = app.on_key(event);
            assert_eq!(form(&app).worktree, on, "{event:?}");
        }
        let _ = app.on_key(key('j'));
        assert!(form(&app).worktree);
        let _ = app.on_key(code(KeyCode::Tab));
        assert_eq!(form(&app).field, Field::Base);
        type_text(&mut app, "dev");
        let _ = app.on_paste("-x\nignored".into());
        assert_eq!(form(&app).base, "dev-x");
        let _ = app.on_key(code(KeyCode::Tab));
        assert_eq!(form(&app).field, Field::Name);
        let _ = app.on_key(code(KeyCode::BackTab));
        assert_eq!(form(&app).field, Field::Base);

        let _ = app.on_key(code(KeyCode::Up));
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(form(&app).field, Field::Preset);
        let _ = app.on_key(code(KeyCode::Left));
        assert_eq!(form(&app).preset, Preset::Custom);
        let mut order = vec![form(&app).field];
        for _ in 0..5 {
            let _ = app.on_key(code(KeyCode::Down));
            order.push(form(&app).field);
        }
        assert_eq!(
            order,
            [
                Field::Preset,
                Field::Command,
                Field::Worktree,
                Field::Base,
                Field::Name,
                Field::Preset
            ]
        );
    }

    #[test]
    fn form_sends_worktree_request_when_on() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "wt");
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(key(' '));
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            (c.kind, c.worktree),
            (
                AgentKind::Generic,
                Some(protocol::WorktreeRequest { base: None })
            )
        );

        for (typed, base) in [(" dev ", Some("dev".to_string())), ("  ", None)] {
            let (mut app, _, _) = two_agents();
            let _ = app.on_key(key('n'));
            type_text(&mut app, "wt");
            let _ = app.on_key(code(KeyCode::Up));
            let _ = app.on_key(key(' '));
            let _ = app.on_key(code(KeyCode::Down));
            type_text(&mut app, typed);
            let c = created(app.on_key(code(KeyCode::Enter)));
            assert_eq!(c.worktree, Some(protocol::WorktreeRequest { base }));
        }
    }

    #[test]
    fn form_worktree_is_off_by_default() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        assert!(!form(&app).worktree);
        assert_eq!(form(&app).base, "");
        type_text(&mut app, "api");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(c.worktree, None);
    }

    fn stat(insertions: u32) -> GitStat {
        GitStat {
            insertions,
            deletions: 1,
            files: 2,
            uncommitted: 3,
        }
    }

    #[test]
    fn stat_requested_on_select_and_every_5s() {
        let (mut app, a, b) = two_agents();
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let ask = |id| vec![Effect::Send(Request::GitStat { id })];
        assert_eq!(app.on_tick(t0), ask(a.id));
        assert!(app.on_tick(at(1)).is_empty());
        app.apply_git_stat(a.id, Ok(stat(5)));
        assert_eq!(app.git_stats.get(&a.id), Some(&stat(5)));
        assert!(app.on_tick(at(4)).is_empty());
        assert_eq!(app.on_tick(at(5)), ask(a.id));
        assert!(app.on_tick(at(11)).is_empty(), "one request in flight");

        let _ = app.on_key(key('j'));
        assert_eq!(app.on_tick(at(11)), ask(b.id));
        let _ = app.on_key(key('k'));
        assert!(app.on_tick(at(12)).is_empty(), "a is still in flight");
        app.apply_git_stat(a.id, Ok(stat(7)));
        assert_eq!(app.git_stats.get(&a.id), Some(&stat(7)));
        assert_eq!(app.git_stats.get(&b.id), None);

        app.apply_git_stat(a.id, Err("not a git repository".into()));
        assert_eq!(app.git_stats.get(&a.id), None);
        assert_eq!(app.message, None);

        app.apply_git_stat(b.id, Ok(stat(1)));
        let _ = app.apply_server(ServerMessage::AgentRemoved(b.id));
        assert_eq!(app.git_stats.get(&b.id), None);
        app.apply_git_stat(b.id, Ok(stat(1)));
        assert_eq!(app.git_stats.get(&b.id), None);

        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert!(app.on_tick(at(16)).is_empty(), "polled a at 12");
        assert_eq!(app.on_tick(at(17)), ask(a.id));
    }

    #[test]
    fn unanswered_stat_is_given_up_after_30s() {
        let (mut app, a, _) = two_agents();
        let t0 = Instant::now();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let ask = vec![Effect::Send(Request::GitStat { id: a.id })];
        assert_eq!(app.on_tick(t0), ask);
        for secs in [5, 10, 15, 20, 25, 29] {
            assert!(app.on_tick(at(secs)).is_empty(), "{secs}s");
        }
        assert_eq!(app.on_tick(at(30)), ask);
        assert!(app.on_tick(at(35)).is_empty(), "the retry is in flight");
        app.apply_git_stat(a.id, Ok(stat(2)));
        assert_eq!(app.git_stats.get(&a.id), Some(&stat(2)));
    }

    #[test]
    fn force_prompt_strips_control_characters_from_file_names() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt]);
        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        app.request_failed("worktree has uncommitted changes:\nevil\u{1b}[2J\u{7}.txt\ntab\there");
        let Some(Popup::Confirm { prompt, .. }) = &app.popup else {
            panic!("no confirm: {:?}", app.popup);
        };
        assert_eq!(
            prompt,
            "Worktree has uncommitted changes:\nevil[2J.txt\ntab    here\nRemove anyway? y/n"
        );
    }

    #[test]
    fn remove_worktree_agent_offers_keep_or_remove() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt.clone()]);
        let remove = |remove_worktree| {
            vec![Effect::Send(Request::RemoveAgent {
                id: wt.id,
                remove_worktree,
                force: false,
            })]
        };
        let _ = app.on_key(key('d'));
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                prompt: "Remove wt? y: agent + worktree · k: keep worktree · n: cancel".into(),
                action: Confirmed::RemoveWorktree(wt.id),
            })
        );
        assert!(app.on_key(key('x')).is_empty());
        assert!(app.popup.is_some());
        assert_eq!(app.on_key(key('y')), remove(true));
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('d'));
        assert_eq!(app.on_key(key('k')), remove(false));
        let _ = app.on_key(key('d'));
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('d'));
        assert!(app.on_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.popup, None);
    }

    #[test]
    fn dirty_worktree_error_asks_to_force() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt.clone()]);
        let dirty = |count: usize| {
            let files: Vec<String> = (1..=count).map(|i| format!("f{i}.txt")).collect();
            format!("worktree has uncommitted changes:\n{}", files.join("\n"))
        };

        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        app.request_failed(dirty(2));
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                prompt: "Worktree has uncommitted changes:\nf1.txt\nf2.txt\nRemove anyway? y/n"
                    .into(),
                action: Confirmed::ForceRemove(wt.id),
            })
        );
        assert_eq!(app.message, None);
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::Send(Request::RemoveAgent {
                id: wt.id,
                remove_worktree: true,
                force: true,
            })]
        );
        app.request_failed(dirty(1));
        assert_eq!(app.popup, None, "a forced remove is never asked again");

        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        app.request_failed(dirty(10));
        let Some(Popup::Confirm { prompt, .. }) = &app.popup else {
            panic!("no confirm: {:?}", app.popup);
        };
        let lines: Vec<&str> = prompt.lines().collect();
        assert_eq!(lines.len(), 11, "{prompt}");
        assert_eq!(lines[8], "f8.txt");
        assert_eq!(lines[9], "… and 2 more");
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);

        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        let _ = app.apply_response(Response::Agents(vec![wt.clone()]));
        app.request_failed(dirty(1));
        assert_eq!(app.popup, None);

        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        app.apply_git_stat(wt.id, Ok(stat(1)));
        app.request_failed(dirty(1));
        assert!(
            matches!(app.popup, Some(Popup::Confirm { .. })),
            "git answers arrive out of band"
        );

        let _ = app.on_key(key('n'));
        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('k'));
        app.request_failed(dirty(1));
        assert_eq!(app.popup, None);
        assert!(
            app.current_message(Instant::now())
                .is_some_and(|m| m.starts_with("worktree has uncommitted changes"))
        );
    }

    fn open_diff(app: &mut App, id: AgentId) {
        assert_eq!(
            app.on_key(key('D')),
            vec![Effect::Send(Request::GitDiff { id })]
        );
        assert_eq!(app.focus, Focus::Diff);
    }

    fn diff_scroll(app: &App) -> usize {
        app.diff.as_ref().expect("diff view").scroll
    }

    #[test]
    fn diff_view_opens_scrolls_and_closes() {
        let (mut app, a, b) = two_agents();
        let _ = app.set_pane_size(80, 10);
        open_diff(&mut app, a.id);
        assert_eq!(app.diff.as_ref().and_then(|d| d.lines.as_ref()), None);
        let text: Vec<String> = (0..100).map(|i| format!(" line {i}")).collect();
        app.apply_diff(b.id, Ok("+other".into()));
        assert_eq!(app.diff.as_ref().and_then(|d| d.lines.as_ref()), None);
        app.apply_diff(a.id, Ok(text.join("\n")));
        assert_eq!(
            app.diff.as_ref().and_then(|d| d.lines.as_ref()),
            Some(&text)
        );
        for (event, expected) in [
            (key('j'), 1),
            (key('k'), 0),
            (key('k'), 0),
            (ctrl('d'), 5),
            (ctrl('u'), 0),
            (code(KeyCode::PageDown), 10),
            (code(KeyCode::PageUp), 0),
            (key('G'), 90),
            (key('j'), 90),
            (code(KeyCode::PageDown), 90),
            (key('g'), 0),
        ] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!(diff_scroll(&app), expected, "{event:?}");
        }
        for event in [key('x'), key('d'), key('n'), code(KeyCode::Enter)] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!((app.focus, &app.popup), (Focus::Diff, &None), "{event:?}");
        }
        for close in [key('D'), key('q'), code(KeyCode::Esc)] {
            assert!(app.on_key(close).is_empty());
            assert_eq!(app.focus, Focus::Agents, "{close:?}");
            assert_eq!(app.diff, None, "{close:?}");
            assert!(!app.prefix_pending);
            open_diff(&mut app, a.id);
            app.apply_diff(a.id, Ok(String::new()));
        }
        app.apply_diff(a.id, Ok("+late".into()));
        let _ = app.on_key(key('q'));
        app.apply_diff(a.id, Ok("+late".into()));
        assert_eq!((app.focus, &app.diff), (Focus::Agents, &None));

        open_diff(&mut app, a.id);
        let _ = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        }));
        assert_eq!(app.focus, Focus::Diff);
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!((app.focus, &app.diff), (Focus::Agents, &None));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    fn scroll_effects(id: AgentId, lines: i32) -> Vec<Effect> {
        vec![Effect::Send(Request::Scroll { id, lines })]
    }

    fn running_pair() -> (App, AgentInfo, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        let _ = app.set_pane_size(80, 10);
        (app, a, b)
    }

    #[test]
    fn scroll_mode_from_sidebar_and_prefix() {
        let (mut app, a, _) = running_pair();
        assert!(app.on_key(key('s')).is_empty());
        assert_eq!(app.focus, Focus::Scroll);
        assert_eq!(
            app.scroll,
            Some(ScrollMode {
                agent: a.id,
                from_pane: false
            })
        );
        let _ = app.on_key(key('q'));
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        let _ = app.on_key(ctrl('q'));
        assert!(app.prefix_pending);
        assert!(app.on_key(key('s')).is_empty());
        assert_eq!(app.focus, Focus::Scroll);
        assert!(!app.prefix_pending);
        assert_eq!(app.scroll.map(|s| s.from_pane), Some(true));
    }

    #[test]
    fn scroll_keys_send_scroll_requests() {
        let (mut app, a, _) = running_pair();
        let _ = app.on_key(key('s'));
        for (event, lines) in [
            (key('j'), -1),
            (key('k'), 1),
            (ctrl('d'), -5),
            (ctrl('u'), 5),
            (code(KeyCode::PageDown), -10),
            (code(KeyCode::PageUp), 10),
            (key('g'), i32::MAX / 2),
            (key('G'), i32::MIN / 2),
        ] {
            assert_eq!(app.on_key(event), scroll_effects(a.id, lines), "{event:?}");
            assert_eq!(app.focus, Focus::Scroll, "{event:?}");
        }
        for event in [key('x'), key('d'), key('n'), code(KeyCode::Enter), key('s')] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!((app.focus, &app.popup), (Focus::Scroll, &None), "{event:?}");
        }
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);
    }

    #[test]
    fn scroll_exit_returns_to_origin_and_bottoms() {
        let (mut app, a, _) = running_pair();
        let bottom = scroll_effects(a.id, i32::MIN / 2);
        for exit in [key('q'), code(KeyCode::Esc)] {
            let _ = app.on_key(key('s'));
            assert_eq!(app.on_key(exit), bottom, "{exit:?}");
            assert_eq!((app.focus, app.scroll), (Focus::Agents, None), "{exit:?}");

            let _ = app.on_key(code(KeyCode::Enter));
            let _ = app.on_key(ctrl('q'));
            let _ = app.on_key(key('s'));
            assert_eq!(app.on_key(exit), bottom, "{exit:?}");
            assert_eq!((app.focus, app.scroll), (Focus::Pane, None), "{exit:?}");
            assert!(!app.prefix_pending);
            app.focus = Focus::Agents;
        }

        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(ctrl('q'));
        let _ = app.on_key(key('s'));
        let _ = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        }));
        assert_eq!(app.focus, Focus::Scroll);
        assert_eq!(app.on_key(key('q')), bottom);
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn scroll_mode_closes_when_its_agent_disappears() {
        let (mut app, a, b) = running_pair();
        let _ = app.on_key(key('s'));
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!((app.focus, app.scroll), (Focus::Agents, None));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn scroll_needs_a_terminal() {
        let p = project("p");
        let stopped = agent(&p, "stopped", Status::Stopped);
        let exited = agent(&p, "exited", Status::Exited(1));
        let (mut app, _) = loaded(vec![p], vec![stopped, exited.clone()]);
        assert!(app.on_key(key('s')).is_empty());
        assert_eq!(app.focus, Focus::Scroll);
        assert_eq!(app.scroll.map(|s| s.agent), Some(exited.id));
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('k'));
        assert!(app.on_key(key('s')).is_empty());
        assert_eq!((app.focus, app.scroll), (Focus::Agents, None));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("nothing to scroll")
        );
    }

    #[test]
    fn diff_is_requested_once_while_loading() {
        let (mut app, a, b) = two_agents();
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        assert!(app.on_key(key('D')).is_empty(), "a's diff is still loading");
        assert_eq!(app.focus, Focus::Diff);
        assert_eq!(app.diff.as_ref().map(|d| d.agent), Some(a.id));
        app.apply_diff(a.id, Ok("+one".into()));
        assert_eq!(
            app.diff.as_ref().and_then(|d| d.lines.clone()),
            Some(vec!["+one".to_string()])
        );
        let _ = app.on_key(key('q'));
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('j'));
        open_diff(&mut app, b.id);
    }

    #[test]
    fn unanswered_diff_is_given_up_after_30s() {
        let (mut app, a, _) = two_agents();
        let t0 = Instant::now();
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        let _ = app.on_tick(t0 + Duration::from_secs(29));
        assert!(app.on_key(key('D')).is_empty());
        let _ = app.on_key(key('q'));
        let _ = app.on_tick(t0 + Duration::from_secs(31));
        open_diff(&mut app, a.id);
    }

    #[test]
    fn quit_keys_quit_from_the_diff_view() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);
        let _ = app.set_pane_size(10, 3);
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);
        assert!(app.on_key(key('q')).is_empty());
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn diff_view_caps_huge_diffs() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        let text: Vec<String> = (0..DIFF_LINE_CAP + 5).map(|i| format!("+{i}")).collect();
        app.apply_diff(a.id, Ok(text.join("\n")));
        let lines = app
            .diff
            .as_ref()
            .and_then(|d| d.lines.clone())
            .expect("lines");
        assert_eq!(lines.len(), DIFF_LINE_CAP + 1);
        assert_eq!(lines[DIFF_LINE_CAP - 1], format!("+{}", DIFF_LINE_CAP - 1));
        assert_eq!(lines[DIFF_LINE_CAP], "… diff truncated (5 more lines)");

        let _ = app.on_key(key('q'));
        open_diff(&mut app, a.id);
        app.apply_diff(a.id, Ok("+\tx\u{1b}[2J\r".into()));
        let lines = app
            .diff
            .as_ref()
            .and_then(|d| d.lines.clone())
            .expect("lines");
        assert_eq!(lines, ["+    x[2J"]);
    }

    #[test]
    fn diff_error_closes_with_message() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        app.apply_diff(a.id, Err("not a git repository".into()));
        assert_eq!((app.focus, &app.diff), (Focus::Agents, &None));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("not a git repository")
        );
    }
}
