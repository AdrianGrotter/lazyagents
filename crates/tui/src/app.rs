use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use protocol::{
    AgentId, AgentInfo, AgentKind, CreateAgent, GitStat, InputEvent, Project, ProjectId, Request,
    Response, Screen, ServerMessage, Status, WorktreeRequest,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Position, Rect};

use crate::commands::{self, Context, CustomCommand, Output, Prompt};
use crate::config::{Config, PresetDef, Theme};
use crate::keymap::{Action, Keymap};
use crate::keys;
use crate::known::{self, KnownProject};
use crate::review::{self, Comment, Location};
use crate::ui::Areas;

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
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentForm {
    pub name: String,
    pub preset: Preset,
    pub command: String,
    pub worktree: bool,
    pub base: String,
    pub prompt: String,
    pub field: Field,
}

/// A run of whitespace, with any dashes touching it, becomes one dash; dashes typed without
/// whitespace stay as typed. Leading and trailing dashes are dropped.
fn normalize_name(input: &str) -> String {
    let separator = |c: char| c.is_whitespace() || c == '-';
    let mut out = String::new();
    let mut rest = input;
    while !rest.is_empty() {
        let split = rest.find(separator).unwrap_or(rest.len());
        out.push_str(&rest[..split]);
        rest = &rest[split..];
        let end = rest.find(|c| !separator(c)).unwrap_or(rest.len());
        let run = &rest[..end];
        out.push_str(if run.contains(char::is_whitespace) {
            "-"
        } else {
            run
        });
        rest = &rest[end..];
    }
    out.trim_matches('-').to_string()
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
        fields.push(Field::Prompt);
        fields
    }

    fn step_field(&mut self, step: i32) {
        let fields = self.fields();
        let index = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        self.field = fields[wrap(index, step, fields.len())];
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectField {
    Path,
    Name,
}

/// An empty name leaves it to the daemon, which names the project after its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectForm {
    pub path: String,
    pub name: String,
    pub field: ProjectField,
}

