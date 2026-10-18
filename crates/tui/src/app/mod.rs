mod commands;
mod diff;
mod finish;
mod forms;
mod input;
mod pane;
mod popup;
mod sidebar;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use protocol::{
    AgentId, AgentInfo, AgentKind, CreateAgent, DiffScope, Failure, GitStat, Project, ProjectId,
    Request, Response, Screen, ServerMessage, Status,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use crate::commands::CustomCommand;
use crate::config::{Config, PresetDef, Theme};
use crate::keymap::{Action, Keymap};
use crate::known::{self, KnownProject};
use crate::review::{Comment, Location};

pub use commands::CommandRun;
pub use diff::{DIFF_LINE_CAP, DiffView};
pub use finish::FinishChoice;
pub use forms::{Field, NewAgentForm, Preset, ProjectField, ProjectForm, Submission};
pub use input::{MouseInput, MouseKind, Selection};
pub use pane::ScrollMode;

use pane::{SCROLL_BOTTOM, scroll_request};

pub const MESSAGE_TTL: Duration = Duration::from_secs(5);
pub const MIN_PANE: (u16, u16) = (20, 5);
pub const STAT_INTERVAL: Duration = Duration::from_secs(5);
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const DIRTY_WORKTREE: &str = "worktree has uncommitted changes";
const DIRTY_FILES_SHOWN: usize = 8;

/// The diff view and scroll mode exist only while they have the focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Agents,
    Pane,
    Diff(DiffView),
    Scroll(ScrollMode),
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
    /// A removal chosen in the finish menu.
    Finish(AgentId, FinishChoice),
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

/// How much of the screen the pane takes: beside the usual sidebar, beside a narrow one, or
/// all of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    Normal,
    Wide,
    Full,
}

impl ScreenMode {
    pub fn next(self) -> Self {
        match self {
            ScreenMode::Normal => ScreenMode::Wide,
            ScreenMode::Wide => ScreenMode::Full,
            ScreenMode::Full => ScreenMode::Normal,
        }
    }
}

