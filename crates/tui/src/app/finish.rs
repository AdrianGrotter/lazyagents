use protocol::{AgentId, Request};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::commands;

use super::{App, Confirmed, Effect, Popup, WorktreeRemoval, label, wrap};

/// A way to finish a worktree agent's work, from the finish menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishChoice {
    Merge,
    PullRequest,
    Remove,
    RemoveWithBranch,
}

impl App {
    /// The finish menu's choices for the selected agent, which must have a worktree.
    pub fn finish_choices(&self) -> Vec<FinishChoice> {
        let mut choices = vec![FinishChoice::Merge];
        if self.gh {
            choices.push(FinishChoice::PullRequest);
        }
        choices.extend([FinishChoice::Remove, FinishChoice::RemoveWithBranch]);
        choices
    }

    pub fn finish_label(&self, choice: FinishChoice) -> String {
        let worktree = self.selected_agent().and_then(|a| a.worktree.as_ref());
        let (branch, base) = worktree.map_or(("the branch", "its base"), |w| {
            (w.branch.as_str(), w.base.as_str())
        });
        match choice {
            FinishChoice::Merge => format!("merge {branch} into {base}"),
            FinishChoice::PullRequest => format!("push {branch} and open a pull request"),
            FinishChoice::Remove => {
                format!("remove the agent and its worktree, keep {branch}")
            }
            FinishChoice::RemoveWithBranch => {
                format!("remove the agent, its worktree and {branch} (merged only)")
            }
        }
    }

    pub(super) fn open_finish(&mut self) {
        let Some(agent) = self.selected_agent() else {
            return;
        };
        if agent.worktree.is_none() {
            let message = format!("{} has no worktree to finish", label(agent));
            self.fail(message);
            return;
        }
        self.popup = Some(Popup::Finish {
            agent: agent.id,
            index: 0,
        });
    }