impl ProjectForm {
    fn focused(&mut self) -> &mut String {
        match self.field {
            ProjectField::Path => &mut self.path,
            ProjectField::Name => &mut self.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    Kill(AgentId),
    Restart(AgentId),
    Remove(AgentId),
    RemoveWorktree(AgentId),
    ForceRemove(AgentId),
    RemoveProject(ProjectId),
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

/// `lines` is `None` until the daemon's diff arrives; `locations` holds each line's place
/// in the files, for comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub agent: AgentId,
    pub lines: Option<Vec<String>>,
    pub locations: Vec<Option<Location>>,
    pub scroll: usize,
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMode {
    pub agent: AgentId,
    pub from_pane: bool,
}

/// A custom command waiting on its prompts; `vars` fills its template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRun {
    pub command: usize,
    pub step: usize,
    pub vars: HashMap<String, String>,
    pub cwd: PathBuf,
    pub project: ProjectId,
    pub input: String,
    pub choice: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Help,
    NewAgent(NewAgentForm),
    Rename {
        agent: AgentId,
        input: String,
    },
    /// Row 0 is "new project"; row `i` is `App::project_candidates()[i - 1]`.
    ProjectPicker {
        index: usize,
    },
    AddProject(ProjectForm),
    Confirm {
        prompt: String,
        action: Confirmed,
    },
    /// Editing `App::filter`, which narrows the Agents box as it changes.
    Filter,
    /// Row `i` is `App::commands[i]`.
    Commands {
        index: usize,
    },
    CommandPrompt(CommandRun),
    /// Writing the review comment on `location` of `agent`'s diff.
    Comment {
        agent: AgentId,
        location: Location,
        code: String,
        input: String,
    },
    /// A line to send to `agent`, as if typed into its pane and submitted.
    Reply {
        agent: AgentId,
        input: String,
    },
    /// A finished command's output.
    Output {
        title: String,
        lines: Vec<String>,
        scroll: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send(Request),
    Copy(String),
    /// Tells the outer terminal, with a bell or a desktop notification.
    Notify(String),
    /// Runs `script` with `sh -c` in the real terminal while the TUI is suspended.
    RunInTerminal {
        title: String,
        script: String,
        cwd: PathBuf,
    },
    /// Runs `script` with `sh -c` in the background and reports through
    /// `App::command_finished`; `show` puts its output in a popup.
    RunCommand {
        title: String,
        script: String,
        cwd: PathBuf,
        show: bool,
    },
    RememberProjects(Vec<KnownProject>),
    ForgetProject(PathBuf),
    Quit,
    StopDaemonAndQuit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Drag,
    Up,
    ScrollUp,
    ScrollDown,
}

/// A left-button or wheel event at absolute terminal coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

/// Cells of the pane as `(col, row)` relative to the pane's inner area; `anchor` is where
/// the drag started, so it may come after `end` in reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub end: (u16, u16),
}

impl Selection {
    fn ordered(self) -> ((u16, u16), (u16, u16)) {
        let reading = |(col, row): (u16, u16)| (row, col);
        if reading(self.anchor) <= reading(self.end) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }

    pub fn rows(self) -> RangeInclusive<u16> {
        let ((_, first), (_, last)) = self.ordered();
        first..=last
    }

    /// The selected columns of `row` in a line `width` cells wide: from the start column
    /// on the first row, up to the end column on the last, the whole line in between.
    pub fn columns(self, row: u16, width: u16) -> Option<RangeInclusive<u16>> {
        let ((start_col, start_row), (end_col, end_row)) = self.ordered();
        if width == 0 || !(start_row..=end_row).contains(&row) {
            return None;
        }
        let first = if row == start_row { start_col } else { 0 };
        let last = if row == end_row { end_col } else { u16::MAX };
        let last = last.min(width - 1);
        (first <= last).then_some(first..=last)
    }

    /// Spacer cells after wide characters have no text and are skipped; trailing spaces
    /// are trimmed from every line.
    pub fn text(self, screen: &Screen) -> String {
        self.rows()
            .map(|row| {
                let Some(line) = screen.lines.get(usize::from(row)) else {
                    return String::new();
                };
                let width = u16::try_from(line.len()).unwrap_or(u16::MAX);
                let Some(columns) = self.columns(row, width) else {
                    return String::new();
                };
                let text: String = columns
                    .map(|col| line[usize::from(col)].text.as_str())
                    .collect();
                text.trim_end_matches(' ').to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentView {
    Project,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Error,
    Info,
}

struct PendingRemove {
    agent: AgentId,
    interactions: u64,
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
    pub message_kind: MessageKind,
    pub git_stats: HashMap<AgentId, GitStat>,
    pub diff: Option<DiffView>,
    pub scroll: Option<ScrollMode>,
    pub selection: Option<Selection>,
    pub known: Vec<KnownProject>,
    /// Agents whose turn ended while they were out of view, until they are viewed.
    unseen: HashSet<AgentId>,
    terminal_focused: bool,
    filter: String,
    /// The agent whose pane was scrolled from the sidebar, to scroll back down once it
    /// is left or opened.
    peeked: Option<AgentId>,
    stats_in_flight: HashMap<AgentId, Instant>,
    diffs_in_flight: HashMap<AgentId, Instant>,
    stat_polled: Option<(AgentId, Instant)>,
    pending_remove: Option<PendingRemove>,
    pending_select: Option<PendingCreate>,
    interactions: u64,
    subscribed_running: bool,
    too_small: bool,
    pub prefix_pending: bool,
    pub keymap: Keymap,
    pub presets: Vec<PresetDef>,
    pub commands: Vec<CustomCommand>,
    /// Review comments per agent, kept until they are sent.
    pub comments: HashMap<AgentId, Vec<Comment>>,
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
            message_kind: MessageKind::Error,
            git_stats: HashMap::new(),
            diff: None,
            scroll: None,
            selection: None,
            known: Vec::new(),
            unseen: HashSet::new(),
            terminal_focused: true,
            filter: String::new(),
            peeked: None,
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
            commands: config.commands,
            comments: HashMap::new(),
            theme: config.theme,
            cwd,
            home,
            shell,
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    /// The path with the home directory shown as `~`.
    pub fn display_path(&self, path: &std::path::Path) -> String {
        match self
            .home
            .as_ref()
            .and_then(|home| path.strip_prefix(home).ok())
        {
            Some(rest) if rest.as_os_str().is_empty() => "~".into(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    /// Known projects that are not in the sidebar.
    pub fn project_candidates(&self) -> Vec<&KnownProject> {
        self.known
            .iter()
            .filter(|k| !self.projects.iter().any(|p| p.path == k.path))
            .collect()
    }

    fn remember_new(&mut self, projects: &[Project]) -> Vec<Effect> {
        let added: Vec<KnownProject> = projects
            .iter()
            .filter(|p| !self.projects.iter().any(|old| old.id == p.id))
            .map(|p| KnownProject {
                name: p.name.clone(),
                path: p.path.clone(),
            })
            .collect();
        if known::remember(&mut self.known, &added) {
            vec![Effect::RememberProjects(added)]
        } else {
            Vec::new()
        }
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
        agents.retain(|a| self.matches_filter(a));
        agents.sort_by(|a, b| {
            (a.status != Status::NeedsInput, position(a.project), &a.name).cmp(&(
                b.status != Status::NeedsInput,
                position(b.project),
                &b.name,
            ))
        });
        agents
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The status the Agents box shows: `done` for an unseen finished agent.
    pub fn status_label(&self, agent: &AgentInfo) -> String {
        match self.unseen.contains(&agent.id) {
            true => "done".to_string(),
            false => agent.status.label(),
        }
    }

    /// A case-insensitive substring of the agent's listed name, name or branch; after
    /// `s:`, of its status as listed.
    fn matches_filter(&self, agent: &AgentInfo) -> bool {
        let needle = self.filter.to_lowercase();
        if let Some(status) = needle.strip_prefix("s:") {
            return self.status_label(agent).to_lowercase().contains(status);
        }
        let branch = agent.worktree.as_ref().map(|w| w.branch.as_str());
        [
            Some(crate::ui::label(agent)),
            Some(agent.name.as_str()),
            branch,
        ]
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(&needle))
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
        self.message_kind = MessageKind::Error;
    }

    pub fn inform(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), Instant::now()));
        self.message_kind = MessageKind::Info;
    }

    /// A dirty-worktree refusal opens the force prompt only while the user is still
    /// waiting for it in the sidebar; otherwise the prompt could take keys typed for an
    /// agent and force-remove its changes.
    pub fn request_failed(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.pending_select = None;
        if let Some(pending) = self.pending_remove.take()
            && let Some(files) = message.strip_prefix(DIRTY_WORKTREE)
            && let Some(name) = self
                .agents
                .iter()
                .find(|a| a.id == pending.agent)
                .map(|a| a.name.clone())
        {
            let waiting = self.popup.is_none()
                && matches!(self.focus, Focus::Projects | Focus::Agents)
                && pending.interactions == self.interactions;
            if waiting {
                self.popup = Some(Popup::Confirm {
                    prompt: force_prompt(files),
                    action: Confirmed::ForceRemove(pending.agent),
                });
            } else {
                let retry = self.keymap.first_key(Action::Remove);
                self.fail(format!("{name}: {DIRTY_WORKTREE} — {retry} to try again"));
            }
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
            Ok(text) => {
                let lines = diff_lines(&text);
                view.locations = review::locations(&lines);
                view.lines = Some(lines);
            }
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
        self.unseen.retain(|id| exists(id));
    }

    pub fn apply_server(&mut self, message: ServerMessage) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        let mut effects = Vec::new();
        match message {
            ServerMessage::AgentUpdated(info) => effects.extend(self.upsert_agent(info)),
            ServerMessage::AgentBell(id) => effects.extend(self.on_bell(id)),
            ServerMessage::AgentRemoved(id) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.agents.retain(|a| a.id != id);
                self.reselect(selected);
            }
            ServerMessage::ProjectsChanged(projects) => {
                let selected = self.selected_agent().map(|a| a.id);
                effects = self.remember_new(&projects);
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
        effects.extend(self.sync_after_server(before, None));
        effects
    }

    pub fn apply_response(&mut self, response: Response) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        self.pending_remove = None;
        let mut created = None;
        let mut effects = Vec::new();
        match response {
            Response::Projects(projects) => {
                let selected = self.selected_agent().map(|a| a.id);
                effects = self.remember_new(&projects);
                self.replace_projects(projects);
                self.reselect(selected);
            }
            Response::Agents(agents) => {
                let selected = self.selected_agent().map(|a| a.id);
                self.agents = agents;
                self.reselect(selected);
            }
            Response::Project(project) => {
                effects = self.remember_new(std::slice::from_ref(&project));
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
                let _ = self.upsert_agent(info);
            }
            Response::Ok | Response::GitStat(_) | Response::Diff(_) => {}
        }
        effects.extend(self.sync_after_server(before, created));
        effects
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
        let mut effects = self.close_lost_scroll(selected);
        if let Some(id) = created {
            self.select_agent(id);
            self.focus = Focus::Pane;
        }
        effects.extend(self.sync());
        effects
    }

    /// Scroll mode ends when the selection moves to another agent, which scrolls the
    /// agent it leaves back to the bottom, or when its agent is removed or stopped.
    fn close_lost_scroll(&mut self, selected: Option<AgentId>) -> Vec<Effect> {
        let Some(mode) = self.scroll.filter(|_| self.focus == Focus::Scroll) else {
            return Vec::new();
        };
        let status = self
            .agents
            .iter()
            .find(|a| a.id == mode.agent)
            .map(|a| a.status);
        let effects = match status {
            None | Some(Status::Stopped) => Vec::new(),
            Some(_) if selected != Some(mode.agent) => {
                vec![scroll_request(mode.agent, SCROLL_BOTTOM)]
            }
            Some(_) => return Vec::new(),
        };
        self.scroll = None;
        self.focus = Focus::Agents;
        effects
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

    /// Returns the notification for an agent out of view that starts needing input or
    /// ends its turn.
    fn upsert_agent(&mut self, info: AgentInfo) -> Option<Effect> {
        let selected = self.selected_agent().map(|a| a.id);
        let in_view = selected == Some(info.id) && self.terminal_focused;
        let (id, status, name) = (info.id, info.status, crate::ui::label(&info).to_string());
        let before = match self.agents.iter_mut().find(|a| a.id == info.id) {
            Some(existing) => Some(std::mem::replace(existing, info).status),
            None => {
                self.agents.push(info);
                None
            }
        };
        let ended_turn =
            matches!(before, Some(Status::Working | Status::NeedsInput)) && status == Status::Idle;
        let started_waiting =
            before.is_some_and(|b| b != Status::NeedsInput) && status == Status::NeedsInput;
        if ended_turn && !in_view {
            self.unseen.insert(id);
        } else if status != Status::Idle {
            self.unseen.remove(&id);
        }
        self.reselect(selected);
        let body = match (in_view, started_waiting, ended_turn) {
            (false, true, _) => format!("{name} needs input"),
            (false, _, true) => format!("{name} is done"),
            _ => return None,
        };
        Some(self.notify(body))
    }

    /// Bells from Claude agents are left out: their hooks already report what a bell
    /// would announce.
    fn on_bell(&mut self, id: AgentId) -> Option<Effect> {
        let in_view = self.selected_agent().map(|a| a.id) == Some(id) && self.terminal_focused;
        let agent = self.agents.iter().find(|a| a.id == id)?;
        if in_view || agent.kind == AgentKind::Claude {
            return None;
        }
        let body = format!("{} rang the bell", crate::ui::label(agent));
        Some(self.notify(body))
    }

    /// The status bar repeats the notification unless it is showing an error.
    fn notify(&mut self, body: String) -> Effect {
        let error_shown = self.message_kind == MessageKind::Error
            && self.current_message(Instant::now()).is_some();
        if !error_shown {
            self.inform(body.clone());
        }
        Effect::Notify(body)
    }

    pub fn window_title(&self) -> String {
        match self
            .agents
            .iter()
            .filter(|a| a.status == Status::NeedsInput)
            .count()
        {
            0 => "lazyagents".to_string(),
            1 => "lazyagents · 1 needs input".to_string(),
            n => format!("lazyagents · {n} need input"),
        }
    }

    pub fn is_unseen(&self, id: AgentId) -> bool {
        self.unseen.contains(&id)
    }

    pub fn on_focus(&mut self, focused: bool) -> Vec<Effect> {
        self.terminal_focused = focused;
        self.sync()
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

    fn set_filter(&mut self, filter: String) -> Vec<Effect> {
        let selected = self.selected_agent().map(|a| a.id);
        self.filter = filter;
        self.agent_index = 0;
        self.reselect(selected);
        self.sync()
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
        let mut effects = self.release_peek();
        effects.extend(self.follow_selection());
        effects
    }

    /// A pane scrolled from the sidebar goes back to the bottom once the selection
    /// leaves it or the pane is opened.
    fn release_peek(&mut self) -> Vec<Effect> {
        let Some(id) = self.peeked else {
            return Vec::new();
        };
        let selected = self.selected_agent().map(|a| a.id);
        if selected == Some(id) && self.focus != Focus::Pane {
            return Vec::new();
        }
        self.peeked = None;
        match self.agents.iter().find(|a| a.id == id) {
            Some(agent) if agent.status != Status::Stopped => {
                vec![scroll_request(id, SCROLL_BOTTOM)]
            }
            _ => Vec::new(),
        }
    }

    fn follow_selection(&mut self) -> Vec<Effect> {
        let wanted = self.selected_agent().map(|a| (a.id, a.status.is_running()));
        if let (Some((id, _)), true) = (wanted, self.terminal_focused) {
            self.unseen.remove(&id);
        }
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
        self.selection = None;
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

/// How many of `comments` sit on `location`: 0 or 1.
fn comments_at(comments: &[Comment], location: Option<&Option<Location>>) -> usize {
    match location {
        Some(Some(location)) => comments.iter().filter(|c| c.location == *location).count(),
        _ => 0,
    }
}

fn press(id: AgentId, code: protocol::KeyCode) -> Effect {
    Effect::Send(Request::Input {
        id,
        event: InputEvent::Key(protocol::Key::new(code, protocol::Modifiers::NONE)),
    })
}

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
            (KeyCode::Char('c'), false) => {
                self.open_comment();
                return Vec::new();
            }
            (KeyCode::Char('n'), false) => {
                self.jump_to_comment(true);
                return Vec::new();
            }
            (KeyCode::Char('N'), false) => {
                self.jump_to_comment(false);
                return Vec::new();
            }
            (KeyCode::Char('S'), false) => return self.send_comments(),
            _ => return Vec::new(),
        };
        self.move_diff_cursor(step);
        Vec::new()
    }

    fn move_diff_cursor(&mut self, step: i64) {
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        let last = view.lines.as_ref().map_or(0, |l| l.len().saturating_sub(1)) as i64;
        view.cursor = (view.cursor as i64).saturating_add(step).clamp(0, last) as usize;
        self.keep_cursor_visible();
    }

    /// Scrolls the least that shows the cursor line, counting the comment rows drawn
    /// under commented lines.
    fn keep_cursor_visible(&mut self) {
        let page = self.diff_page();
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        let commented = self
            .comments
            .get(&view.agent)
            .map_or(&[][..], Vec::as_slice);
        let rows = |from: usize, to: usize| {
            (from..=to)
                .map(|i| 1 + comments_at(commented, view.locations.get(i)))
                .sum::<usize>()
        };
        if view.cursor < view.scroll {
            view.scroll = view.cursor;
        }
        while view.scroll < view.cursor && rows(view.scroll, view.cursor) > page {
            view.scroll += 1;
        }
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
        view.cursor = view
            .cursor
            .clamp(view.scroll, view.scroll + page.saturating_sub(1));
        let lines = view.lines.as_ref().map_or(0, Vec::len);
        view.cursor = view.cursor.min(lines.saturating_sub(1));
    }

    fn open_comment(&mut self) {
        let Some(view) = &self.diff else {
            return;
        };
        let (Some(Some(location)), Some(code)) = (
            view.locations.get(view.cursor),
            view.lines.as_ref().and_then(|l| l.get(view.cursor)),
        ) else {
            self.fail("comment on a changed or context line");
            return;
        };
        let input = self
            .comments
            .get(&view.agent)
            .and_then(|c| c.iter().find(|c| c.location == *location))
            .map(|c| c.text.clone())
            .unwrap_or_default();
        self.popup = Some(Popup::Comment {
            agent: view.agent,
            location: location.clone(),
            code: code.clone(),
            input,
        });
    }

    /// An empty comment removes the one on that line.
    fn save_comment(&mut self, agent: AgentId, location: Location, code: String, text: String) {
        let comments = self.comments.entry(agent).or_default();
        comments.retain(|c| c.location != location);
        let text = text.trim().to_string();
        if !text.is_empty() {
            comments.push(Comment {
                location,
                code,
                text,
            });
        }
        if comments.is_empty() {
            self.comments.remove(&agent);
        }
        self.keep_cursor_visible();
    }

    fn jump_to_comment(&mut self, forward: bool) {
        let Some(view) = self.diff.as_mut() else {
            return;
        };
        let commented = self
            .comments
            .get(&view.agent)
            .map_or(&[][..], Vec::as_slice);
        let count = view.locations.len();
        let target = (1..=count)
            .map(|offset| match forward {
                true => (view.cursor + offset) % count,
                false => (view.cursor + count - offset % count) % count,
            })
            .find(|&i| comments_at(commented, view.locations.get(i)) > 0);
        match target {
            Some(line) => {
                view.cursor = line;
                self.keep_cursor_visible();
            }
            None => self.fail("no comments on this diff"),
        }
    }

    /// Sends every comment on the agent's diff as one prompt, in diff order, and returns
    /// to the sidebar to watch it work.
    fn send_comments(&mut self) -> Vec<Effect> {
        let Some(view) = &self.diff else {
            return Vec::new();
        };
        let id = view.agent;
        let order = |location: &Location| {
            view.locations
                .iter()
                .position(|l| l.as_ref() == Some(location))
        };
        let Some(mut comments) = self.comments.get(&id).cloned() else {
            self.fail("no comments to send");
            return Vec::new();
        };
        comments.sort_by_key(|c| order(&c.location));
        let Some(agent) = self.agents.iter().find(|a| a.id == id) else {
            return Vec::new();
        };
        if !agent.status.is_running() {
            let message = format!("{} is not running", crate::ui::label(agent));
            self.fail(message);
            return Vec::new();
        }
        let message = format!(
            "sent {} comment{} to {}",
            comments.len(),
            if comments.len() == 1 { "" } else { "s" },
            crate::ui::label(agent)
        );
        self.comments.remove(&id);
        self.close_diff();
        self.inform(message);
        vec![
            Effect::Send(Request::Input {
                id,
                event: InputEvent::Paste(review::prompt(&comments)),
            }),
            press(id, protocol::KeyCode::Enter),
        ]
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
            Some(Popup::Rename { input, .. }) => input.push_str(&line),
            Some(Popup::CommandPrompt(run)) => run.input.push_str(&line),
            Some(Popup::Comment { input, .. }) => input.push_str(&line),
            Some(Popup::Reply { input, .. }) => {
                input.push_str(&text.replace("\r\n", " ").replace(['\r', '\n'], " "))
            }
            Some(Popup::AddProject(form)) => form.focused().push_str(&line),
            Some(Popup::NewAgent(form)) => match form.field {
                Field::Name => form.name.push_str(&line),
                Field::Command => form.command.push_str(&line),
                Field::Base => form.base.push_str(&line),
                Field::Prompt => form
                    .prompt
                    .push_str(&text.replace("\r\n", " ").replace(['\r', '\n'], " ")),
                Field::Preset | Field::Worktree => {}
            },
            Some(Popup::Filter) => {
                let filter = format!("{}{}", self.filter, line);
                let filter: String = filter.chars().filter(|c| !c.is_control()).collect();
                return self.set_filter(filter);
            }
            Some(
                Popup::Help
                | Popup::ProjectPicker { .. }
                | Popup::Confirm { .. }
                | Popup::Commands { .. }
                | Popup::Output { .. },
            )
            | None => {}
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
        if let (Focus::Scroll, Some(mode)) = (self.focus, self.scroll) {
            let lines = if up { WHEEL_LINES } else { -WHEEL_LINES };
            return vec![scroll_request(mode.agent, lines)];
        }
        let Some(agent) = self.selected_agent() else {
            return Vec::new();
        };
        if agent.status == Status::Stopped {
            return Vec::new();
        }
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

    pub fn on_mouse(&mut self, event: MouseInput, areas: &Areas) -> Vec<Effect> {
        if self.too_small {
            if event.kind == MouseKind::Up {
                self.selection = None;
            }
            return Vec::new();
        }
        let at = Position::new(event.col, event.row);
        let pane = areas.pane_inner;
        match event.kind {
            MouseKind::ScrollUp | MouseKind::ScrollDown if pane.contains(at) => self.on_wheel(
                event.kind == MouseKind::ScrollUp,
                event.col - pane.x,
                event.row - pane.y,
            ),
            MouseKind::ScrollUp | MouseKind::ScrollDown => Vec::new(),
            MouseKind::Down => {
                self.interactions += 1;
                self.selection = None;
                if self.popup.is_some() {
                    if crate::ui::popup_area(self, areas.screen).is_some_and(|r| !r.contains(at)) {
                        self.popup = None;
                    }
                    return Vec::new();
                }
                self.on_click(at, areas)
            }
            MouseKind::Drag => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.end = clamped(at, pane);
                }
                Vec::new()
            }
            MouseKind::Up => {
                let Some(mut selection) = self.selection.take() else {
                    return Vec::new();
                };
                selection.end = clamped(at, pane);
                if selection.anchor == selection.end {
                    return Vec::new();
                }
                let text = selection.text(&self.screen);
                if text.is_empty() {
                    return Vec::new();
                }
                self.inform(format!("copied {} characters", text.chars().count()));
                vec![Effect::Copy(text)]
            }
        }
    }

    /// A click is an explicit action, so a click in the pane on a running agent focuses
    /// it; a click on an Agents row never does.
    fn on_click(&mut self, at: Position, areas: &Areas) -> Vec<Effect> {
        if areas.projects.contains(at) {
            let mut effects = self.leave_for_sidebar(Focus::Projects);
            let row = crate::ui::row_at(
                areas.projects,
                self.project_index,
                self.projects.len(),
                at.y,
            );
            if let Some(index) = row {
                self.highlight_project(index);
            }
            effects.extend(self.sync());
            return effects;
        }
        if areas.agents.contains(at) {
            let mut effects = self.leave_for_sidebar(Focus::Agents);
            let agents = self.visible_agents();
            let row = crate::ui::row_at(areas.agents, self.agent_index, agents.len(), at.y);
            if let Some(id) = row.map(|index| agents[index].id) {
                self.select_agent(id);
            }
            effects.extend(self.sync());
            return effects;
        }
        if !areas.pane_inner.contains(at) || self.focus == Focus::Diff {
            return Vec::new();
        }
        let Some((name, running)) = self
            .selected_agent()
            .map(|a| (a.name.clone(), a.status.is_running()))
        else {
            return Vec::new();
        };
        let cell = clamped(at, areas.pane_inner);
        self.selection = Some(Selection {
            anchor: cell,
            end: cell,
        });
        if self.focus == Focus::Scroll {
            return Vec::new();
        }
        if running {
            self.focus = Focus::Pane;
        } else {
            let message = format!(
                "{name} is not running — {} restarts",
                self.keymap.first_key(Action::Restart)
            );
            self.fail(message);
        }
        Vec::new()
    }

    /// Closes the diff view or scroll mode, scrolling the agent back to the bottom, and
    /// focuses a sidebar box.
    fn leave_for_sidebar(&mut self, focus: Focus) -> Vec<Effect> {
        let mut effects = Vec::new();
        match self.focus {
            Focus::Diff => self.diff = None,
            Focus::Scroll => {
                if let Some(mode) = self.scroll.take() {
                    effects.push(scroll_request(mode.agent, SCROLL_BOTTOM));
                }
            }
            Focus::Projects | Focus::Agents | Focus::Pane => {}
        }
        self.focus = focus;
        self.prefix_pending = false;
        effects
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
        match self.keymap.direct(&key) {
            Some(Action::PaneNext) => self.cycle_in_pane(1),
            Some(Action::PanePrevious) => self.cycle_in_pane(-1),
            Some(_) => {
                self.leave_pane();
                Vec::new()
            }
            None => self.input(id, &key),
        }
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
        if key.code == KeyCode::Esc && !self.filter.is_empty() {
            return self.set_filter(String::new());
        }
        if let Some(index) = self.command_key(&key) {
            return self.start_command(index);
        }
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
                self.popup = Some(if self.project_candidates().is_empty() {
                    self.project_form()
                } else {
                    Popup::ProjectPicker { index: 0 }
                });
            }
            Action::Stop => self.confirm_for_selected("Stop", Confirmed::Kill),
            Action::Remove if self.focus == Focus::Projects => self.confirm_remove_project(),
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
                        locations: Vec::new(),
                        scroll: 0,
                        cursor: 0,
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
            Action::Commands => self.popup = Some(Popup::Commands { index: 0 }),
            Action::Approve | Action::Deny => {
                let answer = if action == Action::Approve {
                    protocol::KeyCode::Enter
                } else {
                    protocol::KeyCode::Esc
                };
                match self.selected_agent() {
                    Some(agent) if agent.status == Status::NeedsInput => {
                        return vec![press(agent.id, answer)];
                    }
                    Some(agent) => {
                        let message =
                            format!("{} is not waiting for an answer", crate::ui::label(agent));
                        self.fail(message);
                    }
                    None => {}
                }
            }
            Action::Reply => match self.selected_agent() {
                Some(agent) if agent.status.is_running() => {
                    self.popup = Some(Popup::Reply {
                        agent: agent.id,
                        input: String::new(),
                    });
                }
                Some(agent) => {
                    let message = format!("{} is not running", crate::ui::label(agent));
                    self.fail(message);
                }
                None => {}
            },
            Action::Filter => {
                self.focus = Focus::Agents;
                self.popup = Some(Popup::Filter);
            }
            Action::PeekDown | Action::PeekUp => {
                let half = (self.diff_page() / 2).max(1) as i32;
                let lines = if action == Action::PeekDown {
                    -half
                } else {
                    half
                };
                match self.selected_agent() {
                    Some(agent) if agent.status != Status::Stopped => {
                        let id = agent.id;
                        self.peeked = Some(id);
                        return vec![scroll_request(id, lines)];
                    }
                    _ => {}
                }
            }
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

    fn project_form(&self) -> Popup {
        Popup::AddProject(ProjectForm {
            path: self.cwd.display().to_string(),
            name: String::new(),
            field: ProjectField::Path,
        })
    }

    fn confirm_remove_project(&mut self) {
        let Some(project) = self.selected_project() else {
            return;
        };
        if self.agents.iter().any(|a| a.project == project.id) {
            let message = format!("remove {}'s agents first", project.name);
            self.fail(message);
            return;
        }
        let prompt = format!(
            "Remove project {}? It stays in the add list ({}).",
            project.name,
            self.keymap.first_key(Action::AddProject)
        );
        let action = Confirmed::RemoveProject(project.id);
        self.popup = Some(Popup::Confirm { prompt, action });
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
            prompt: String::new(),
            field: Field::Name,
        }));
    }

    fn next_attention(&mut self) {
        let position = |id: ProjectId| self.projects.iter().position(|p| p.id == id);
        let mut ordered: Vec<&AgentInfo> = self
            .agents
            .iter()
            .filter(|a| self.matches_filter(a))
            .collect();
        ordered.sort_by(|a, b| (position(a.project), &a.name).cmp(&(position(b.project), &b.name)));
        let current = self.selected_agent().map(|a| a.id);
        let start = current
            .and_then(|id| ordered.iter().position(|a| a.id == id))
            .map_or(0, |i| i + 1);
        let others: Vec<&AgentInfo> = (0..ordered.len())
            .map(|offset| ordered[(start + offset) % ordered.len()])
            .filter(|a| Some(a.id) != current)
            .collect();
        let next = others
            .iter()
            .find(|a| a.status == Status::NeedsInput)
            .or_else(|| others.iter().find(|a| self.unseen.contains(&a.id)))
            .map(|a| a.id);
        let current_needs_input = self
            .selected_agent()
            .is_some_and(|a| a.status == Status::NeedsInput);
        match next {
            Some(id) => self.select_agent(id),
            None if current_needs_input => self.fail("no other agent needs attention"),
            None => self.fail("no agent needs attention"),
        }
    }

    fn on_popup_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(popup) = self.popup.take() else {
            return Vec::new();
        };
        match popup {
            Popup::Help => Vec::new(),
            Popup::Commands { index } => self.on_commands_key(index, key),
            Popup::Comment {
                agent,
                location,
                code,
                mut input,
            } => {
                match edit(&mut input, &key) {
                    Edit::Submit => self.save_comment(agent, location, code, input),
                    Edit::Cancel => {}
                    Edit::Continue => {
                        self.popup = Some(Popup::Comment {
                            agent,
                            location,
                            code,
                            input,
                        })
                    }
                }
                Vec::new()
            }
            Popup::Reply { agent, mut input } => match edit(&mut input, &key) {
                Edit::Submit if input.trim().is_empty() => Vec::new(),
                Edit::Submit => vec![
                    Effect::Send(Request::Input {
                        id: agent,
                        event: InputEvent::Paste(input),
                    }),
                    press(agent, protocol::KeyCode::Enter),
                ],
                Edit::Cancel => Vec::new(),
                Edit::Continue => {
                    self.popup = Some(Popup::Reply { agent, input });
                    Vec::new()
                }
            },
            Popup::CommandPrompt(run) => self.on_command_prompt_key(run, key),
            Popup::Output {
                title,
                lines,
                scroll,
            } => {
                self.on_output_key(title, lines, scroll, key);
                Vec::new()
            }
            Popup::Filter => {
                let mut filter = self.filter.clone();
                match edit(&mut filter, &key) {
                    Edit::Submit => return self.sync(),
                    Edit::Cancel => return self.set_filter(String::new()),
                    Edit::Continue => {}
                }
                self.popup = Some(Popup::Filter);
                self.set_filter(filter)
            }
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
                    name: normalize_name(&input),
                })],
                Edit::Cancel => Vec::new(),
                Edit::Continue => {
                    self.popup = Some(Popup::Rename { agent, input });
                    Vec::new()
                }
            },
            Popup::ProjectPicker { index } => self.on_picker_key(index, &key),
            Popup::AddProject(form) => self.on_project_form_key(form, &key),
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
            (Confirmed::RemoveProject(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::Send(Request::RemoveProject { id: *id })]
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
        self.pending_remove = (remove_worktree && !force).then_some(PendingRemove {
            agent: id,
            interactions: self.interactions,
        });
        vec![Effect::Send(Request::RemoveAgent {
            id,
            remove_worktree,
            force,
        })]
    }

