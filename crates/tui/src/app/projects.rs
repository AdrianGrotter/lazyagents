use protocol::ProjectId;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::keymap::Action;

use super::forms::NewAgentForm;
use super::{AgentView, App, Confirmed, Effect, Focus, Popup, wrap};

impl App {
    pub(super) fn open_switcher(&mut self) {
        let index = match (self.view, self.selected_project()) {
            (AgentView::Project, Some(_)) => self.project_index + 1,
            _ => 0,
        };
        self.popup = Some(Popup::ProjectSwitcher { index });
    }

    pub(super) fn open_add_project(&mut self) {
        self.popup = Some(if self.project_candidates().is_empty() {
            self.project_form()
        } else {
            Popup::ProjectPicker { index: 0 }
        });
    }

    pub(super) fn on_switcher_key(&mut self, index: usize, key: &KeyEvent) -> Vec<Effect> {
        let rows = self.projects.len() + 2;
        let project = index
            .checked_sub(1)
            .and_then(|i| self.projects.get(i))
            .map(|p| p.id);
        let index = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
            KeyCode::Enter if index == 0 => return self.switch_to(None),
            KeyCode::Enter if project.is_some() => return self.switch_to(Some(index - 1)),
            KeyCode::Enter => {
                self.open_add_project();
                return Vec::new();
            }
            KeyCode::Char('j') | KeyCode::Down => wrap(index, 1, rows),
            KeyCode::Char('k') | KeyCode::Up => wrap(index, -1, rows),
            KeyCode::Char('d') => {
                self.popup = Some(Popup::ProjectSwitcher { index });
                if let Some(id) = project {
                    self.confirm_remove_project(id);
                }
                return Vec::new();
            }
            _ => index,
        };
        self.popup = Some(Popup::ProjectSwitcher { index });
        Vec::new()
    }

    fn switch_to(&mut self, index: Option<usize>) -> Vec<Effect> {
        self.switch_project(index);
        self.focus = Focus::Agents;
        self.sync()
    }

    /// A project with agents is refused, leaving the open popup as it is.
    pub(super) fn confirm_remove_project(&mut self, id: ProjectId) {
        let Some(project) = self.projects.iter().find(|p| p.id == id) else {
            return;
        };
        if self.agents.iter().any(|a| a.project == id) {
            let message = format!("remove {}'s agents first", project.name);
            self.fail(message);
            return;
        }
        let prompt = format!(
            "Remove project {}? It stays in the add list ({}).",
            project.name,
            self.keymap.first_key(Action::AddProject)
        );
        let action = Confirmed::RemoveProject(id);
        self.popup = Some(Popup::Confirm { prompt, action });
    }

    /// The list starts at the current project, or in the all-agents view at the project last
    /// picked for a new agent.
    pub(super) fn open_new_agent(&mut self) {
        if self.projects.is_empty() {
            let add = self.keymap.first_key(Action::AddProject);
            self.fail(format!("add a project first ({add})"));
            return;
        }
        let current = self.selected_project().map(|p| p.id);
        let start = match self.view {
            AgentView::Project => current,
            AgentView::All => self.new_agent_project.or(current),
        };
        let index = start
            .and_then(|id| self.projects.iter().position(|p| p.id == id))
            .unwrap_or(0);
        self.popup = Some(Popup::NewAgentProject { index });
    }

    pub(super) fn on_new_agent_project_key(&mut self, index: usize, key: &KeyEvent) {
        let rows = self.projects.len();
        let index = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return,
            KeyCode::Enter => {
                if let Some(project) = self.projects.get(index) {
                    self.new_agent_project = Some(project.id);
                    let outside_git = !(self.is_git_repo)(&project.path);
                    self.popup = Some(Popup::NewAgent(NewAgentForm::new(project.id, outside_git)));
                }
                return;
            }
            KeyCode::Char('j') | KeyCode::Down if rows > 0 => wrap(index, 1, rows),
            KeyCode::Char('k') | KeyCode::Up if rows > 0 => wrap(index, -1, rows),
            _ => index,
        };
        self.popup = Some(Popup::NewAgentProject { index });
    }
}