    /// The removing choices ask for confirmation first, like `d`.
    pub(super) fn on_finish_key(
        &mut self,
        agent: AgentId,
        index: usize,
        key: KeyEvent,
    ) -> Vec<Effect> {
        let choices = self.finish_choices();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.popup = Some(Popup::Finish {
                    agent,
                    index: wrap(index, 1, choices.len()),
                });
                Vec::new()
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.popup = Some(Popup::Finish {
                    agent,
                    index: wrap(index, -1, choices.len()),
                });
                Vec::new()
            }
            KeyCode::Enter => match choices.get(index) {
                Some(choice @ (FinishChoice::Remove | FinishChoice::RemoveWithBranch)) => {
                    self.confirm_finish(agent, *choice);
                    Vec::new()
                }
                Some(choice) => self.finish(agent, *choice),
                None => Vec::new(),
            },
            KeyCode::Esc | KeyCode::Char('q') => Vec::new(),
            _ => {
                self.popup = Some(Popup::Finish { agent, index });
                Vec::new()
            }
        }
    }

    fn confirm_finish(&mut self, id: AgentId, choice: FinishChoice) {
        let Some(agent) = self.agents.iter().find(|a| a.id == id) else {
            return;
        };
        let Some(worktree) = &agent.worktree else {
            return;
        };
        let (name, branch) = (&agent.name, &worktree.branch);
        let prompt = match choice {
            FinishChoice::RemoveWithBranch => {
                format!("Remove {name}, its worktree and {branch} (merged only)?")
            }
            _ => format!("Remove {name} and its worktree, keep {branch}?"),
        };
        self.popup = Some(Popup::Confirm {
            prompt,
            action: Confirmed::Finish(id, choice),
        });
    }

    pub(super) fn finish(&mut self, id: AgentId, choice: FinishChoice) -> Vec<Effect> {
        let Some(agent) = self.agents.iter().find(|a| a.id == id) else {
            return Vec::new();
        };
        let Some(worktree) = agent.worktree.clone() else {
            return Vec::new();
        };
        match choice {
            FinishChoice::Merge => {
                self.inform(format!(
                    "merging {} into {}…",
                    worktree.branch, worktree.base
                ));
                vec![Effect::Send(Request::MergeAgent { id })]
            }
            FinishChoice::PullRequest => {
                let branch = commands::shell_quote(&worktree.branch);
                let base = commands::shell_quote(&worktree.base);
                let base_ref = commands::shell_quote(&format!("refs/heads/{}", worktree.base));
                vec![Effect::RunInTerminal {
                    title: "pull request".into(),
                    script: format!(
                        "git push -u origin {branch} && if git show-ref --verify --quiet {base_ref}; \
                         then gh pr create --fill --head {branch} --base {base}; \
                         else gh pr create --fill --head {branch}; fi"
                    ),
                    cwd: worktree.path.clone(),
                }]
            }
            FinishChoice::Remove => self.remove(id, WorktreeRemoval::Remove),
            FinishChoice::RemoveWithBranch => {
                let done = format!("removed {}, its worktree and branch", agent.name);
                self.removals.insert(id, done);
                vec![Effect::Send(Request::RemoveAgent {
                    id,
                    remove_worktree: true,
                    force: false,
                    delete_branch: true,
                })]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::MessageKind;
    use crate::app::tests::{agent, loaded, project};
    use protocol::{AgentInfo, Response, ServerMessage, Status};
    use ratatui::crossterm::event::KeyModifiers;
    use std::time::Instant;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    fn worktree_app(gh: bool) -> (App, AgentInfo) {
        let p = project("p");
        let a = AgentInfo {
            worktree: Some(Box::new(protocol::WorktreeInfo {
                path: "/w/api".into(),
                branch: "lazyagents/api".into(),
                base: "main".into(),
            })),
            cwd: "/w/api".into(),
            ..agent(&p, "api", Status::Idle)
        };
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        app.gh = gh;
        (app, a)
    }

    fn choices(app: &App) -> Vec<String> {
        app.finish_choices()
            .iter()
            .map(|choice| app.finish_label(*choice))
            .collect()
    }

    #[test]
    fn f_offers_the_ways_to_finish_a_worktree_agent() {
        let (mut app, _) = worktree_app(true);
        let _ = app.on_key(key('F'));
        assert!(matches!(app.popup, Some(Popup::Finish { index: 0, .. })));
        assert_eq!(
            choices(&app),
            [
                "merge lazyagents/api into main",
                "push lazyagents/api and open a pull request",
                "remove the agent and its worktree, keep lazyagents/api",
                "remove the agent, its worktree and lazyagents/api (merged only)",
            ]
        );
        let (app, _) = worktree_app(false);
        assert!(!choices(&app).iter().any(|c| c.contains("pull request")));
    }

    #[test]
    fn each_choice_sends_its_request() {
        let (mut app, a) = worktree_app(true);
        let _ = app.on_key(key('F'));
        assert_eq!(
            app.on_key(enter()),
            vec![Effect::Send(Request::MergeAgent { id: a.id })]
        );
        assert_eq!(app.popup, None);

        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        assert_eq!(
            app.on_key(enter()),
            vec![Effect::RunInTerminal {
                title: "pull request".into(),
                script: "git push -u origin 'lazyagents/api' && if git show-ref --verify --quiet 'refs/heads/main'; then gh pr create --fill --head 'lazyagents/api' --base 'main'; else gh pr create --fill --head 'lazyagents/api'; fi"
                    .into(),
                cwd: "/w/api".into(),
            }]
        );
    }

    fn confirm_prompt(app: &App) -> String {
        match &app.popup {
            Some(Popup::Confirm { prompt, .. }) => prompt.clone(),
            other => panic!("not confirming: {other:?}"),
        }
    }

    #[test]
    fn removing_from_the_finish_menu_asks_first_and_reports_success() {
        let (mut app, a) = worktree_app(false);
        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        assert!(app.on_key(enter()).is_empty());
        assert_eq!(
            confirm_prompt(&app),
            "Remove api and its worktree, keep lazyagents/api?"
        );
        assert!(app.on_key(key('n')).is_empty());
        assert_eq!(app.popup, None);

        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('j'));
        assert!(app.on_key(enter()).is_empty());
        assert_eq!(
            confirm_prompt(&app),
            "Remove api, its worktree and lazyagents/api (merged only)?"
        );
        let removal = Request::RemoveAgent {
            id: a.id,
            remove_worktree: true,
            force: false,
            delete_branch: true,
        };
        assert_eq!(app.on_key(key('y')), vec![Effect::Send(removal.clone())]);
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        let _ = app.apply_reply(&removal, Ok(Response::Ok));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("removed api, its worktree and branch")
        );
        assert_eq!(
            app.message.as_ref().map(|m| m.kind),
            Some(MessageKind::Info)
        );
    }

    #[test]
    fn removing_a_worktree_reports_the_branch_it_kept() {
        let (mut app, a) = worktree_app(false);
        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        let _ = app.on_key(enter());
        let removal = Request::RemoveAgent {
            id: a.id,
            remove_worktree: true,
            force: false,
            delete_branch: false,
        };
        assert_eq!(app.on_key(enter()), vec![Effect::Send(removal.clone())]);
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        let _ = app.apply_reply(&removal, Ok(Response::Ok));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("removed api and its worktree, kept branch lazyagents/api")
        );
    }

    #[test]
    fn a_refused_removal_reports_only_the_refusal() {
        let (mut app, a) = worktree_app(false);
        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        let _ = app.on_key(key('j'));
        let _ = app.on_key(enter());
        let effects = app.on_key(key('y'));
        let [Effect::Send(removal)] = effects.as_slice() else {
            panic!("{effects:?}")
        };
        let _ = app.apply_reply(
            removal,
            Err("lazyagents/api is not merged into main".into()),
        );
        let _ = app.apply_reply(removal, Ok(Response::Ok));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("lazyagents/api is not merged into main")
        );
        assert!(app.agents.iter().any(|x| x.id == a.id));
    }

    #[test]
    fn the_pull_request_script_quotes_the_base() {
        let (mut app, a) = worktree_app(true);
        app.agents
            .iter_mut()
            .find(|x| x.id == a.id)
            .and_then(|x| x.worktree.as_mut())
            .unwrap()
            .base = "it's".into();
        let _ = app.on_key(key('F'));
        let _ = app.on_key(key('j'));
        assert_eq!(
            app.on_key(enter()),
            vec![Effect::RunInTerminal {
                title: "pull request".into(),
                script: r#"git push -u origin 'lazyagents/api' && if git show-ref --verify --quiet 'refs/heads/it'\''s'; then gh pr create --fill --head 'lazyagents/api' --base 'it'\''s'; else gh pr create --fill --head 'lazyagents/api'; fi"#
                    .into(),
                cwd: "/w/api".into(),
            }]
        );
    }

    #[test]
    fn finishing_needs_a_worktree_agent() {
        let p = project("p");
        let (mut app, _) = loaded(vec![p.clone()], vec![agent(&p, "plain", Status::Idle)]);
        assert!(app.on_key(key('F')).is_empty());
        assert_eq!(app.popup, None);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("plain has no worktree to finish")
        );
    }

    #[test]
    fn a_finished_operation_is_reported() {
        let (mut app, _) = worktree_app(false);
        let _ = app.apply_response(Response::Done("merged lazyagents/api into main".into()));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("merged lazyagents/api into main")
        );
    }
}