    fn on_picker_key(&mut self, index: usize, key: &KeyEvent) -> Vec<Effect> {
        let rows = self.project_candidates().len() + 1;
        let picked = index
            .checked_sub(1)
            .and_then(|i| self.project_candidates().get(i).map(|k| (*k).clone()));
        let index = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Enter => {
                return match picked {
                    Some(project) => vec![Effect::Send(Request::AddProject {
                        path: project.path,
                        name: Some(project.name),
                    })],
                    None => {
                        self.popup = Some(self.project_form());
                        Vec::new()
                    }
                };
            }
            KeyCode::Char('j') | KeyCode::Down => wrap(index, 1, rows),
            KeyCode::Char('k') | KeyCode::Up => wrap(index, -1, rows),
            KeyCode::Char('x') => match picked {
                Some(project) => {
                    known::forget(&mut self.known, &project.path);
                    self.popup = Some(Popup::ProjectPicker {
                        index: index.min(rows - 2),
                    });
                    return vec![Effect::ForgetProject(project.path)];
                }
                None => index,
            },
            _ => index,
        };
        self.popup = Some(Popup::ProjectPicker { index });
        Vec::new()
    }

    fn on_project_form_key(&mut self, mut form: ProjectForm, key: &KeyEvent) -> Vec<Effect> {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                form.field = match form.field {
                    ProjectField::Path => ProjectField::Name,
                    ProjectField::Name => ProjectField::Path,
                };
            }
            _ => match edit(form.focused(), key) {
                Edit::Submit => {
                    let name = form.name.trim();
                    return vec![Effect::Send(Request::AddProject {
                        path: self.project_path(form.path.trim()),
                        name: (!name.is_empty()).then(|| name.to_string()),
                    })];
                }
                Edit::Cancel => return Vec::new(),
                Edit::Continue => {}
            },
        }
        self.popup = Some(Popup::AddProject(form));
        Vec::new()
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
                    Field::Prompt => Some(&mut form.prompt),
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

    /// `<preset>-<n>` with the lowest `n` that no agent in the project uses.
    fn free_name(&self, project: ProjectId, preset: Preset) -> String {
        self.free_name_from(project, preset.label(&self.presets))
    }

    fn free_name_from(&self, project: ProjectId, base: &str) -> String {
        let base = normalize_name(base);
        let base = if base.is_empty() { "agent" } else { &base };
        let taken = |name: &str| {
            self.agents
                .iter()
                .any(|a| a.project == project && a.name == name)
        };
        (1..)
            .map(|n| format!("{base}-{n}"))
            .find(|name| !taken(name))
            .expect("some number is free")
    }

    fn submit_form(&mut self, form: NewAgentForm) -> Vec<Effect> {
        let Some(project) = self.selected_project().map(|p| p.id) else {
            return Vec::new();
        };
        let name = match normalize_name(&form.name) {
            name if name.is_empty() => self.free_name(project, form.preset),
            name => name,
        };
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
        let prompt = form.prompt.trim();
        let prompt = (!prompt.is_empty()).then(|| prompt.to_string());
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
            prompt,
        }))]
    }
}

