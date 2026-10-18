use protocol::{AgentId, InputEvent, Request};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::fixed_keys::{self, HelpKey};
use crate::help;
use crate::keymap::Action;

use super::forms::{Submission, normalize_name};
use super::pane::press;
use super::{App, Confirmed, DEFAULT_SIZE, Effect, Help, PendingRemove, Popup, WorktreeRemoval};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Edit {
    Continue,
    Submit,
    Cancel,
}

pub(super) fn edit(input: &mut String, key: &KeyEvent) -> Edit {
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
    /// How far the help popup can scroll on `screen`, and how many rows a page moves.
    pub fn help_scroll_limits(&self, help: &Help, screen: (u16, u16)) -> (usize, usize) {
        let layout = help::Layout::new(self, help, screen);
        (layout.max_scroll(), layout.room)
    }

    pub(super) fn on_popup_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(popup) = self.popup.take() else {
            return Vec::new();
        };
        match popup {
            Popup::Help(help) => {
                self.on_help_key(help, &key);
                Vec::new()
            }
            Popup::Commands { index } => self.on_commands_key(index, key),
            Popup::Finish { agent, index } => self.on_finish_key(agent, index, key),
            Popup::ApproveSetup {
                form,
                create,
                project,
                script,
                scroll,
            } => self.on_approve_setup_key(form, create, project, script, scroll, key),
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
            Popup::Rename {
                agent,
                mut input,
                submission,
            } => match edit(&mut input, &key) {
                Edit::Submit => {
                    let request = Request::RenameAgent {
                        id: agent,
                        name: normalize_name(&input),
                    };
                    self.popup = Some(Popup::Rename {
                        agent,
                        input,
                        submission: Submission::Sending(Box::new(request.clone())),
                    });
                    vec![Effect::Send(request)]
                }
                Edit::Cancel => Vec::new(),
                Edit::Continue => {
                    self.popup = Some(Popup::Rename {
                        agent,
                        input,
                        submission,
                    });
                    Vec::new()
                }
            },
            Popup::ProjectPicker { index } => self.on_picker_key(index, &key),
            Popup::AddProject(form) => self.on_project_form_key(form, &key),
            Popup::NewAgent(form) => self.on_form_key(form, key),
        }
    }

    /// After `/`, keys edit the filter until `enter` keeps it or `esc` clears it; otherwise
    /// they scroll, `esc` clears a kept filter, and the help key closes the popup like the
    /// table's close keys. A close key held with ctrl closes it even while typing.
    fn on_help_key(&mut self, mut help: Help, key: &KeyEvent) {
        let action = fixed_keys::help_key(key);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let closes = action == Some(HelpKey::Close) && (ctrl || !help.typing)
            || !help.typing && self.keymap.sidebar(key) == Some(Action::Help);
        let clears = !help.typing && key.code == KeyCode::Esc && !help.filter.is_empty();
        if closes && !clears {
            return;
        }
        if clears {
            help.filter.clear();
            help.scroll = 0;
        } else if help.typing {
            let before = help.filter.clone();
            match edit(&mut help.filter, key) {
                Edit::Submit => help.typing = false,
                Edit::Cancel => {
                    help.typing = false;
                    help.filter.clear();
                }
                Edit::Continue => {}
            }
            if help.filter != before {
                help.scroll = 0;
            }
        } else {
            match action {
                Some(HelpKey::Filter) => help.typing = true,
                Some(HelpKey::Move(motion)) => {
                    let screen = self.screen_size.unwrap_or(DEFAULT_SIZE);
                    let (last, page) = self.help_scroll_limits(&help, screen);
                    help.scroll = motion.position(help.scroll, page, last);
                }
                Some(HelpKey::Close) | None => {}
            }
        }
        self.popup = Some(Popup::Help(help));
    }

    /// `None` keeps the prompt open.
    fn on_confirm_key(&mut self, action: &Confirmed, key: &KeyEvent) -> Option<Vec<Effect>> {
        let effects = match (action, key.code) {
            (_, KeyCode::Char('n') | KeyCode::Esc) => Vec::new(),
            (Confirmed::RemoveWorktree(id), KeyCode::Char('y')) => {
                self.remove(*id, WorktreeRemoval::Remove)
            }
            (Confirmed::RemoveWorktree(id), KeyCode::Char('k')) => {
                self.remove(*id, WorktreeRemoval::Keep)
            }
            (Confirmed::ForceRemove(id), KeyCode::Char('y')) => {
                self.remove(*id, WorktreeRemoval::Force)
            }
            (Confirmed::RemoveWorktree(_) | Confirmed::ForceRemove(_), _) => return None,
            (Confirmed::Kill(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::Send(Request::KillAgent { id: *id })]
            }
            (Confirmed::Restart(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![self.restart(*id)]
            }
            (Confirmed::Remove(id), KeyCode::Char('y') | KeyCode::Enter) => {
                self.remove(*id, WorktreeRemoval::Keep)
            }
            (Confirmed::RemoveProject(id), KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::Send(Request::RemoveProject { id: *id })]
            }
            (Confirmed::StopDaemon, KeyCode::Char('y') | KeyCode::Enter) => {
                vec![Effect::StopDaemonAndQuit]
            }
            (Confirmed::Finish(id, choice), KeyCode::Char('y') | KeyCode::Enter) => {
                self.finish(*id, *choice)
            }
            _ => return None,
        };
        Some(effects)
    }

    /// Remembers a remove that the daemon may refuse for a dirty worktree, so the refusal
    /// can offer to force it.
    pub(super) fn remove(&mut self, id: AgentId, worktree: WorktreeRemoval) -> Vec<Effect> {
        let (remove_worktree, force) = match worktree {
            WorktreeRemoval::Keep => (false, false),
            WorktreeRemoval::Remove => (true, false),
            WorktreeRemoval::Force => (true, true),
        };
        if remove_worktree
            && let Some(agent) = self.agents.iter().find(|a| a.id == id)
            && let Some(worktree) = &agent.worktree
        {
            let done = format!(
                "removed {} and its worktree, kept branch {}",
                agent.name, worktree.branch
            );
            self.removals.insert(id, done);
        }
        self.pending_remove = (remove_worktree && !force).then_some(PendingRemove {
            agent: id,
            interactions: self.interactions,
        });
        vec![Effect::Send(Request::RemoveAgent {
            id,
            remove_worktree,
            force,
            delete_branch: false,
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Focus;
    use crate::app::tests::{
        code, ctrl, filter_app, key, loaded, names, project, two_agents, type_text, worktree_agent,
    };

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
    fn help_closes_on_escape_enter_question_mark_or_ctrl_c() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('?'));
        let _ = app.on_key(key('/'));
        let _ = app.on_key(ctrl('c'));
        assert_eq!(app.popup, None);
        for close in [
            code(KeyCode::Esc),
            code(KeyCode::Enter),
            key('?'),
            ctrl('c'),
        ] {
            let (mut app, _, _) = two_agents();
            let _ = app.on_key(key('?'));
            assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
            let _ = app.on_key(key('z'));
            assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
            let _ = app.on_key(close);
            assert_eq!(app.popup, None, "{close:?}");
        }
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
                delete_branch: false,
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
}

#[cfg(test)]
mod help_tests {
    use super::*;
    use crate::app::tests::{agent, loaded, project};
    use protocol::Status;
    use std::time::Instant;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn help(app: &App) -> &Help {
        match &app.popup {
            Some(Popup::Help(help)) => help,
            other => panic!("no help popup: {other:?}"),
        }
    }

    fn stopped() -> App {
        let p = project("p");
        loaded(vec![p.clone()], vec![agent(&p, "api", Status::Stopped)]).0
    }

    #[test]
    fn slash_filters_the_help_and_escape_clears_then_closes_it() {
        let mut app = stopped();
        let _ = app.on_key(key('?'));
        let _ = app.on_key(key('/'));
        for c in "diff?".chars() {
            let _ = app.on_key(key(c));
        }
        assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
        assert_eq!(help(&app).filter, "diff?");
        let _ = app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(help(&app).filter, "diff");
        let _ = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(help(&app).filter, "diff");
        let _ = app.on_key(key('j'));
        assert_eq!(help(&app).filter, "diff");
        let _ = app.on_key(key('/'));
        let _ = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(help(&app).filter, "");
        let _ = app.on_key(key('/'));
        let _ = app.on_paste("pane\nmore".into());
        assert_eq!(help(&app).filter, "pane");
        let _ = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let _ = app.on_key(key('/'));
        let _ = app.on_key(key('x'));
        let _ = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        if let Some(Popup::Help(help)) = &mut app.popup {
            help.scroll = 2;
        }
        let _ = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!((help(&app).filter.as_str(), help(&app).scroll), ("", 0));
        let _ = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('?'));
        assert_eq!(help(&app).filter, "");
        let _ = app.on_key(key('?'));
        assert_eq!(app.popup, None);
    }

    #[test]
    fn the_help_scrolls_and_page_keys_keep_it_open() {
        let mut app = stopped();
        app.set_screen_size(60, 20);
        let _ = app.on_key(key('?'));
        let _ = app.on_key(key('k'));
        assert_eq!(help(&app).scroll, 0);
        let _ = app.on_key(key('j'));
        let _ = app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(help(&app).scroll, 2);
        let _ = app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
        assert!(help(&app).scroll > 2, "{}", help(&app).scroll);
        let _ = app.on_key(key('G'));
        let last = help(&app).scroll;
        let _ = app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let _ = app.on_key(key('j'));
        assert_eq!(help(&app).scroll, last);
        let _ = app.on_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(help(&app).scroll < last);
        let _ = app.on_key(key('g'));
        assert_eq!(help(&app).scroll, 0);
        let _ = app.on_key(key('G'));
        let _ = app.on_key(key('q'));
        assert_eq!(app.popup, None);
        let _ = app.on_key(key('?'));
        assert_eq!(help(&app).scroll, 0);
    }

    #[test]
    fn an_action_that_does_not_apply_says_why() {
        for (pressed, why) in [
            ('x', "api is not running"),
            ('s', "api has no terminal yet — enter starts it"),
            ('J', "api has no terminal yet — enter starts it"),
            ('y', "api is not waiting for an answer"),
            ('i', "api is not running"),
        ] {
            let mut app = stopped();
            assert!(app.on_key(key(pressed)).is_empty(), "{pressed}");
            assert_eq!(app.current_message(Instant::now()), Some(why), "{pressed}");
        }
        let p = project("p");
        let (mut app, _) = loaded(vec![p], Vec::new());
        let _ = app.on_key(key('x'));
        assert_eq!(
            app.current_message(Instant::now()),
            Some("no agent selected")
        );
    }

    #[test]
    fn unavailable_reports_every_agent_action_that_cannot_run() {
        let app = stopped();
        assert!(app.unavailable(Action::Stop).is_some());
        assert!(app.unavailable(Action::Restart).is_none());
        assert!(app.unavailable(Action::NewAgent).is_none());
        assert!(app.unavailable(Action::Help).is_none());
    }
}
