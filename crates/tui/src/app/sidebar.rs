use protocol::{AgentId, AgentInfo, DiffScope, ProjectId, Request, Status};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::keymap::Action;

use super::forms::{NewAgentForm, Submission};
use super::pane::{press, scroll_request};
use super::{AgentView, App, Confirmed, DEFAULT_SIZE, Effect, Focus, Help, Popup, label, wrap};

impl App {
    pub(super) fn on_sidebar_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.code == KeyCode::Esc && !self.filter.is_empty() {
            return self.set_filter(String::new());
        }
        if let Some(index) = self.command_key(&key) {
            return self.start_command(index);
        }
        let Some(action) = self.keymap.sidebar(&key) else {
            return Vec::new();
        };
        if let Some(reason) = self.unavailable(action) {
            self.fail(reason);
            return Vec::new();
        }
        match action {
            Action::Quit => return vec![Effect::Quit],
            Action::StopDaemon => {
                self.popup = Some(Popup::Confirm {
                    prompt: "Stop the daemon and every agent?".into(),
                    action: Confirmed::StopDaemon,
                });
            }
            Action::Help => self.popup = Some(Popup::Help(Help::default())),
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
            Action::Open => match (&self.focus, self.selected_agent()) {
                (Focus::Projects, _) => self.focus = Focus::Agents,
                (_, Some(agent)) if agent.status.is_running() => self.focus = Focus::Pane,
                (_, Some(agent)) => {
                    let id = agent.id;
                    self.open_on_start = Some(id);
                    return vec![self.restart(id)];
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
                    return self.show_diff(id, DiffScope::All);
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
                        submission: Submission::Editing,
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
                self.settled_row = 0;
                if let Some(id) = selected {
                    self.select_agent(id);
                }
            }
            Action::Scroll => self.enter_scroll(false),
            Action::Commands => self.popup = Some(Popup::Commands { index: 0 }),
            Action::Finish => self.open_finish(),
            Action::Approve => {
                if let Some(agent) = self.selected_agent() {
                    return vec![Effect::Send(Request::Approve { id: agent.id })];
                }
            }
            Action::Deny => {
                if let Some(agent) = self.selected_agent() {
                    return vec![press(agent.id, protocol::KeyCode::Esc)];
                }
            }
            Action::Reply => {
                if let Some(agent) = self.selected_agent() {
                    self.popup = Some(Popup::Reply {
                        agent: agent.id,
                        input: String::new(),
                    });
                }
            }
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
            Action::ScreenMode => self.screen_mode = self.screen_mode.next(),
            Action::PaneNext
            | Action::PanePrevious
            | Action::PaneLeave
            | Action::PaneScroll
            | Action::PaneSendPrefix
            | Action::PaneScreenMode => {}
        }
        self.sync()
    }