impl App {
    fn command_key(&self, key: &KeyEvent) -> Option<usize> {
        self.commands
            .iter()
            .position(|c| c.key.is_some_and(|spec| spec.matches(key)))
    }

    fn start_command(&mut self, index: usize) -> Vec<Effect> {
        let Some(command) = self.commands.get(index) else {
            return Vec::new();
        };
        let description = command.description.clone();
        let agent = self.selected_agent().cloned();
        let project = match (command.context, &agent) {
            (Context::Agent, Some(agent)) => self.projects.iter().find(|p| p.id == agent.project),
            _ => self.selected_project(),
        }
        .cloned();
        let (cwd, project) = match (command.context, &agent, project) {
            (Context::Agent, Some(agent), Some(project)) => (agent.cwd.clone(), project),
            (Context::Project, _, Some(project)) => (project.path.clone(), project),
            (Context::Agent, ..) => {
                self.fail(format!("{description} needs a selected agent"));
                return Vec::new();
            }
            (Context::Project, ..) => {
                self.fail(format!("{description} needs a selected project"));
                return Vec::new();
            }
        };
        let mut vars = HashMap::from([
            ("project.name".to_string(), project.name.clone()),
            (
                "project.path".to_string(),
                project.path.display().to_string(),
            ),
        ]);
        if let Some(agent) = &agent {
            let worktree = agent.worktree.as_ref();
            vars.extend([
                ("agent.name".to_string(), agent.name.clone()),
                (
                    "agent.title".to_string(),
                    crate::ui::label(agent).to_string(),
                ),
                ("agent.cwd".to_string(), agent.cwd.display().to_string()),
                (
                    "agent.branch".to_string(),
                    worktree.map(|w| w.branch.clone()).unwrap_or_default(),
                ),
                (
                    "agent.base".to_string(),
                    worktree.map(|w| w.base.clone()).unwrap_or_default(),
                ),
            ]);
        }
        let run = CommandRun {
            command: index,
            step: 0,
            vars,
            cwd,
            project: project.id,
            input: String::new(),
            choice: 0,
        };
        self.next_prompt(run)
    }

    /// Opens the run's next prompt, or runs the command once every prompt is answered.
    fn next_prompt(&mut self, mut run: CommandRun) -> Vec<Effect> {
        let prompt = self
            .commands
            .get(run.command)
            .and_then(|c| c.prompts.get(run.step));
        match prompt {
            Some(prompt) => {
                run.input = match prompt {
                    Prompt::Input { initial, .. } => initial.clone(),
                    _ => String::new(),
                };
                run.choice = 0;
                self.popup = Some(Popup::CommandPrompt(run));
                Vec::new()
            }
            None => self.run_command(run),
        }
    }

