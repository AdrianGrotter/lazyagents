use std::path::PathBuf;
use std::time::{Duration, Instant};

use protocol::{
    AgentId, AgentInfo, Project, ProjectId, Request, Response, Screen, ServerMessage, Status,
};

pub const MESSAGE_TTL: Duration = Duration::from_secs(5);

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
    pending_select: Option<(ProjectId, String)>,
    subscribed_running: bool,
    #[expect(dead_code)]
    cwd: PathBuf,
    #[expect(dead_code)]
    shell: String,
}

impl App {
    pub fn new(cwd: PathBuf, shell: String) -> Self {
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
            subscribed_running: false,
            cwd,
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

    pub fn fail(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), Instant::now()));
    }

    pub fn apply_server(&mut self, message: ServerMessage) -> Vec<Effect> {
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
        self.sync()
    }

    pub fn apply_response(&mut self, response: Response) -> Vec<Effect> {
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
                    self.project_index = index;
                    self.agent_index = 0;
                }
            }
            Response::Agent(info) => self.upsert_agent(info),
            Response::Ok => {}
        }
        self.sync()
    }

    pub fn set_pane_size(&mut self, cols: u16, rows: u16) -> Vec<Effect> {
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

    fn replace_projects(&mut self, projects: Vec<Project>) {
        let selected = self.selected_project().map(|p| p.id);
        self.projects = projects;
        if let Some(index) = selected.and_then(|id| self.projects.iter().position(|p| p.id == id)) {
            self.project_index = index;
        }
    }

    fn upsert_agent(&mut self, info: AgentInfo) {
        let selected = self.selected_agent().map(|a| a.id);
        match self.agents.iter_mut().find(|a| a.id == info.id) {
            Some(existing) => *existing = info.clone(),
            None => self.agents.push(info.clone()),
        }
        let wanted = self
            .pending_select
            .as_ref()
            .is_some_and(|(project, name)| *project == info.project && *name == info.name);
        if wanted {
            self.pending_select = None;
            self.select_agent(info.id);
            self.focus = Focus::Pane;
        } else if let Some(id) = selected {
            self.select_agent(id);
        }
    }

    fn reselect(&mut self, selected: Option<AgentId>) {
        if let Some(id) = selected {
            self.select_agent(id);
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

    fn sync(&mut self) -> Vec<Effect> {
        self.project_index = self
            .project_index
            .min(self.projects.len().saturating_sub(1));
        let count = self.visible_agents().len();
        self.agent_index = self.agent_index.min(count.saturating_sub(1));
        let wanted = self.selected_agent().map(|a| (a.id, a.status.is_running()));
        if wanted.is_none() && self.focus == Focus::Pane {
            self.focus = Focus::Agents;
        }
        let running = wanted.is_some_and(|(_, running)| running);
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use protocol::{AgentKind, Cell, Frame, Row};

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
        }
    }

    pub(crate) fn loaded(projects: Vec<Project>, agents: Vec<AgentInfo>) -> (App, Vec<Effect>) {
        let mut app = App::new("/work".into(), "/bin/sh".into());
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
    fn subscribing_sends_the_known_pane_size() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let mut app = App::new("/work".into(), "/bin/sh".into());
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
        let mut app = App::new("/work".into(), "/bin/sh".into());
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
    fn pending_select_selects_and_focuses_the_new_agent_via_update() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone()], vec![a]);
        app.pending_select = Some((p.id, "aaa".into()));
        let new = agent(&p, "aaa", Status::Starting);
        let effects = app.apply_server(ServerMessage::AgentUpdated(new.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(new.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(app.pending_select, None);
        assert_eq!(
            effects,
            vec![Effect::Send(Request::Subscribe { id: new.id })]
        );
    }

    #[test]
    fn pending_select_selects_and_focuses_the_new_agent_via_response() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p.clone()], vec![a]);
        app.pending_select = Some((p.id, "aaa".into()));
        let new = agent(&p, "aaa", Status::Starting);
        let effects = app.apply_response(Response::Agent(new.clone()));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(new.id));
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(app.pending_select, None);
        assert_eq!(
            effects,
            vec![Effect::Send(Request::Subscribe { id: new.id })]
        );
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
}