    pub(super) fn restart(&self, id: AgentId) -> Effect {
        let (cols, rows) = self.pane_size.unwrap_or(DEFAULT_SIZE);
        Effect::Send(Request::RestartAgent { id, cols, rows })
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
            Focus::Agents | Focus::Pane | Focus::Diff(_) | Focus::Scroll(_) => {
                self.move_agent(step)
            }
        }
    }

    pub(super) fn move_agent(&mut self, step: i32) {
        let count = self.visible_agents().len();
        if count == 0 {
            return;
        }
        let row = self.selected_row().unwrap_or(self.settled_row);
        self.select_row(wrap(row, step, count));
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
        let outside_git = self
            .selected_project()
            .is_some_and(|p| !(self.is_git_repo)(&p.path));
        self.popup = Some(Popup::NewAgent(NewAgentForm {
            outside_git,
            ..NewAgentForm::default()
        }));
    }

    /// Whether the next-attention key has an agent to go to besides the selected one.
    pub fn others_need_attention(&self) -> bool {
        self.next_attention_target().is_some()
    }

    /// The agent the next-attention key goes to: after the selection in list order, the
    /// first other agent that needs input, else the first other one that is done.
    fn next_attention_target(&self) -> Option<AgentId> {
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
        others
            .iter()
            .find(|a| a.status == Status::NeedsInput)
            .or_else(|| others.iter().find(|a| self.unseen.contains(&a.id)))
            .map(|a| a.id)
    }

    fn next_attention(&mut self) {
        let current_needs_input = self
            .selected_agent()
            .is_some_and(|a| a.status == Status::NeedsInput);
        match self.next_attention_target() {
            Some(id) => self.select_agent(id),
            None if current_needs_input => self.fail("no other agent needs attention"),
            None => self.fail("no agent needs attention"),
        }
    }

    /// Why `action` cannot act on the selection, or `None` when it can. The help popup dims
    /// such actions, and pressing one shows the reason instead.
    pub fn unavailable(&self, action: Action) -> Option<String> {
        let on_project = self.focus == Focus::Projects;
        let needs_agent = match action {
            Action::Open | Action::Remove => !on_project,
            Action::Stop
            | Action::Restart
            | Action::Rename
            | Action::Diff
            | Action::Scroll
            | Action::PeekDown
            | Action::PeekUp
            | Action::Approve
            | Action::Deny
            | Action::Reply => true,
            _ => false,
        };
        if !needs_agent {
            return None;
        }
        let Some(agent) = self.selected_agent() else {
            return Some("no agent selected".to_string());
        };
        let name = label(agent);
        let open = self.keymap.first_key(Action::Open);
        match action {
            Action::Stop | Action::Reply if !agent.status.is_running() => {
                Some(format!("{name} is not running"))
            }
            Action::Scroll | Action::PeekDown | Action::PeekUp
                if agent.status == Status::Stopped =>
            {
                Some(format!("{name} has no terminal yet — {open} starts it"))
            }
            Action::Approve | Action::Deny if agent.status != Status::NeedsInput => {
                Some(format!("{name} is not waiting for an answer"))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        agent, code, created, finish_turn, key, loaded, project, two_agents, type_text, update,
    };
    use protocol::ServerMessage;
    use std::time::Instant;

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
            (app.focus.clone(), app.selected_project().map(|x| x.id)),
            (Focus::Projects, Some(q.id))
        );
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(
            (app.focus.clone(), app.selected_project().map(|x| x.id)),
            (Focus::Projects, Some(p.id))
        );

        let _ = app.on_key(code(KeyCode::Right));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
        let _ = app.on_key(code(KeyCode::Down));
        assert_eq!(
            (app.focus.clone(), app.selected_agent().map(|x| x.id)),
            (Focus::Agents, Some(b.id))
        );
        let _ = app.on_key(code(KeyCode::Up));
        assert_eq!(
            (app.focus.clone(), app.selected_agent().map(|x| x.id)),
            (Focus::Agents, Some(a.id))
        );
        assert_eq!(app.selected_project().map(|x| x.id), Some(p.id));
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
                delete_branch: false,
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
    fn number_keys_focus_the_sidebar_boxes() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('2'));
        assert_eq!(app.focus, Focus::Agents);
        let _ = app.on_key(key('1'));
        assert_eq!(app.focus, Focus::Projects);
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

    #[test]
    fn enter_on_an_exited_agent_restarts_it_and_opens_the_pane_once_it_runs() {
        for status in [Status::Exited(0), Status::Stopped] {
            let p = project("p");
            let a = agent(&p, "alpha", status);
            let (mut app, _) = loaded(vec![p], vec![a.clone()]);
            let _ = app.set_pane_size(80, 20);
            assert_eq!(
                app.on_key(code(KeyCode::Enter)),
                vec![Effect::Send(Request::RestartAgent {
                    id: a.id,
                    cols: 80,
                    rows: 20
                })],
                "{status:?}"
            );
            assert_eq!(app.focus, Focus::Agents, "{status:?}");
            let started = AgentInfo {
                status: Status::Starting,
                ..a.clone()
            };
            let _ = app.apply_server(ServerMessage::AgentUpdated(started));
            assert_eq!(app.focus, Focus::Pane, "{status:?}");
        }
    }

    #[test]
    fn a_restart_from_enter_opens_the_pane_only_for_its_own_start() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Exited(0));
        let (mut app, _) = loaded(vec![p.clone()], vec![a.clone()]);
        let _ = app.on_key(code(KeyCode::Enter));
        let failed = AgentInfo {
            status: Status::Exited(127),
            last_error: Some("not found".into()),
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(failed));
        let started = AgentInfo {
            status: Status::Starting,
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(started.clone()));
        assert_eq!(app.focus, Focus::Agents);

        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a, b]);
        let _ = app.on_key(code(KeyCode::Enter));
        let _ = app.on_key(key('j'));
        let _ = app.apply_server(ServerMessage::AgentUpdated(started));
        assert_eq!(app.focus, Focus::Agents);
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
        let _ = app.on_key(code(KeyCode::Esc));
        let _ = app.on_key(key('A'));
        assert_eq!(app.view, AgentView::Project);
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    use crate::app::tests::{agent, loaded, project};
    use protocol::{InputEvent, Key as ProtoKey, KeyCode as ProtoCode, Modifiers};
    use ratatui::crossterm::event::KeyModifiers;
    use std::time::Instant;

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
        assert_eq!(
            app.on_key(key('y')),
            vec![Effect::Send(Request::Approve { id: a.id })]
        );
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
mod screen_mode_tests {
    use super::*;
    use crate::app::ScreenMode;
    use crate::app::tests::{agent, loaded, project};
    use ratatui::crossterm::event::KeyModifiers;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn plus_cycles_the_screen_mode_from_the_sidebar_and_the_pane() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "a", Status::Idle)]);
        assert_eq!(app.screen_mode, ScreenMode::Normal);
        for expected in [ScreenMode::Wide, ScreenMode::Full, ScreenMode::Normal] {
            assert!(app.on_key(key('+')).is_empty());
            assert_eq!(app.screen_mode, expected);
        }
        let _ = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.focus, Focus::Pane);
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
        assert!(app.on_key(key('+')).is_empty());
        assert_eq!(
            (app.screen_mode, app.focus.clone()),
            (ScreenMode::Wide, Focus::Pane)
        );
    }
}