    fn on_command_prompt_key(&mut self, mut run: CommandRun, key: KeyEvent) -> Vec<Effect> {
        let Some(prompt) = self
            .commands
            .get(run.command)
            .and_then(|c| c.prompts.get(run.step))
            .cloned()
        else {
            return Vec::new();
        };
        let answer = match &prompt {
            Prompt::Input { key: name, .. } => match edit(&mut run.input, &key) {
                Edit::Submit => Some((name.clone(), run.input.clone())),
                Edit::Cancel => return Vec::new(),
                Edit::Continue => None,
            },
            Prompt::Menu {
                key: name, options, ..
            } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    run.choice = wrap(run.choice, 1, options.len());
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    run.choice = wrap(run.choice, -1, options.len());
                    None
                }
                KeyCode::Enter => Some((name.clone(), options[run.choice].clone())),
                KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
                _ => None,
            },
            Prompt::Confirm { .. } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some((String::new(), String::new())),
                KeyCode::Char('n') | KeyCode::Esc => return Vec::new(),
                _ => None,
            },
        };
        match answer {
            Some((name, value)) => {
                if !name.is_empty() {
                    run.vars.insert(format!("form.{name}"), value);
                }
                run.step += 1;
                self.next_prompt(run)
            }
            None => {
                self.popup = Some(Popup::CommandPrompt(run));
                Vec::new()
            }
        }
    }

    fn run_command(&mut self, run: CommandRun) -> Vec<Effect> {
        let Some(command) = self.commands.get(run.command).cloned() else {
            return Vec::new();
        };
        let script = match commands::expand(&command.command, &run.vars) {
            Ok(script) => script,
            Err(error) => {
                self.fail(format!("{}: {error}", command.description));
                return Vec::new();
            }
        };
        let (title, cwd) = (command.description, run.cwd);
        match command.output {
            Output::Terminal => vec![Effect::RunInTerminal { title, script, cwd }],
            Output::Popup | Output::None => {
                let show = command.output == Output::Popup;
                if show {
                    self.inform(format!("running {title}…"));
                }
                vec![Effect::RunCommand {
                    title,
                    script,
                    cwd,
                    show,
                }]
            }
            Output::Agent => {
                let name = self.free_name_from(run.project, &command.name);
                let (cols, rows) = self.pane_size.unwrap_or(DEFAULT_SIZE);
                self.pending_select = Some(PendingCreate {
                    project: run.project,
                    name: name.clone(),
                    interactions: self.interactions,
                });
                let cwd = commands::shell_quote(&cwd.display().to_string());
                vec![Effect::Send(Request::CreateAgent(CreateAgent {
                    project: run.project,
                    name,
                    kind: AgentKind::Generic,
                    command: vec!["sh".into(), "-c".into(), format!("cd {cwd} && {script}")],
                    cols,
                    rows,
                    worktree: None,
                    prompt: None,
                }))]
            }
        }
    }

    fn on_commands_key(&mut self, index: usize, key: KeyEvent) -> Vec<Effect> {
        let count = self.commands.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if count > 0 => {
                self.popup = Some(Popup::Commands {
                    index: wrap(index, 1, count),
                });
            }
            KeyCode::Char('k') | KeyCode::Up if count > 0 => {
                self.popup = Some(Popup::Commands {
                    index: wrap(index, -1, count),
                });
            }
            KeyCode::Enter if index < count => return self.start_command(index),
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => self.popup = Some(Popup::Commands { index }),
        }
        Vec::new()
    }

    fn on_output_key(&mut self, title: String, lines: Vec<String>, scroll: usize, key: KeyEvent) {
        let page = self.diff_page().max(2);
        let last = lines.len().saturating_sub(1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scroll = match (key.code, ctrl) {
            (KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q'), false) => return,
            (KeyCode::Char('j') | KeyCode::Down, false) => scroll + 1,
            (KeyCode::Char('k') | KeyCode::Up, false) => scroll.saturating_sub(1),
            (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => scroll + page / 2,
            (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => scroll.saturating_sub(page / 2),
            (KeyCode::Char('g'), false) => 0,
            (KeyCode::Char('G'), false) => last,
            _ => scroll,
        };
        self.popup = Some(Popup::Output {
            title,
            lines,
            scroll: scroll.min(last),
        });
    }

    /// `result` is the exit code and the combined stdout and stderr, or why the command
    /// could not run.
    pub fn command_finished(
        &mut self,
        title: &str,
        show: bool,
        result: Result<(i32, String), String>,
    ) {
        let (code, output) = match result {
            Ok(done) => done,
            Err(error) => {
                self.fail(format!("{title} failed: {error}"));
                return;
            }
        };
        if show && self.popup.is_none() {
            let title = match code {
                0 => title.to_string(),
                code => format!("{title} (exit {code})"),
            };
            self.popup = Some(Popup::Output {
                title,
                lines: output.lines().map(str::to_string).collect(),
                scroll: 0,
            });
            if self.current_message(Instant::now()).is_some()
                && self.message_kind == MessageKind::Info
            {
                self.message = None;
            }
            return;
        }
        let last = output.lines().rev().find(|l| !l.trim().is_empty());
        match (code, last) {
            (0, _) => self.inform(format!("{title}: done")),
            (code, Some(line)) => self.fail(format!("{title} failed (exit {code}): {line}")),
            (code, None) => self.fail(format!("{title} failed (exit {code})")),
        }
    }

    /// For a command run in the real terminal, which only reports a failure.
    pub fn terminal_command_finished(&mut self, title: &str, result: Result<i32, String>) {
        match result {
            Ok(0) => {}
            Ok(code) => self.fail(format!("{title} exited with {code}")),
            Err(error) => self.fail(format!("{title} failed: {error}")),
        }
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

/// `at` as a cell of `area`, moved inside it when the pointer has left it.
fn clamped(at: Position, area: Rect) -> (u16, u16) {
    let col =
        at.x.clamp(area.x, area.right().saturating_sub(1).max(area.x));
    let row =
        at.y.clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
    (col - area.x, row - area.y)
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
            title: None,
            waiting_for: None,
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
        let (app, effects) = loaded(vec![p.clone()], vec![a.clone()]);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(
            effects,
            vec![
                Effect::RememberProjects(vec![known_of(&p)]),
                Effect::Send(Request::Subscribe { id: a.id })
            ]
        );
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
    fn ctrl_j_and_ctrl_k_reach_the_agent_by_default() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        for c in ['j', 'k'] {
            assert_eq!(
                app.on_key(ctrl(c)),
                vec![char_input(a.id, c, protocol::Modifiers::CTRL)]
            );
            assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        }
    }

    #[test]
    fn direct_next_and_previous_switch_and_stay_in_the_pane() {
        let config = configured(
            r#"
            [keys.pane]
            direct_next = "ctrl-j"
            direct_previous = "ctrl-k"
            "#,
        );
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded_with(config, vec![p], vec![a.clone(), b.clone()]);
        let _ = app.on_key(code(KeyCode::Enter));
        let effects = app.on_key(ctrl('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        let effects = app.on_key(ctrl('k'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: a.id })]);
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
    fn normalize_name_turns_whitespace_into_single_dashes() {
        for (input, expected) in [
            ("a - b", "a-b"),
            ("a  b", "a-b"),
            ("a -- b", "a-b"),
            ("a--b", "a--b"),
            ("a\tb", "a-b"),
            ("  my  new agent ", "my-new-agent"),
            (" -x- ", "x"),
            ("", ""),
            ("   ", ""),
            ("plain", "plain"),
        ] {
            assert_eq!(normalize_name(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn form_submit_normalizes_the_name() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, " my  new agent ");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(c.name, "my-new-agent");
    }

    #[test]
    fn rename_submit_normalizes_the_name() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(key('R'));
        for _ in 0..5 {
            let _ = app.on_key(code(KeyCode::Backspace));
        }
        type_text(&mut app, " big  omega ");
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::RenameAgent {
                id: a.id,
                name: "big-omega".into()
            })]
        );
    }

    #[test]
    fn number_keys_focus_the_sidebar_boxes() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('2'));
        assert_eq!(app.focus, Focus::Agents);
        let _ = app.on_key(key('1'));
        assert_eq!(app.focus, Focus::Projects);
    }

    #[test]
    fn number_keys_are_text_in_the_new_agent_name_field() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let _ = app.on_key(key('1'));
        let _ = app.on_key(key('2'));
        assert_eq!(form(&app).name, "12");
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
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.path == "/tmp/x"),
            "{:?}",
            app.popup
        );
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_paste("My repo\nnext".into());
        assert!(
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.name == "My repo"),
            "{:?}",
            app.popup
        );
    }

    fn known_of(project: &Project) -> KnownProject {
        KnownProject {
            name: project.name.clone(),
            path: project.path.clone(),
        }
    }

    /// One open project `p` with an agent, and `old`, added in an earlier session.
    fn with_known_old() -> (App, Project, KnownProject) {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone()], vec![a]);
        let old = KnownProject {
            name: "old".into(),
            path: "/p/old".into(),
        };
        app.known.push(old.clone());
        (app, p, old)
    }

    #[test]
    fn loading_remembers_the_daemons_projects_in_order() {
        let (p, q) = (project("p"), project("q"));
        let (app, effects) = loaded(vec![p.clone(), q.clone()], Vec::new());
        let expected = vec![known_of(&p), known_of(&q)];
        assert_eq!(effects, vec![Effect::RememberProjects(expected.clone())]);
        assert_eq!(app.known, expected);
    }

    #[test]
    fn a_project_removed_from_the_sidebar_stays_known() {
        let (p, q) = (project("p"), project("q"));
        let (mut app, _) = loaded(vec![p.clone(), q.clone()], Vec::new());
        let effects = app.apply_server(ServerMessage::ProjectsChanged(vec![p.clone()]));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::RememberProjects(_))),
            "{effects:?}"
        );
        assert_eq!(app.known, vec![known_of(&p), known_of(&q)]);
        assert_eq!(app.project_candidates(), vec![&known_of(&q)]);
    }

    #[test]
    fn a_newly_added_project_moves_to_the_front() {
        let (mut app, p, old) = with_known_old();
        let readded = Project {
            id: ProjectId::random(),
            name: old.name.clone(),
            path: old.path.clone(),
        };
        let effects = app.apply_server(ServerMessage::ProjectsChanged(vec![
            p.clone(),
            readded.clone(),
        ]));
        assert!(
            effects.contains(&Effect::RememberProjects(vec![old.clone()])),
            "{effects:?}"
        );
        assert_eq!(app.known, vec![old, known_of(&p)]);
        assert!(app.project_candidates().is_empty());
    }

    #[test]
    fn add_opens_the_form_when_no_other_project_is_known() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('a'));
        assert_eq!(
            app.popup,
            Some(Popup::AddProject(ProjectForm {
                path: "/work".into(),
                name: String::new(),
                field: ProjectField::Path,
            }))
        );
    }

    #[test]
    fn add_opens_the_picker_on_new_project() {
        let (mut app, _, old) = with_known_old();
        let _ = app.on_key(key('a'));
        assert_eq!(app.popup, Some(Popup::ProjectPicker { index: 0 }));
        assert_eq!(app.project_candidates(), vec![&old]);
        let _ = app.on_key(code(KeyCode::Enter));
        assert!(
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.path == "/work"),
            "{:?}",
            app.popup
        );
    }

    #[test]
    fn picker_adds_a_known_project_with_its_name() {
        let (mut app, _, old) = with_known_old();
        let _ = app.on_key(key('a'));
        let _ = app.on_key(key('j'));
        assert_eq!(app.popup, Some(Popup::ProjectPicker { index: 1 }));
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::AddProject {
                path: old.path,
                name: Some(old.name),
            })]
        );
        assert_eq!(app.popup, None);
    }

    #[test]
    fn picker_moves_with_wrapping_and_closes_on_esc() {
        let (mut app, _, _) = with_known_old();
        let _ = app.on_key(key('a'));
        let _ = app.on_key(key('k'));
        assert_eq!(app.popup, Some(Popup::ProjectPicker { index: 1 }));
        let _ = app.on_key(code(KeyCode::Down));
        assert_eq!(app.popup, Some(Popup::ProjectPicker { index: 0 }));
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn x_in_the_picker_forgets_the_highlighted_project() {
        let (mut app, p, old) = with_known_old();
        let _ = app.on_key(key('a'));
        assert!(app.on_key(key('x')).is_empty());
        assert_eq!(app.known.len(), 2);
        let _ = app.on_key(key('j'));
        assert_eq!(app.on_key(key('x')), vec![Effect::ForgetProject(old.path)]);
        assert_eq!(app.known, vec![known_of(&p)]);
        assert_eq!(app.popup, Some(Popup::ProjectPicker { index: 0 }));
    }

    #[test]
    fn project_form_sends_the_name_and_path() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('a'));
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "~/repo");
        let _ = app.on_key(code(KeyCode::Tab));
        type_text(&mut app, " My repo ");
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![Effect::Send(Request::AddProject {
                path: "/home/u/repo".into(),
                name: Some("My repo".into()),
            })]
        );
    }

    #[test]
    fn d_on_a_project_without_agents_confirms_removing_it() {
        let (p, q) = (project("p"), project("q"));
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p, q.clone()], vec![a]);
        let _ = app.on_key(key('1'));
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('d'));
        assert!(
            matches!(
                &app.popup,
                Some(Popup::Confirm { prompt, action: Confirmed::RemoveProject(id) })
                    if *id == q.id && prompt.contains('q')
            ),
            "{:?}",
            app.popup
        );
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::Send(Request::RemoveProject { id: q.id })]
        );
    }

    #[test]
    fn d_on_a_project_with_agents_refuses() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('1'));
        let _ = app.on_key(key('d'));
        assert_eq!(app.popup, None);
        let message = app.current_message(Instant::now()).unwrap_or_default();
        assert!(message.contains("agents first"), "{message}");
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
    fn an_empty_name_takes_the_next_free_number_for_the_preset() {
        let (p, q) = (project("p"), project("q"));
        let agents = vec![
            agent(&p, "claude-1", Status::Idle),
            agent(&p, "claude-3", Status::Idle),
            agent(&q, "claude-2", Status::Idle),
        ];
        let (mut app, _) = loaded(vec![p, q], agents);
        let _ = app.on_key(key('n'));
        assert_eq!(created(app.on_key(code(KeyCode::Enter))).name, "claude-2");

        let _ = app.on_key(key('n'));
        type_text(&mut app, "   ");
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        assert_eq!(created(app.on_key(code(KeyCode::Enter))).name, "shell-1");
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
            title: None,
            waiting_for: None,
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

    fn update(app: &mut App, info: &AgentInfo, status: Status) {
        let _ = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status,
            ..info.clone()
        }));
    }

    fn finish_turn(app: &mut App, info: &AgentInfo) {
        update(app, info, Status::Working);
        update(app, info, Status::Idle);
    }

    #[test]
    fn a_turn_that_ends_out_of_view_is_unseen_until_selected() {
        let (mut app, a, b) = two_agents();
        finish_turn(&mut app, &b);
        assert!(app.is_unseen(b.id));
        finish_turn(&mut app, &a);
        assert!(!app.is_unseen(a.id));
        let _ = app.on_key(key('j'));
        assert!(!app.is_unseen(b.id));
    }

    #[test]
    fn an_unseen_agent_that_works_again_is_no_longer_unseen() {
        let (mut app, _, b) = two_agents();
        finish_turn(&mut app, &b);
        update(&mut app, &b, Status::Working);
        assert!(!app.is_unseen(b.id));
        update(&mut app, &b, Status::NeedsInput);
        update(&mut app, &b, Status::Idle);
        assert!(app.is_unseen(b.id));
        update(&mut app, &b, Status::Exited(0));
        assert!(!app.is_unseen(b.id));
    }

    #[test]
    fn the_selected_agent_ending_a_turn_unfocused_is_unseen_until_focus_returns() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_focus(false);
        finish_turn(&mut app, &a);
        assert!(app.is_unseen(a.id));
        let _ = app.on_focus(true);
        assert!(!app.is_unseen(a.id));
    }

    #[test]
    fn g_visits_agents_needing_input_before_unseen_ones() {
        let p = project("p");
        let a = agent(&p, "a", Status::Idle);
        let b = agent(&p, "b", Status::Idle);
        let c = agent(&p, "c", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone(), c.clone()]);
        finish_turn(&mut app, &b);
        update(&mut app, &c, Status::NeedsInput);
        let selected = |app: &App| app.selected_agent().map(|x| x.id);
        let _ = app.on_key(key('g'));
        assert_eq!(selected(&app), Some(c.id));
        let _ = app.on_key(key('g'));
        assert_eq!(selected(&app), Some(b.id));
        let _ = app.on_key(key('g'));
        assert_eq!(selected(&app), Some(c.id));
        let _ = app.on_key(key('g'));
        assert_eq!(selected(&app), Some(c.id));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("no other agent needs attention")
        );
    }

    fn notifications(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Notify(body) => Some(body.clone()),
                _ => None,
            })
            .collect()
    }

    fn updated(app: &mut App, info: &AgentInfo, status: Status) -> Vec<String> {
        notifications(&app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status,
            ..info.clone()
        })))
    }

    #[test]
    fn agents_out_of_view_notify_when_they_need_input_or_finish() {
        let (mut app, a, b) = two_agents();
        assert!(updated(&mut app, &b, Status::Working).is_empty());
        assert_eq!(
            updated(&mut app, &b, Status::NeedsInput),
            ["beta needs input"]
        );
        assert!(updated(&mut app, &b, Status::NeedsInput).is_empty());
        assert!(updated(&mut app, &b, Status::Working).is_empty());
        assert_eq!(updated(&mut app, &b, Status::Idle), ["beta is done"]);
        assert!(updated(&mut app, &a, Status::Working).is_empty());
        assert!(updated(&mut app, &a, Status::NeedsInput).is_empty());
        assert!(updated(&mut app, &a, Status::Idle).is_empty());
        let _ = app.on_focus(false);
        assert!(updated(&mut app, &a, Status::Working).is_empty());
        assert_eq!(updated(&mut app, &a, Status::Idle), ["alpha is done"]);
    }

    #[test]
    fn a_bell_out_of_view_notifies_for_generic_agents_only() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Working);
        let c = AgentInfo {
            kind: AgentKind::Claude,
            ..agent(&p, "gamma", Status::Working)
        };
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone(), c.clone()]);
        let bell =
            |app: &mut App, id| notifications(&app.apply_server(ServerMessage::AgentBell(id)));
        assert_eq!(bell(&mut app, b.id), ["beta rang the bell"]);
        assert!(bell(&mut app, a.id).is_empty());
        assert!(bell(&mut app, c.id).is_empty());
    }

    #[test]
    fn the_window_title_counts_agents_needing_input() {
        let (mut app, a, b) = two_agents();
        assert_eq!(app.window_title(), "lazyagents");
        let _ = updated(&mut app, &a, Status::NeedsInput);
        assert_eq!(app.window_title(), "lazyagents · 1 needs input");
        let _ = updated(&mut app, &b, Status::NeedsInput);
        assert_eq!(app.window_title(), "lazyagents · 2 need input");
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
        assert_eq!(effects, vec![Effect::RememberProjects(vec![known_of(&s)])]);
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
            [Field::Name, Field::Preset, Field::Worktree, Field::Prompt]
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
        assert_eq!(form(&app).field, Field::Prompt);
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
                Field::Prompt,
                Field::Name
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
            let _ = app.on_key(code(KeyCode::Up));
            let _ = app.on_key(key(' '));
            let _ = app.on_key(code(KeyCode::Down));
            type_text(&mut app, typed);
            let c = created(app.on_key(code(KeyCode::Enter)));
            assert_eq!(c.worktree, Some(protocol::WorktreeRequest { base }));
        }
    }

    #[test]
    fn form_sends_the_prompt() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(form(&app).field, Field::Prompt);
        type_text(&mut app, "fix it");
        let _ = app.on_paste(" in\nparser.rs\r\n".into());
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(c.prompt.as_deref(), Some("fix it in parser.rs"));

        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let _ = app.on_key(code(KeyCode::Up));
        type_text(&mut app, "   ");
        let c = created(app.on_key(code(KeyCode::Enter)));
        assert_eq!(c.prompt, None);
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
    fn dirty_worktree_refusal_after_the_user_moved_on_is_only_a_message() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt.clone()]);
        let _ = app.on_key(key('d'));
        let _ = app.on_key(key('y'));
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.on_key(key('y')),
            vec![input_key(
                wt.id,
                protocol::Key::new(protocol::KeyCode::Char('y'), protocol::Modifiers::NONE)
            )]
        );

        app.request_failed("worktree has uncommitted changes:\nf.txt");
        assert_eq!(app.popup, None);
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("wt: worktree has uncommitted changes — d to try again")
        );
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

    fn diff_place(app: &App) -> (usize, usize) {
        let view = app.diff.as_ref().expect("diff view");
        (view.cursor, view.scroll)
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
            (key('j'), (1, 0)),
            (key('k'), (0, 0)),
            (key('k'), (0, 0)),
            (ctrl('d'), (5, 0)),
            (ctrl('d'), (10, 1)),
            (ctrl('u'), (5, 1)),
            (code(KeyCode::PageDown), (15, 6)),
            (code(KeyCode::PageUp), (5, 5)),
            (key('G'), (99, 90)),
            (key('j'), (99, 90)),
            (code(KeyCode::PageDown), (99, 90)),
            (key('g'), (0, 0)),
        ] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!(diff_place(&app), expected, "{event:?}");
        }
        for event in [key('x'), key('d'), key('n'), key('S'), code(KeyCode::Enter)] {
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
    fn shifted_j_and_k_scroll_the_pane_from_the_sidebar() {
        let (mut app, a, b) = running_pair();
        for (event, lines) in [
            (key('J'), -5),
            (key('K'), 5),
            (code(KeyCode::PageDown), -5),
            (code(KeyCode::PageUp), 5),
        ] {
            assert_eq!(app.on_key(event), scroll_effects(a.id, lines), "{event:?}");
            assert_eq!(app.focus, Focus::Agents, "{event:?}");
        }
        let effects = app.on_key(key('j'));
        assert_eq!(effects.first(), scroll_effects(a.id, i32::MIN / 2).first());
        assert!(effects.contains(&Effect::Send(Request::Subscribe { id: b.id })));
        let effects = app.on_key(key('k'));
        assert!(effects.contains(&Effect::Send(Request::Subscribe { id: a.id })));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Send(Request::Scroll { .. }))),
            "{effects:?}"
        );
    }

    #[test]
    fn opening_a_scrolled_pane_scrolls_it_back_down() {
        let (mut app, a, _) = running_pair();
        let _ = app.on_key(key('K'));
        let effects = app.on_key(code(KeyCode::Enter));
        assert_eq!(effects, scroll_effects(a.id, i32::MIN / 2));
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn a_stopped_agent_has_no_pane_to_scroll() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a]);
        let _ = app.set_pane_size(80, 10);
        assert!(app.on_key(key('J')).is_empty());
    }

    fn names(app: &App) -> Vec<String> {
        app.visible_agents()
            .iter()
            .map(|a| a.name.clone())
            .collect()
    }

    fn filter_app() -> App {
        let p = project("p");
        let api = AgentInfo {
            worktree: Some(protocol::WorktreeInfo {
                path: "/w/api".into(),
                branch: "lazyagents/api".into(),
                base: "main".into(),
            }),
            ..agent(&p, "api", Status::Idle)
        };
        let docs = AgentInfo {
            kind: AgentKind::Claude,
            title: Some("Fix the parser".into()),
            ..agent(&p, "docs", Status::Idle)
        };
        let web = agent(&p, "web", Status::NeedsInput);
        loaded(vec![p], vec![api, docs, web]).0
    }

    #[test]
    fn slash_filters_the_agents_as_you_type() {
        let mut app = filter_app();
        let _ = app.on_key(key('/'));
        type_text(&mut app, "PARSER");
        assert_eq!(names(&app), ["docs"]);
        assert_eq!(app.selected_agent().map(|a| a.name.as_str()), Some("docs"));
        let _ = app.on_key(code(KeyCode::Backspace));
        let _ = app.on_key(code(KeyCode::Backspace));
        let _ = app.on_key(code(KeyCode::Backspace));
        let _ = app.on_key(code(KeyCode::Backspace));
        let _ = app.on_key(code(KeyCode::Backspace));
        let _ = app.on_key(code(KeyCode::Backspace));
        type_text(&mut app, "lazyagents/a");
        assert_eq!(names(&app), ["api"]);
        let _ = app.on_paste("\u{7f}".into());
        assert_eq!(app.filter(), "lazyagents/a");
    }

    #[test]
    fn the_filter_matches_statuses_after_s_colon() {
        let mut app = filter_app();
        let _ = app.on_key(key('/'));
        type_text(&mut app, "needs");
        assert!(names(&app).is_empty());
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "S:Needs");
        assert_eq!(names(&app), ["web"]);
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "s:idle");
        assert_eq!(names(&app), ["api", "docs"]);
    }

    #[test]
    fn enter_keeps_the_filter_and_esc_clears_it() {
        let mut app = filter_app();
        let _ = app.on_key(key('/'));
        type_text(&mut app, "do");
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(app.popup, None);
        assert_eq!(app.filter(), "do");
        assert_eq!(names(&app), ["docs"]);
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|a| a.name.as_str()), Some("docs"));
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.filter(), "");
        assert_eq!(names(&app), ["web", "api", "docs"]);

        let _ = app.on_key(key('/'));
        type_text(&mut app, "zzz");
        assert!(names(&app).is_empty());
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!((app.popup.as_ref(), app.filter()), (None, ""));
        assert_eq!(names(&app).len(), 3);
    }

    #[test]
    fn slash_from_projects_filters_the_agents() {
        let mut app = filter_app();
        let _ = app.on_key(key('h'));
        let _ = app.on_key(key('/'));
        assert_eq!(app.focus, Focus::Agents);
        assert!(matches!(app.popup, Some(Popup::Filter)));
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

    fn screen_areas(app: &App) -> Areas {
        crate::ui::layout(ratatui::layout::Rect::new(0, 0, 100, 30), app)
    }

    fn mouse(kind: MouseKind, (col, row): (u16, u16)) -> MouseInput {
        MouseInput { kind, col, row }
    }

    fn in_pane(areas: &Areas, col: u16, row: u16) -> (u16, u16) {
        (areas.pane_inner.x + col, areas.pane_inner.y + row)
    }

    fn agent_row_at(areas: &Areas, index: u16) -> (u16, u16) {
        (areas.agents.x + 3, areas.agents.y + 1 + index)
    }

    fn project_row_at(areas: &Areas, index: u16) -> (u16, u16) {
        (areas.projects.x + 3, areas.projects.y + 1 + index)
    }

    /// `世` is wide: it is followed by an empty spacer cell, as the daemon sends it.
    fn screen_of(lines: &[&str]) -> Screen {
        let mut screen = Screen::blank(20, lines.len() as u16);
        for (row, line) in lines.iter().enumerate() {
            let mut cells = Vec::new();
            for c in line.chars() {
                cells.push(Cell {
                    text: c.to_string(),
                    ..Cell::default()
                });
                if c == '世' {
                    cells.push(Cell {
                        text: String::new(),
                        ..Cell::default()
                    });
                }
            }
            for (col, cell) in cells.into_iter().enumerate() {
                screen.lines[row][col] = cell;
            }
        }
        screen
    }

    #[test]
    fn click_project_row_selects_and_focuses_projects() {
        let p = project("p");
        let q = project("q");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&q, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p, q], vec![a.clone(), b.clone()]);
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 1)), &areas);
        assert_eq!((app.focus, app.project_index), (Focus::Projects, 1));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        assert!(
            app.on_mouse(mouse(MouseKind::Up, project_row_at(&areas, 1)), &areas)
                .is_empty()
        );
        assert_eq!(app.focus, Focus::Projects);

        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let _ = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas);
        assert_eq!((app.focus, app.prefix_pending), (Focus::Projects, false));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
    }

    #[test]
    fn click_agent_row_selects_and_focuses_agents_not_pane() {
        let (mut app, a, b) = two_agents();
        app.focus = Focus::Projects;
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 1)), &areas);
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        assert!(
            app.on_mouse(mouse(MouseKind::Up, agent_row_at(&areas, 1)), &areas)
                .is_empty()
        );
        assert_eq!((app.focus, app.selection), (Focus::Agents, None));

        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 0)), &areas);
        assert_eq!((app.focus, app.prefix_pending), (Focus::Agents, false));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));

        app.focus = Focus::Projects;
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 5)), &areas);
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
    }

    #[test]
    fn click_leaves_the_diff_and_scroll_views() {
        let (mut app, a, b) = two_agents();
        let areas = screen_areas(&app);
        open_diff(&mut app, a.id);
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!((app.focus, app.selection), (Focus::Diff, None));
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 1)), &areas);
        assert_eq!((app.focus, &app.diff), (Focus::Agents, &None));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));

        let _ = app.on_key(key('s'));
        let effects = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 0)), &areas);
        assert_eq!(
            effects,
            vec![
                Effect::Send(Request::Scroll {
                    id: b.id,
                    lines: i32::MIN / 2
                }),
                Effect::Send(Request::Subscribe { id: a.id }),
            ]
        );
        assert_eq!((app.focus, app.scroll), (Focus::Agents, None));

        let _ = app.on_key(key('s'));
        let effects = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas);
        assert_eq!(effects, scroll_effects(a.id, i32::MIN / 2));
        assert_eq!((app.focus, app.scroll), (Focus::Projects, None));
    }

    #[test]
    fn click_in_scroll_mode_selects_history_without_leaving() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["older line"]);
        let _ = app.on_key(key('s'));
        assert!(
            app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas)
                .is_empty()
        );
        assert_eq!(app.focus, Focus::Scroll);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 4, 0)), &areas);
        assert_eq!(effects, vec![Effect::Copy("older".into())]);
        assert_eq!(app.focus, Focus::Scroll);
    }

    #[test]
    fn click_pane_focuses_running_agent() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        let at = in_pane(&areas, 4, 2);
        assert!(app.on_mouse(mouse(MouseKind::Down, at), &areas).is_empty());
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: (4, 2),
                end: (4, 2)
            })
        );
        assert!(app.on_mouse(mouse(MouseKind::Up, at), &areas).is_empty());
        assert_eq!((app.focus, app.selection), (Focus::Pane, None));

        let p = project("p");
        let (mut empty, _) = loaded(vec![p], Vec::new());
        assert!(
            empty
                .on_mouse(mouse(MouseKind::Down, at), &areas)
                .is_empty()
        );
        assert_eq!((empty.focus, empty.selection), (Focus::Agents, None));
    }

    #[test]
    fn click_pane_on_stopped_agent_keeps_focus() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a]);
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 1, 1)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("alpha is not running — r restarts")
        );
    }

    #[test]
    fn drag_selects_and_release_copies_text() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello world   ", "a世b   ", "last line here"]);
        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 6, 0)), &areas);
        assert!(
            app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 10, 1)), &areas)
                .is_empty()
        );
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 2, 2)), &areas);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: (6, 0),
                end: (2, 2)
            })
        );
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 2, 2)), &areas);
        assert_eq!(effects, vec![Effect::Copy("world\na世b\nlas".into())]);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("copied 13 characters")
        );
        assert_eq!(app.selection, None);

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 2, 2)), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 1, 1)), &areas);
        assert_eq!(effects, vec![Effect::Copy("世b\nlas".into())]);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("copied 6 characters")
        );

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 2, 1)), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 9, 1)), &areas);
        assert_eq!(effects, vec![Effect::Copy("b".into())]);
    }

    #[test]
    fn plain_click_copies_nothing() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello"]);
        let at = in_pane(&areas, 1, 0);
        let _ = app.on_mouse(mouse(MouseKind::Down, at), &areas);
        assert!(app.on_mouse(mouse(MouseKind::Up, at), &areas).is_empty());
        let _ = app.on_mouse(mouse(MouseKind::Down, at), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 3, 0)), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Drag, at), &areas);
        assert!(app.on_mouse(mouse(MouseKind::Up, at), &areas).is_empty());
        assert_eq!(app.selection, None);
        assert_eq!(app.current_message(Instant::now()), None);
    }

    #[test]
    fn click_outside_popup_closes_it() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        let _ = app.on_key(key('?'));
        let popup = crate::ui::popup_area(&app, areas.screen).expect("popup area");
        let inside = (popup.x + 1, popup.y + 1);
        assert!(
            app.on_mouse(mouse(MouseKind::Down, inside), &areas)
                .is_empty()
        );
        assert!(
            app.on_mouse(mouse(MouseKind::Up, inside), &areas)
                .is_empty()
        );
        assert_eq!(app.popup, Some(Popup::Help));
        assert!(
            app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas)
                .is_empty()
        );
        assert_eq!((&app.popup, app.focus), (&None, Focus::Agents));

        let _ = app.on_key(key('x'));
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            (&app.popup, app.focus, app.selection),
            (&None, Focus::Agents, None)
        );
        assert!(
            app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 5, 0)), &areas)
                .is_empty()
        );
    }

    #[test]
    fn a_release_while_too_small_drops_the_selection() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello"]);
        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 1, 0)), &areas);
        assert!(app.selection.is_some());
        let _ = app.set_pane_size(5, 2);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 3, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.selection, None);
    }

    #[test]
    fn the_wheel_over_a_stopped_agent_does_nothing() {
        let p = project("p");
        let stopped = agent(&p, "halted", Status::Stopped);
        let exited = agent(&p, "done", Status::Exited(0));
        let (mut app, _) = loaded(vec![p], vec![stopped.clone(), exited.clone()]);
        let areas = screen_areas(&app);
        app.select_agent(stopped.id);
        let wheel = mouse(MouseKind::ScrollUp, in_pane(&areas, 1, 1));
        assert_eq!(app.on_mouse(wheel, &areas), Vec::new());
        assert_eq!(app.current_message(Instant::now()), None);
        app.select_agent(exited.id);
        assert_eq!(
            app.on_mouse(wheel, &areas),
            vec![Effect::Send(Request::Scroll {
                id: exited.id,
                lines: WHEEL_LINES
            })]
        );
    }

    #[test]
    fn drag_is_clamped_to_the_pane() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        let (width, height) = (areas.pane_inner.width, areas.pane_inner.height);
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 1, 1)), &areas);
        assert_eq!(app.selection, None);
        let _ = app.on_mouse(mouse(MouseKind::Down, (0, 29)), &areas);
        assert_eq!(app.selection, None);

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 5, 5)), &areas);
        let end = |app: &App| app.selection.map(|s| s.end);
        let _ = app.on_mouse(mouse(MouseKind::Drag, (0, 29)), &areas);
        assert_eq!(end(&app), Some((0, height - 1)));
        let _ = app.on_mouse(mouse(MouseKind::Drag, (99, 0)), &areas);
        assert_eq!(end(&app), Some((width - 1, 0)));
        let _ = app.on_mouse(mouse(MouseKind::Drag, (500, 500)), &areas);
        assert_eq!(end(&app), Some((width - 1, height - 1)));
    }

    #[test]
    fn wheel_scrolls_history_in_scroll_mode() {
        let (mut app, a, _) = running_pair();
        let areas = screen_areas(&app);
        let _ = app.on_key(key('s'));
        let at = in_pane(&areas, 1, 1);
        assert_eq!(
            app.on_mouse(mouse(MouseKind::ScrollUp, at), &areas),
            scroll_effects(a.id, 3)
        );
        assert_eq!(
            app.on_mouse(mouse(MouseKind::ScrollDown, at), &areas),
            scroll_effects(a.id, -3)
        );
        assert_eq!(app.focus, Focus::Scroll);
    }

    #[test]
    fn wheel_outside_the_pane_does_nothing() {
        let (mut app, _, _) = running_pair();
        let areas = screen_areas(&app);
        assert!(
            app.on_mouse(mouse(MouseKind::ScrollUp, agent_row_at(&areas, 0)), &areas)
                .is_empty()
        );
    }

    #[test]
    fn scroll_mode_bottoms_its_agent_when_the_selection_moves_away() {
        let (mut app, a, b) = running_pair();
        let _ = app.on_key(key('s'));
        app.agent_index = 1;
        let effects = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        assert_eq!(effects.first(), scroll_effects(a.id, i32::MIN / 2).first());
        assert!(effects.contains(&Effect::Send(Request::Subscribe { id: b.id })));
        assert_eq!((app.focus, app.scroll), (Focus::Agents, None));
    }

    #[test]
    fn scroll_mode_closes_when_its_agent_stops() {
        let (mut app, a, _) = running_pair();
        let _ = app.on_key(key('s'));
        let effects = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status: Status::Stopped,
            ..a.clone()
        }));
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!((app.focus, app.scroll), (Focus::Agents, None));
    }
}