/// The help popup's state, which lasts while it is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Help {
    pub filter: String,
    /// Whether typed keys go to the filter.
    pub typing: bool,
    /// The first row on screen.
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    Help(Help),
    NewAgent(NewAgentForm),
    Rename {
        agent: AgentId,
        input: String,
        submission: Submission,
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
    /// Asking to run `project`'s worktree setup `script` for the agent `create` describes, as
    /// filled in on `form`.
    ApproveSetup {
        form: Box<NewAgentForm>,
        create: Box<CreateAgent>,
        project: PathBuf,
        script: String,
        /// The first script row on screen, counting wrapped rows.
        scroll: usize,
    },
    /// Row `index` is `App::finish_choices()[index]` for `agent`.
    Finish {
        agent: AgentId,
        index: usize,
    },
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

impl Popup {
    fn submission(&self) -> Option<&Submission> {
        match self {
            Popup::NewAgent(form) => Some(&form.submission),
            Popup::AddProject(form) => Some(&form.submission),
            Popup::Rename { submission, .. } => Some(submission),
            _ => None,
        }
    }

    fn submission_mut(&mut self) -> Option<&mut Submission> {
        match self {
            Popup::NewAgent(form) => Some(&mut form.submission),
            Popup::AddProject(form) => Some(&mut form.submission),
            Popup::Rename { submission, .. } => Some(submission),
            _ => None,
        }
    }

    pub fn is_sending(&self) -> bool {
        matches!(self.submission(), Some(Submission::Sending(_)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Send(Request),
    Copy(String),
    /// Tells the outer terminal, with a bell or a desktop notification.
    Notify(String),
    /// Remembers that the user approved `project`'s worktree setup `script`.
    ApproveSetup {
        project: PathBuf,
        script: String,
    },
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
pub enum AgentView {
    Project,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Error,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub kind: MessageKind,
    pub at: Instant,
}

/// What removing an agent does with its worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeRemoval {
    Keep,
    Remove,
    /// Remove it even when it has uncommitted changes.
    Force,
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
    /// Kept among `visible_agents()` by `settle_selection` whenever any agent is listed.
    pub selected: Option<AgentId>,
    /// The selected agent's row when the selection last settled; the agent that comes to
    /// this row takes over when the selected one leaves the list.
    settled_row: usize,
    pub subscribed: Option<AgentId>,
    pub screen: Screen,
    pub pane_size: Option<(u16, u16)>,
    /// The whole terminal, which sizes the popups that scroll.
    pub screen_size: Option<(u16, u16)>,
    pub message: Option<Message>,
    /// A key or click came after the message was posted.
    message_seen: bool,
    pub git_stats: HashMap<AgentId, GitStat>,
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
    diffs_in_flight: HashMap<(AgentId, DiffScope), Instant>,
    stat_polled: Option<(AgentId, Instant)>,
    pending_remove: Option<PendingRemove>,
    pending_select: Option<PendingCreate>,
    /// What to report once the removal of an agent with a worktree succeeds.
    removals: HashMap<AgentId, String>,
    /// The agent `enter` restarted; its pane opens if the agent's next update shows it
    /// running while it is still selected.
    open_on_start: Option<AgentId>,
    interactions: u64,
    subscribed_running: bool,
    too_small: bool,
    pub prefix_pending: bool,
    pub keymap: Keymap,
    pub presets: Vec<PresetDef>,
    pub commands: Vec<CustomCommand>,
    pub screen_mode: ScreenMode,
    /// Whether `gh` is on `PATH`, which the pull request choice of the finish menu needs.
    pub gh: bool,
    pub approved_setups: Vec<crate::approved::Approval>,
    /// Review comments per agent, kept until they are sent.
    pub comments: HashMap<AgentId, Vec<Comment>>,
    pub theme: Theme,
    /// Decides whether the new agent form offers a worktree for a project's path.
    pub is_git_repo: fn(&Path) -> bool,
    cwd: PathBuf,
    home: Option<PathBuf>,
    shell: String,
}

impl App {
    pub fn new(cwd: PathBuf, shell: String, home: Option<PathBuf>, config: Config) -> Self {
        let popup = (!config.warnings.is_empty()).then(|| Popup::Output {
            title: "Config warnings".into(),
            lines: config
                .warnings
                .iter()
                .flat_map(|w| w.lines())
                .map(String::from)
                .collect(),
            scroll: 0,
        });
        Self {
            projects: Vec::new(),
            agents: Vec::new(),
            focus: Focus::Agents,
            view: AgentView::Project,
            popup,
            project_index: 0,
            selected: None,
            settled_row: 0,
            subscribed: None,
            screen: Screen::blank(0, 0),
            pane_size: None,
            screen_size: None,
            message: None,
            message_seen: false,
            git_stats: HashMap::new(),
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
            removals: HashMap::new(),
            open_on_start: None,
            interactions: 0,
            subscribed_running: false,
            too_small: false,
            prefix_pending: false,
            keymap: config.keymap,
            presets: config.presets,
            commands: config.commands,
            comments: HashMap::new(),
            screen_mode: ScreenMode::Normal,
            gh: false,
            approved_setups: Vec::new(),
            theme: config.theme,
            is_git_repo: in_git_repo,
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
        [Some(label(agent)), Some(agent.name.as_str()), branch]
            .into_iter()
            .flatten()
            .any(|text| text.to_lowercase().contains(&needle))
    }

    pub fn selected_agent(&self) -> Option<&AgentInfo> {
        let id = self.selected?;
        self.agents
            .iter()
            .find(|a| a.id == id)
            .filter(|a| self.is_listed(a))
    }

    /// The selected agent's row in `visible_agents()`.
    pub fn selected_row(&self) -> Option<usize> {
        let id = self.selected?;
        self.visible_agents().iter().position(|a| a.id == id)
    }

    /// Whether the Agents box lists `agent`, found without sorting the list.
    fn is_listed(&self, agent: &AgentInfo) -> bool {
        let in_view = match self.view {
            AgentView::All => true,
            AgentView::Project => self
                .selected_project()
                .is_some_and(|p| p.id == agent.project),
        };
        in_view && self.matches_filter(agent)
    }

    pub fn attention(&self, project: ProjectId) -> usize {
        self.agents
            .iter()
            .filter(|a| a.project == project && a.status == Status::NeedsInput)
            .count()
    }

    /// A message shows for `MESSAGE_TTL`; an error also stays after that until the user
    /// presses a key or clicks, so it is never gone before anyone was there to read it.
    pub fn current_message(&self, now: Instant) -> Option<&str> {
        self.shown_message(now).map(|m| m.text.as_str())
    }

    pub fn shown_message(&self, now: Instant) -> Option<&Message> {
        let message = self.message.as_ref()?;
        let unread = message.kind == MessageKind::Error && !self.message_seen;
        (unread || now.saturating_duration_since(message.at) < MESSAGE_TTL).then_some(message)
    }

    pub fn animating(&self) -> bool {
        self.agents
            .iter()
            .any(|a| matches!(a.status, Status::Working | Status::Starting))
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.post(message.into(), MessageKind::Error);
    }

    pub fn inform(&mut self, message: impl Into<String>) {
        self.post(message.into(), MessageKind::Info);
    }

    fn post(&mut self, text: String, kind: MessageKind) {
        self.message = Some(Message {
            text,
            kind,
            at: Instant::now(),
        });
        self.message_seen = false;
    }

    /// A dirty-worktree refusal opens the force prompt only while the user is still
    /// waiting for it in the sidebar; otherwise the prompt could take keys typed for an
    /// agent and force-remove its changes.
    pub fn request_failed(&mut self, failure: Failure) {
        self.pending_select = None;
        if let Some(pending) = self.pending_remove.take()
            && let Failure::DirtyWorktree { files } = &failure
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
        self.fail(failure.to_string());
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

    fn forget_gone_agents(&mut self) {
        let exists = |id: &AgentId| self.agents.iter().any(|a| a.id == *id);
        self.git_stats.retain(|id, _| exists(id));
        self.stats_in_flight.retain(|id, _| exists(id));
        self.diffs_in_flight.retain(|(id, _), _| exists(id));
        self.unseen.retain(|id| exists(id));
    }

    pub fn apply_server(&mut self, message: ServerMessage) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        let mut effects = Vec::new();
        match message {
            ServerMessage::AgentUpdated(info) => effects.extend(self.upsert_agent(info)),
            ServerMessage::AgentBell(id) => effects.extend(self.on_bell(id)),
            ServerMessage::AgentRemoved(id) => self.agents.retain(|a| a.id != id),
            ServerMessage::ProjectsChanged(projects) => {
                effects = self.remember_new(&projects);
                self.replace_projects(projects);
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

    /// The answer to the request the open form waits for settles the form: success closes it
    /// and a refusal stays in it. Any other answer updates the model or the status line.
    pub fn apply_reply(
        &mut self,
        request: &Request,
        result: Result<Response, Failure>,
    ) -> Vec<Effect> {
        if let Request::RemoveAgent { id, .. } = request
            && let Some(done) = self.removals.remove(id)
            && result.is_ok()
        {
            self.inform(done);
        }
        let form = self
            .popup
            .as_mut()
            .and_then(Popup::submission_mut)
            .filter(|s| matches!(s, Submission::Sending(sent) if **sent == *request));
        match (form, result) {
            (Some(_), Ok(response)) => {
                self.popup = None;
                self.apply_response(response)
            }
            (Some(submission), Err(error)) => {
                *submission = Submission::Failed(error.to_string());
                if matches!(request, Request::CreateAgent(_)) {
                    self.pending_select = None;
                }
                Vec::new()
            }
            (None, Ok(response)) => self.apply_response(response),
            (None, Err(error)) => {
                self.request_failed(error);
                Vec::new()
            }
        }
    }

    pub fn apply_response(&mut self, response: Response) -> Vec<Effect> {
        let before = self.selected_agent().map(|a| a.id);
        self.pending_remove = None;
        let mut created = None;
        let mut effects = Vec::new();
        match response {
            Response::Projects(projects) => {
                effects = self.remember_new(&projects);
                self.replace_projects(projects);
            }
            Response::Agents(agents) => self.agents = agents,
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
                let is_create_reply = pending.is_some();
                let undisturbed = pending.is_some_and(|p| p.interactions == self.interactions)
                    && self.popup.is_none();
                if undisturbed && info.status.is_running() {
                    created = Some(info.id);
                }
                if is_create_reply && !self.agents.iter().any(|a| a.id == info.id) {
                    let _ = self.upsert_agent(info);
                }
            }
            Response::Done(message) => self.inform(message),
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
        if let Focus::Diff(view) = &self.focus
            && selected != Some(view.agent)
        {
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
        let Some(mode) = self.scroll_mode() else {
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
        let (id, status, name) = (info.id, info.status, label(&info).to_string());
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
        if self.open_on_start == Some(id) {
            self.open_on_start = None;
            if status.is_running() && selected == Some(id) && self.focus == Focus::Agents {
                self.focus = Focus::Pane;
            }
        }
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
        let body = format!("{} rang the bell", label(agent));
        Some(self.notify(body))
    }

    /// The status bar repeats the notification unless it is showing an error.
    fn notify(&mut self, body: String) -> Effect {
        let error_shown = self
            .shown_message(Instant::now())
            .is_some_and(|m| m.kind == MessageKind::Error);
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

    /// Highlights a project and selects its first agent; in the all-agents view a
    /// project without agents keeps the current agent selected.
    fn highlight_project(&mut self, index: usize) {
        self.project_index = index;
        let project = self.projects.get(index).map(|p| p.id);
        let first = self
            .visible_agents()
            .iter()
            .position(|a| Some(a.project) == project);
        match (self.view, first) {
            (AgentView::Project, _) => self.select_row(0),
            (AgentView::All, Some(first)) => self.select_row(first),
            (AgentView::All, None) => {}
        }
    }

    /// A selected agent that the new filter hides gives way to the first row.
    fn set_filter(&mut self, filter: String) -> Vec<Effect> {
        self.filter = filter;
        self.settled_row = 0;
        self.sync()
    }

    /// Selects the agent and highlights its project.
    pub fn select_agent(&mut self, id: AgentId) {
        let Some(project) = self.agents.iter().find(|a| a.id == id).map(|a| a.project) else {
            return;
        };
        if let Some(index) = self.projects.iter().position(|p| p.id == project) {
            self.project_index = index;
        }
        if let Some(row) = self.visible_agents().iter().position(|a| a.id == id) {
            self.selected = Some(id);
            self.settled_row = row;
        }
    }

    /// Selects the agent on `row`, or the last one when the list is shorter.
    fn select_row(&mut self, row: usize) {
        let visible = self.visible_agents();
        let row = row.min(visible.len().saturating_sub(1));
        self.selected = visible.get(row).map(|a| a.id);
        self.settled_row = row;
    }

    /// Keeps the selected agent while it is listed; otherwise the agent that now has its
    /// row takes over.
    fn settle_selection(&mut self) {
        match self.selected_row() {
            Some(row) => self.settled_row = row,
            None => self.select_row(self.settled_row),
        }
    }

    fn clamp(&mut self) {
        self.project_index = self
            .project_index
            .min(self.projects.len().saturating_sub(1));
        self.settle_selection();
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

impl App {
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.message_seen = true;
        if !self.too_small && self.popup.as_ref().is_some_and(Popup::is_sending) {
            if key.code == KeyCode::Esc {
                self.interactions += 1;
                self.popup = None;
            }
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
            Focus::Diff(_) => self.on_diff_key(key),
            Focus::Scroll(_) => self.on_scroll_key(key),
            Focus::Projects | Focus::Agents => self.on_sidebar_key(key),
        }
    }

    pub fn set_screen_size(&mut self, cols: u16, rows: u16) {
        self.screen_size = Some((cols, rows));
    }

    /// Nothing is visible, so only the quit keys (from the sidebar) and the prefix
    /// followed by a leave key (leave the pane) act; no key reaches an agent.
    fn on_too_small_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if matches!(self.focus, Focus::Diff(_)) {
            return self.leave_diff(&key).unwrap_or_default();
        }
        if matches!(self.focus, Focus::Scroll(_)) {
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
}

/// A Claude agent is listed by its session's title once it has one.
pub fn label(agent: &AgentInfo) -> &str {
    match (&agent.title, agent.kind) {
        (Some(title), AgentKind::Claude) => title,
        _ => &agent.name,
    }
}

/// `start` for a stopped agent and `restart` for an exited one, as `enter` does them.
pub fn start_verb(status: Status) -> &'static str {
    match status {
        Status::Stopped => "start",
        _ => "restart",
    }
}

/// Whether `path` is in a git repository as git finds one: a `.git` directory or file in it
/// or in a directory above it.
fn in_git_repo(path: &Path) -> bool {
    path.ancestors().any(|dir| dir.join(".git").exists())
}

fn wrap(index: usize, step: i32, count: usize) -> usize {
    (index as i64 + i64::from(step)).rem_euclid(count as i64) as usize
}

fn force_prompt(files: &[String]) -> String {
    let files: Vec<&String> = files.iter().filter(|f| !f.trim().is_empty()).collect();
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::layout::Areas;
    use protocol::{AgentKind, Cell, CreateAgent, Failure, Frame, InputEvent, Row};
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
            usage: None,
        }
    }

    /// Answers the open diff view's request with `result`.
    pub(crate) fn apply_diff(app: &mut App, id: AgentId, result: Result<String, String>) {
        let scope = app.diff().map_or(DiffScope::All, |view| view.scope);
        app.apply_scoped_diff(id, scope, result);
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
        app.is_git_repo = |_| true;
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
        app.selected = Some(b.id);
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
    fn selection_stays_on_its_agent_when_a_rename_moves_another_above_it() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let c = agent(&p, "gamma", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a, b.clone(), c.clone()]);
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        let effects = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            name: "aardvark".into(),
            ..c.clone()
        }));
        assert_eq!(names(&app), ["aardvark", "alpha", "beta"]);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn a_rename_reply_keeps_the_renamed_agent_selected_on_its_new_row() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a, b.clone()]);
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('R'));
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "aaa");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let renamed = AgentInfo {
            name: "aaa".into(),
            ..b.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(renamed.clone()));
        let _ = app.apply_reply(&request, Ok(Response::Agent(renamed)));
        assert_eq!(app.popup, None);
        assert_eq!(app.selected_row(), Some(0));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn a_filter_that_still_lists_the_selected_agent_keeps_it_on_its_new_row() {
        let mut app = filter_app();
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|a| a.name.as_str()), Some("docs"));
        let _ = app.on_key(key('/'));
        type_text(&mut app, "s:idle");
        assert_eq!(names(&app), ["api", "docs"]);
        assert_eq!(app.selected_agent().map(|a| a.name.as_str()), Some("docs"));
        assert_eq!(app.selected_row(), Some(1));
    }

    fn three_idle() -> (App, AgentInfo, AgentInfo, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "a1", Status::Idle);
        let b = agent(&p, "a2", Status::Idle);
        let c = agent(&p, "b1", Status::Idle);
        let (app, _) = loaded(vec![p], vec![a.clone(), b.clone(), c.clone()]);
        (app, a, b, c)
    }

    #[test]
    fn removing_the_selected_agent_from_a_middle_row_selects_the_one_moving_into_it() {
        let (mut app, _, b, c) = three_idle();
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        let _ = app.apply_server(ServerMessage::AgentRemoved(b.id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(c.id));
        assert_eq!(app.selected_row(), Some(1));
    }

    #[test]
    fn removing_the_selected_agent_from_the_last_row_selects_the_new_last_row() {
        let (mut app, _, b, c) = three_idle();
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(c.id));
        let _ = app.apply_server(ServerMessage::AgentRemoved(c.id));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.selected_row(), Some(1));
    }

    #[test]
    fn a_filter_hiding_the_selected_agent_selects_row_0_which_clearing_keeps() {
        let (mut app, a, _, c) = three_idle();
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('j'));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(c.id));
        let _ = app.on_key(key('/'));
        type_text(&mut app, "a");
        assert_eq!(names(&app), ["a1", "a2"]);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.filter(), "");
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.selected_row(), Some(0));
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
    fn info_messages_expire() {
        let mut app = App::new(
            "/work".into(),
            "/bin/sh".into(),
            Some("/home/u".into()),
            Config::default(),
        );
        app.inform("done");
        let now = Instant::now();
        assert_eq!(app.current_message(now), Some("done"));
        assert_eq!(
            app.current_message(now + MESSAGE_TTL + Duration::from_millis(1)),
            None
        );
    }

    #[test]
    fn an_error_stays_past_its_time_until_a_key_or_click() {
        let (mut app, _, _) = two_agents();
        app.fail("boom");
        let later = Instant::now() + MESSAGE_TTL + Duration::from_secs(60);
        assert_eq!(app.current_message(later), Some("boom"));
        let _ = app.on_key(key('j'));
        assert_eq!(app.current_message(Instant::now()), Some("boom"));
        assert_eq!(app.current_message(later), None);

        app.fail("again");
        let areas = crate::ui::layout(ratatui::layout::Rect::new(0, 0, 100, 30), &app);
        let at = MouseInput {
            kind: MouseKind::Down,
            col: 0,
            row: 0,
        };
        let _ = app.on_mouse(at, &areas);
        assert_eq!(app.current_message(later), None);
    }

    #[test]
    fn removing_another_agent_keeps_the_selection() {
        let p = project("p");
        let a = agent(&p, "a", Status::Idle);
        let b = agent(&p, "b", Status::Idle);
        let c = agent(&p, "c", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone(), c]);
        app.selected = Some(b.id);
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

    pub(super) fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    pub(super) fn code(k: KeyCode) -> KeyEvent {
        KeyEvent::new(k, KeyModifiers::NONE)
    }

    pub(super) fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    pub(super) fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    pub(super) fn input_key(id: AgentId, k: protocol::Key) -> Effect {
        Effect::Send(Request::Input {
            id,
            event: InputEvent::Key(k),
        })
    }

    pub(super) fn two_agents() -> (App, AgentInfo, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        (app, a, b)
    }

    pub(super) fn known_of(project: &Project) -> KnownProject {
        KnownProject {
            name: project.name.clone(),
            path: project.path.clone(),
        }
    }

    /// One open project `p` with an agent, and `old`, added in an earlier session.
    pub(super) fn with_known_old() -> (App, Project, KnownProject) {
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
    fn config_warnings_open_a_popup_with_one_warning_per_line() {
        let config = Config {
            warnings: vec![
                "unknown table [x]".into(),
                "config.toml: line 3: expected a value\nbroken = ".into(),
            ],
            ..Config::default()
        };
        let mut app = App::new("/w".into(), "/bin/sh".into(), None, config);
        assert_eq!(
            app.popup,
            Some(Popup::Output {
                title: "Config warnings".into(),
                lines: vec![
                    "unknown table [x]".into(),
                    "config.toml: line 3: expected a value".into(),
                    "broken = ".into(),
                ],
                scroll: 0,
            })
        );
        assert_eq!(app.message, None);
        let _ = app.on_key(key('x'));
        assert!(app.popup.is_some());
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.popup, None);
        let app = App::new("/w".into(), "/bin/sh".into(), None, Config::default());
        assert_eq!((app.popup, app.message), (None, None));
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
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("add a project first (P)")
        );
        let config = configured("[keys.sidebar]\nopen = \"ctrl-o\"\n");
        let p = project("p");
        let (mut app, _) = loaded_with(
            config,
            vec![p.clone()],
            vec![agent(&p, "a", Status::Stopped)],
        );
        let areas = screen_areas(&app);
        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 1, 1)), &areas);
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("a is not running — ctrl-o starts it")
        );
    }

    pub(super) fn submit_shell_agent(app: &mut App, name: &str) -> (AgentInfo, Request) {
        let project = app.selected_project().map(|p| p.id).expect("project");
        let _ = app.on_key(key('n'));
        type_text(app, name);
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Right));
        let request = Request::CreateAgent(created(app.on_key(code(KeyCode::Enter))));
        let info = AgentInfo {
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
            usage: None,
        };
        (info, request)
    }

    #[test]
    fn created_agent_is_selected_and_focused_by_the_create_response() {
        let (mut app, a, _) = two_agents();
        let (info, request) = submit_shell_agent(&mut app, "zed");
        let _ = app.apply_server(ServerMessage::AgentUpdated(info.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        assert_eq!(app.focus, Focus::Agents);
        let effects = app.apply_reply(&request, Ok(Response::Agent(info.clone())));
        assert_eq!(app.popup, None);
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
        let (_, request) = submit_shell_agent(&mut app, "alpha");
        let _ = app.apply_reply(&request, Err("an agent named alpha already exists".into()));
        let _ = app.on_key(code(KeyCode::Esc));
        let waiting = AgentInfo {
            status: Status::NeedsInput,
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(waiting.clone()));
        let _ = app.apply_response(Response::Agent(waiting));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn create_response_after_moving_does_not_steal_the_selection_or_focus() {
        let (mut app, _, b) = two_agents();
        let (info, request) = submit_shell_agent(&mut app, "zed");
        let _ = app.on_key(code(KeyCode::Esc));
        let _ = app.on_key(key('j'));
        let _ = app.apply_server(ServerMessage::AgentUpdated(info.clone()));
        let _ = app.apply_reply(&request, Ok(Response::Agent(info)));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(app.focus, Focus::Agents);
    }

    fn sent(effects: &[Effect]) -> Request {
        match effects {
            [Effect::Send(request)] => request.clone(),
            other => panic!("expected one request: {other:?}"),
        }
    }

    pub(super) fn submission(app: &App) -> Submission {
        match &app.popup {
            Some(Popup::NewAgent(form)) => form.submission.clone(),
            Some(Popup::AddProject(form)) => form.submission.clone(),
            Some(Popup::Rename { submission, .. }) => submission.clone(),
            other => panic!("no form: {other:?}"),
        }
    }

    #[test]
    fn a_refused_new_agent_keeps_the_form_with_the_input_and_the_error() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "dup");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        assert_eq!(
            submission(&app),
            Submission::Sending(Box::new(request.clone()))
        );
        type_text(&mut app, "x");
        let _ = app.on_paste("pasted".into());
        assert_eq!(form(&app).name, "dup");

        let refusal = "an agent named dup already exists";
        let _ = app.apply_reply(&request, Err(refusal.into()));
        assert_eq!(form(&app).name, "dup");
        assert_eq!(submission(&app), Submission::Failed(refusal.into()));
        assert_eq!(app.message, None);

        type_text(&mut app, "2");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let Request::CreateAgent(create) = &request else {
            panic!("{request:?}")
        };
        assert_eq!(create.name, "dup2");
        let info = agent(&app.projects[0], "dup2", Status::Starting);
        let _ = app.apply_reply(&request, Ok(Response::Agent(info.clone())));
        assert_eq!(app.popup, None);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(info.id));
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn escape_while_a_form_waits_leaves_its_reply_to_the_status_line() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        type_text(&mut app, "dup");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let _ = app.on_key(code(KeyCode::Esc));
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('n'));
        let _ = app.apply_reply(&request, Err("refused".into()));
        assert_eq!(submission(&app), Submission::Editing);
        assert_eq!(
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("refused")
        );
    }

    #[test]
    fn a_refused_rename_keeps_the_popup_with_the_error() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(key('R'));
        type_text(&mut app, "2");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let refusal = "an agent named alpha2 already exists";
        let _ = app.apply_reply(&request, Err(refusal.into()));
        assert!(
            matches!(&app.popup, Some(Popup::Rename { input, .. }) if input == "alpha2"),
            "{:?}",
            app.popup
        );
        assert_eq!(submission(&app), Submission::Failed(refusal.into()));
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let renamed = AgentInfo {
            name: "alpha2".into(),
            ..a
        };
        let _ = app.apply_reply(&request, Ok(Response::Agent(renamed)));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn a_refused_project_keeps_the_form_with_the_error() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('a'));
        let _ = app.on_key(ctrl('u'));
        type_text(&mut app, "nowhere");
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let refusal = "/work/nowhere: No such file or directory";
        let _ = app.apply_reply(&request, Err(refusal.into()));
        assert!(
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.path == "nowhere"),
            "{:?}",
            app.popup
        );
        assert_eq!(submission(&app), Submission::Failed(refusal.into()));
        let request = sent(&app.on_key(code(KeyCode::Enter)));
        let added = project("nowhere");
        let _ = app.apply_reply(&request, Ok(Response::Project(added.clone())));
        assert_eq!(app.popup, None);
        assert_eq!(app.selected_project().map(|p| p.id), Some(added.id));
    }

    #[test]
    fn agent_reply_never_overwrites_newer_state_from_an_event() {
        let (mut app, _, b) = two_agents();
        let waiting = AgentInfo {
            status: Status::NeedsInput,
            ..b.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(waiting));
        let stale = AgentInfo {
            status: Status::Starting,
            ..b.clone()
        };
        let _ = app.apply_response(Response::Agent(stale));
        let status = app.agents.iter().find(|x| x.id == b.id).map(|x| x.status);
        assert_eq!(status, Some(Status::NeedsInput));
    }

    #[test]
    fn agent_reply_for_a_removed_agent_does_not_bring_it_back() {
        let (mut app, a, _) = two_agents();
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        let _ = app.apply_response(Response::Agent(a.clone()));
        assert!(app.agents.iter().all(|x| x.id != a.id));
    }

    #[test]
    fn create_reply_without_a_preceding_event_adds_and_selects_the_agent() {
        let (mut app, _, _) = two_agents();
        let (info, request) = submit_shell_agent(&mut app, "zed");
        let working = AgentInfo {
            status: Status::Working,
            ..info.clone()
        };
        let _ = app.apply_reply(&request, Ok(Response::Agent(working)));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(info.id));
        assert_eq!(app.focus, Focus::Pane);
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

    pub(super) fn update(app: &mut App, info: &AgentInfo, status: Status) {
        let _ = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status,
            ..info.clone()
        }));
    }

    pub(super) fn finish_turn(app: &mut App, info: &AgentInfo) {
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
            worktree: Some(Box::new(protocol::WorktreeInfo {
                path: format!("/s/worktrees/{name}").into(),
                branch: format!("lazyagents/{name}"),
                base: "main".into(),
            })),
            ..agent(project, name, Status::Idle)
        }
    }

    #[test]
    fn a_git_repo_is_found_from_any_directory_inside_it() {
        let dir = tempfile::tempdir().expect("dir");
        let repo = dir.path().join("repo");
        let nested = repo.join("src/deep");
        std::fs::create_dir_all(&nested).expect("dirs");
        if in_git_repo(dir.path()) {
            eprintln!("the temp dir is inside a git repository; not checking a path outside one");
        } else {
            assert!(!in_git_repo(&repo));
        }
        std::fs::create_dir(repo.join(".git")).expect(".git");
        assert!(in_git_repo(&repo));
        assert!(in_git_repo(&nested));
        let linked = dir.path().join("linked");
        std::fs::create_dir(&linked).expect("linked");
        std::fs::write(linked.join(".git"), "gitdir: ../repo/.git\n").expect(".git file");
        assert!(in_git_repo(&linked));
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
        app.request_failed(Failure::DirtyWorktree {
            files: vec!["evil\u{1b}[2J\u{7}.txt".into(), "tab\there".into()],
        });
        let Some(Popup::Confirm { prompt, .. }) = &app.popup else {
            panic!("no confirm: {:?}", app.popup);
        };
        assert_eq!(
            prompt,
            "Worktree has uncommitted changes:\nevil[2J.txt\ntab    here\nRemove anyway? y/n"
        );
    }

    #[test]
    fn only_a_dirty_worktree_refusal_offers_to_force() {
        let p = project("p");
        let wt = worktree_agent(&p, "wt");
        let (mut app, _) = loaded(vec![p], vec![wt]);
        let _ = app.on_key(key('d'));
        let request = sent(&app.on_key(key('y')));
        let lookalike = "worktree has uncommitted changes: git status failed";
        let _ = app.apply_reply(&request, Err(lookalike.into()));
        assert_eq!(app.popup, None);
        assert_eq!(app.current_message(Instant::now()), Some(lookalike));
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

        app.request_failed(Failure::DirtyWorktree {
            files: vec!["f.txt".into()],
        });
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
            Failure::DirtyWorktree { files }
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
                delete_branch: false,
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

    pub(super) fn names(app: &App) -> Vec<String> {
        app.visible_agents()
            .iter()
            .map(|a| a.name.clone())
            .collect()
    }

    pub(super) fn filter_app() -> App {
        let p = project("p");
        let api = AgentInfo {
            worktree: Some(Box::new(protocol::WorktreeInfo {
                path: "/w/api".into(),
                branch: "lazyagents/api".into(),
                base: "main".into(),
            })),
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

    pub(super) fn screen_areas(app: &App) -> Areas {
        crate::ui::layout(ratatui::layout::Rect::new(0, 0, 100, 30), app)
    }

    pub(super) fn mouse(kind: MouseKind, (col, row): (u16, u16)) -> MouseInput {
        MouseInput { kind, col, row }
    }

    pub(super) fn in_pane(areas: &Areas, col: u16, row: u16) -> (u16, u16) {
        (areas.pane_inner.x + col, areas.pane_inner.y + row)
    }

    pub(super) fn open_diff(app: &mut App, id: AgentId) {
        assert_eq!(
            app.on_key(key('D')),
            vec![Effect::Send(Request::GitDiff {
                id,
                scope: DiffScope::All
            })]
        );
        assert!(
            matches!(app.focus.clone(), Focus::Diff(_)),
            "{:?}",
            app.focus
        );
    }

    pub(super) fn created(effects: Vec<Effect>) -> CreateAgent {
        match effects.as_slice() {
            [Effect::Send(Request::CreateAgent(c))] => c.clone(),
            other => panic!("unexpected effects {other:?}"),
        }
    }

    pub(super) fn configured(text: &str) -> Config {
        crate::config::parse(text).expect("config parses")
    }

    pub(super) fn form(app: &App) -> NewAgentForm {
        match &app.popup {
            Some(Popup::NewAgent(form)) => form.clone(),
            other => panic!("no form: {other:?}"),
        }
    }

    pub(super) fn scroll_effects(id: AgentId, lines: i32) -> Vec<Effect> {
        vec![Effect::Send(Request::Scroll { id, lines })]
    }

    pub(super) fn running_pair() -> (App, AgentInfo, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b.clone()]);
        let _ = app.set_pane_size(80, 10);
        (app, a, b)
    }
}