#[cfg(test)]
mod command_tests {
    use super::tests::{agent, loaded_with, project};
    use super::*;
    use protocol::{AgentKind, CreateAgent};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    fn with_commands(text: &str) -> (App, AgentInfo) {
        let config = crate::config::parse(text).expect("config");
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let p = project("p");
        let a = AgentInfo {
            worktree: Some(protocol::WorktreeInfo {
                path: "/w/api".into(),
                branch: "lazyagents/api".into(),
                base: "main".into(),
            }),
            cwd: "/w/api".into(),
            ..agent(&p, "api", Status::Idle)
        };
        let (app, _) = loaded_with(config, vec![p], vec![a.clone()]);
        (app, a)
    }

    #[test]
    fn a_command_key_runs_its_command_in_the_terminal() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "L"
            description = "lazygit"
            command = "lazygit -p {{agent.cwd}} # {{agent.branch}} {{project.name}}"
            "#,
        );
        assert_eq!(
            app.on_key(key('L')),
            vec![Effect::RunInTerminal {
                title: "lazygit".into(),
                script: "lazygit -p '/w/api' # 'lazyagents/api' 'p'".into(),
                cwd: "/w/api".into(),
            }]
        );
    }

    #[test]
    fn project_commands_run_in_the_project() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "T"
            description = "tests"
            command = "make test"
            context = "project"
            output = "popup"
            "#,
        );
        assert_eq!(
            app.on_key(key('T')),
            vec![Effect::RunCommand {
                title: "tests".into(),
                script: "make test".into(),
                cwd: "/p/p".into(),
                show: true,
            }]
        );
    }

    #[test]
    fn an_agent_command_needs_an_agent() {
        let config = crate::config::parse(
            "[[commands]]\nkey = \"L\"\ndescription = \"lazygit\"\ncommand = \"lazygit\"",
        )
        .expect("config");
        let (mut app, _) = loaded_with(config, vec![project("p")], Vec::new());
        assert!(app.on_key(key('L')).is_empty());
        assert_eq!(
            app.current_message(Instant::now()),
            Some("lazygit needs a selected agent")
        );
    }

    #[test]
    fn prompts_collect_values_before_the_command_runs() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "C"
            description = "commit"
            command = "git commit -m {{form.message}} --{{form.how}}"
            output = "none"

            [[commands.prompts]]
            type = "input"
            key = "message"
            title = "Message"
            initial = "wip: "

            [[commands.prompts]]
            type = "menu"
            key = "how"
            title = "How"
            options = ["quiet", "verbose"]

            [[commands.prompts]]
            type = "confirm"
            title = "Commit now?"
            "#,
        );
        assert!(app.on_key(key('C')).is_empty());
        typed(&mut app, "fix it");
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key('j')).is_empty());
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::RunCommand {
                title: "commit".into(),
                script: "git commit -m 'wip: fix it' --'verbose'".into(),
                cwd: "/w/api".into(),
                show: false,
            }]
        );
        assert_eq!(app.popup, None);
    }

    #[test]
    fn escape_abandons_a_command_at_any_prompt() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            key = "C"
            description = "commit"
            command = "git commit -m {{form.message}}"

            [[commands.prompts]]
            type = "input"
            key = "message"
            "#,
        );
        let _ = app.on_key(key('C'));
        typed(&mut app, "x");
        assert!(app.on_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.popup, None);
    }

    #[test]
    fn an_agent_output_starts_a_generic_agent_in_the_directory() {
        let (mut app, a) = with_commands(
            r#"
            [[commands]]
            key = "W"
            description = "watch tests"
            command = "cargo watch -x test"
            output = "agent"
            name = "watch"
            "#,
        );
        let effects = app.on_key(key('W'));
        let [Effect::Send(Request::CreateAgent(create))] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        assert_eq!(
            create,
            &CreateAgent {
                project: a.project,
                name: "watch-1".into(),
                kind: AgentKind::Generic,
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "cd '/w/api' && cargo watch -x test".into()
                ],
                cols: create.cols,
                rows: create.rows,
                worktree: None,
                prompt: None,
            }
        );
    }

    #[test]
    fn an_unknown_variable_runs_nothing() {
        let (mut app, _) = with_commands(
            "[[commands]]\nkey = \"L\"\ndescription = \"x\"\ncommand = \"echo {{agent.colour}}\"",
        );
        assert!(app.on_key(key('L')).is_empty());
        assert_eq!(
            app.current_message(Instant::now()),
            Some("x: unknown variable agent.colour")
        );
    }

    #[test]
    fn c_lists_the_commands_and_enter_runs_one() {
        let (mut app, _) = with_commands(
            r#"
            [[commands]]
            description = "first"
            command = "one"
            output = "none"

            [[commands]]
            key = "L"
            description = "second"
            command = "two"
            output = "none"
            "#,
        );
        let _ = app.on_key(key('c'));
        assert_eq!(app.popup, Some(Popup::Commands { index: 0 }));
        let _ = app.on_key(key('j'));
        let effects = app.on_key(code(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::RunCommand { script, .. }] if script == "two"),
            "{effects:?}"
        );
    }

    #[test]
    fn popup_output_opens_when_the_command_finishes() {
        let (mut app, _) = with_commands("");
        app.command_finished("status", true, Ok((0, "M a.rs\n?? b.rs\n".into())));
        assert_eq!(
            app.popup,
            Some(Popup::Output {
                title: "status".into(),
                lines: vec!["M a.rs".into(), "?? b.rs".into()],
                scroll: 0,
            })
        );
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn background_commands_report_how_they_ended() {
        let (mut app, _) = with_commands("");
        app.command_finished("fmt", false, Ok((0, String::new())));
        assert_eq!(app.current_message(Instant::now()), Some("fmt: done"));
        app.command_finished("fmt", false, Ok((2, "boom\nlast line\n".into())));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("fmt failed (exit 2): last line")
        );
        app.command_finished("fmt", true, Err("sh: not found".into()));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("fmt failed: sh: not found")
        );
    }
}

#[cfg(test)]
mod reply_tests {
    use super::tests::{agent, loaded, project};
    use super::*;
    use protocol::{InputEvent, Key as ProtoKey, KeyCode as ProtoCode, Modifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn input(id: AgentId, event: InputEvent) -> Effect {
        Effect::Send(Request::Input { id, event })
    }

    fn pressed(id: AgentId, code: ProtoCode) -> Effect {
        input(id, InputEvent::Key(ProtoKey::new(code, Modifiers::NONE)))
    }

    fn waiting() -> (App, AgentInfo) {
        let p = project("p");
        let a = AgentInfo {
            waiting_for: Some("Bash: npm test".into()),
            ..agent(&p, "api", Status::NeedsInput)
        };
        let (app, _) = loaded(vec![p], vec![a.clone()]);
        (app, a)
    }

    #[test]
    fn y_approves_and_capital_n_denies_a_waiting_agent() {
        let (mut app, a) = waiting();
        assert_eq!(app.on_key(key('y')), vec![pressed(a.id, ProtoCode::Enter)]);
        assert_eq!(app.on_key(key('N')), vec![pressed(a.id, ProtoCode::Esc)]);
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn approving_an_agent_that_is_not_waiting_sends_nothing() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "api", Status::Working)]);
        assert!(app.on_key(key('y')).is_empty());
        assert_eq!(
            app.current_message(Instant::now()),
            Some("api is not waiting for an answer")
        );
    }

    #[test]
    fn i_sends_a_typed_line_to_the_agent() {
        let (mut app, a) = waiting();
        assert!(app.on_key(key('i')).is_empty());
        for c in "use pnpm".chars() {
            let _ = app.on_key(key(c));
        }
        assert_eq!(
            app.on_key(code(KeyCode::Enter)),
            vec![
                input(a.id, InputEvent::Paste("use pnpm".into())),
                pressed(a.id, ProtoCode::Enter),
            ]
        );
        assert_eq!(app.popup, None);
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn an_empty_reply_or_escape_sends_nothing() {
        let (mut app, _) = waiting();
        let _ = app.on_key(key('i'));
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        let _ = app.on_key(key('i'));
        let _ = app.on_key(key('x'));
        assert!(app.on_key(code(KeyCode::Esc)).is_empty());
        assert_eq!(app.popup, None);
    }

    #[test]
    fn a_stopped_agent_takes_no_reply() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "api", Status::Stopped)]);
        assert!(app.on_key(key('i')).is_empty());
        assert_eq!(app.popup, None);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("api is not running")
        );
    }
}

#[cfg(test)]
mod diff_comment_tests {
    use super::tests::{agent, loaded, project};
    use super::*;
    use protocol::{InputEvent, Key as ProtoKey, KeyCode as ProtoCode, Modifiers};

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n\
                        --- a/src/a.rs\n\
                        +++ b/src/a.rs\n\
                        @@ -10,2 +10,2 @@\n \
                        keep\n\
                        -old\n\
                        +new";

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    fn with_diff() -> (App, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "api", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let _ = app.set_pane_size(80, 10);
        let _ = app.on_key(key('D'));
        app.apply_diff(a.id, Ok(DIFF.into()));
        (app, a)
    }

    fn to_line(app: &mut App, line: usize) {
        let _ = app.on_key(key('g'));
        for _ in 0..line {
            let _ = app.on_key(key('j'));
        }
    }

    fn comment(app: &mut App, line: usize, text: &str) {
        to_line(app, line);
        let _ = app.on_key(key('c'));
        typed(app, text);
        let _ = app.on_key(code(KeyCode::Enter));
    }

    fn texts(app: &App, id: AgentId) -> Vec<String> {
        app.comments
            .get(&id)
            .map(|c| c.iter().map(|c| c.text.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn c_comments_on_the_cursor_line_and_edits_or_drops_it() {
        let (mut app, a) = with_diff();
        comment(&mut app, 6, "name this better");
        assert_eq!(app.focus, Focus::Diff);
        let comments = &app.comments[&a.id];
        assert_eq!(comments.len(), 1);
        assert_eq!(
            comments[0].location,
            crate::review::Location {
                file: "src/a.rs".into(),
                line: 11,
                removed: false
            }
        );
        assert_eq!(comments[0].code, "+new");
        to_line(&mut app, 6);
        let _ = app.on_key(key('c'));
        assert!(matches!(
            &app.popup,
            Some(Popup::Comment { input, .. }) if input == "name this better"
        ));
        let _ = app.on_key(ctrl_u());
        typed(&mut app, "rename to fresh");
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(texts(&app, a.id), ["rename to fresh"]);
        to_line(&mut app, 6);
        let _ = app.on_key(key('c'));
        let _ = app.on_key(ctrl_u());
        let _ = app.on_key(code(KeyCode::Enter));
        assert!(texts(&app, a.id).is_empty());
    }

    fn ctrl_u() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_header_line_takes_no_comment() {
        let (mut app, _) = with_diff();
        to_line(&mut app, 3);
        assert!(app.on_key(key('c')).is_empty());
        assert_eq!(app.popup, None);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("comment on a changed or context line")
        );
    }

    #[test]
    fn capital_s_sends_the_comments_in_diff_order_and_clears_them() {
        let (mut app, a) = with_diff();
        comment(&mut app, 6, "second");
        comment(&mut app, 4, "first");
        let effects = app.on_key(key('S'));
        let expected = crate::review::prompt(&app_comments_sorted());
        assert_eq!(
            effects,
            vec![
                Effect::Send(Request::Input {
                    id: a.id,
                    event: InputEvent::Paste(expected),
                }),
                Effect::Send(Request::Input {
                    id: a.id,
                    event: InputEvent::Key(ProtoKey::new(ProtoCode::Enter, Modifiers::NONE)),
                }),
            ]
        );
        assert!(texts(&app, a.id).is_empty());
        assert_eq!((app.focus, &app.diff), (Focus::Agents, &None));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("sent 2 comments to api")
        );
    }

    fn app_comments_sorted() -> Vec<crate::review::Comment> {
        let at = |line, removed| crate::review::Location {
            file: "src/a.rs".into(),
            line,
            removed,
        };
        vec![
            crate::review::Comment {
                location: at(10, false),
                code: " keep".into(),
                text: "first".into(),
            },
            crate::review::Comment {
                location: at(11, false),
                code: "+new".into(),
                text: "second".into(),
            },
        ]
    }

    #[test]
    fn n_and_capital_n_jump_between_commented_lines() {
        let (mut app, _) = with_diff();
        comment(&mut app, 4, "a");
        comment(&mut app, 6, "b");
        to_line(&mut app, 0);
        let cursor = |app: &App| app.diff.as_ref().map(|d| d.cursor);
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(4));
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(6));
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(4));
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(cursor(&app), Some(6));
    }

    #[test]
    fn comments_outlive_the_diff_view() {
        let (mut app, a) = with_diff();
        comment(&mut app, 5, "why?");
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('D'));
        app.apply_diff(a.id, Ok(DIFF.into()));
        assert_eq!(texts(&app, a.id), ["why?"]);
    }
}
