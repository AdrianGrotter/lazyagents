use protocol::{AgentId, InputEvent, Request, Status};
use ratatui::crossterm::event::KeyEvent;

use crate::fixed_keys::{self, Motion, ScrollKey};
use crate::keymap::{Action, Keymap};
use crate::keys;

use super::{App, Effect, Focus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMode {
    pub agent: AgentId,
    pub from_pane: bool,
}

pub(super) const SCROLL_TOP: i32 = i32::MAX / 2;
pub(super) const SCROLL_BOTTOM: i32 = i32::MIN / 2;

pub(super) fn press(id: AgentId, code: protocol::KeyCode) -> Effect {
    Effect::Send(Request::Input {
        id,
        event: InputEvent::Key(protocol::Key::new(code, protocol::Modifiers::NONE)),
    })
}

pub(super) fn scroll_request(id: AgentId, lines: i32) -> Effect {
    Effect::Send(Request::Scroll { id, lines })
}

pub(super) fn prefix_help(keymap: &Keymap) -> String {
    let keys = |actions: &[Action]| {
        let keys: Vec<_> = actions.iter().flat_map(|a| keymap.keys(*a)).collect();
        crate::keymap::join(&keys)
    };
    let parts: Vec<String> = [
        (keys(&[Action::PaneNext, Action::PanePrevious]), "switch"),
        (
            keys(&[Action::PaneNextActive, Action::PanePreviousActive]),
            "active",
        ),
        (keys(&[Action::PaneLeave]), "sidebar"),
        (keys(&[Action::PaneScroll]), "scroll"),
        (keys(&[Action::PaneScreenMode]), "screen"),
        (keys(&[Action::PaneSendPrefix]), "send"),
    ]
    .into_iter()
    .filter(|(keys, _)| !keys.is_empty())
    .map(|(keys, label)| format!("{keys} {label}"))
    .collect();
    format!("{}: {}", keymap.prefix(), parts.join(" · "))
}

impl App {
    /// Scroll mode, while it has the focus.
    pub fn scroll_mode(&self) -> Option<ScrollMode> {
        match self.focus {
            Focus::Scroll(mode) => Some(mode),
            _ => None,
        }
    }

    /// Leaves scroll mode, returning to the pane only if it was entered from there and
    /// the agent still runs, and scrolls the agent back to the bottom.
    fn exit_scroll(&mut self) -> Vec<Effect> {
        let Some(mode) = self.scroll_mode() else {
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

    pub(super) fn enter_scroll(&mut self, from_pane: bool) {
        match self.selected_agent() {
            Some(agent) if agent.status != Status::Stopped => {
                self.focus = Focus::Scroll(ScrollMode {
                    agent: agent.id,
                    from_pane,
                });
                self.prefix_pending = false;
            }
            Some(_) => self.fail("nothing to scroll"),
            None => {}
        }
    }

    /// Exits or quits; `None` for any other key.
    pub(super) fn leave_scroll(&mut self, key: &KeyEvent) -> Option<Vec<Effect>> {
        if fixed_keys::scroll_key(key) == Some(ScrollKey::Exit) {
            return Some(self.exit_scroll());
        }
        (self.keymap.sidebar(key) == Some(Action::Quit)).then(|| vec![Effect::Quit])
    }

    pub(super) fn on_scroll_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.leave_scroll(&key) {
            return effects;
        }
        let Some(mode) = self.scroll_mode() else {
            return Vec::new();
        };
        let page = self.diff_page() as i32;
        let Some(ScrollKey::Move(motion)) = fixed_keys::scroll_key(&key) else {
            return Vec::new();
        };
        let lines = match motion {
            Motion::Down => -1,
            Motion::Up => 1,
            Motion::HalfPageDown => -(page / 2),
            Motion::HalfPageUp => page / 2,
            Motion::PageDown => -page,
            Motion::PageUp => page,
            Motion::Top => SCROLL_TOP,
            Motion::Bottom => SCROLL_BOTTOM,
        };
        vec![scroll_request(mode.agent, lines)]
    }

    /// Closes the diff view or scroll mode, scrolling the agent back to the bottom, and
    /// focuses a sidebar box.
    pub(super) fn leave_for_sidebar(&mut self, focus: Focus) -> Vec<Effect> {
        let effects = match self.scroll_mode() {
            Some(mode) => vec![scroll_request(mode.agent, SCROLL_BOTTOM)],
            None => Vec::new(),
        };
        self.focus = focus;
        self.prefix_pending = false;
        effects
    }

    pub(super) fn on_pane_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(id) = self.selected_agent().map(|a| a.id) else {
            self.leave_pane();
            return Vec::new();
        };
        if std::mem::take(&mut self.prefix_pending) {
            return match self.keymap.after_prefix(&key) {
                Some(Action::PaneNext) => self.cycle_in_pane(1),
                Some(Action::PanePrevious) => self.cycle_in_pane(-1),
                Some(Action::PaneNextActive) => self.cycle_active_in_pane(1),
                Some(Action::PanePreviousActive) => self.cycle_active_in_pane(-1),
                Some(Action::PaneLeave) => {
                    self.leave_pane();
                    Vec::new()
                }
                Some(Action::PaneScroll) => {
                    self.enter_scroll(true);
                    Vec::new()
                }
                Some(Action::PaneSendPrefix) => self.input(id, &self.keymap.prefix().event()),
                Some(Action::PaneScreenMode) => {
                    self.screen_mode = self.screen_mode.next();
                    Vec::new()
                }
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
            Some(Action::PaneNextActive) => self.cycle_active_in_pane(1),
            Some(Action::PanePreviousActive) => self.cycle_active_in_pane(-1),
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

    fn cycle_active_in_pane(&mut self, step: i32) -> Vec<Effect> {
        self.move_active(step);
        self.sync()
    }

    pub(super) fn leave_pane(&mut self) {
        self.focus = Focus::Agents;
        self.prefix_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        agent, code, configured, ctrl, input_key, key, loaded, loaded_with, project, running_pair,
        scroll_effects, two_agents,
    };
    use protocol::{AgentInfo, ServerMessage};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use std::time::Instant;

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
            app.message.as_ref().map(|m| m.text.as_str()),
            Some(
                "ctrl-q: j/k switch · J/K active · q/esc sidebar · s scroll · + screen · ctrl-q send"
            )
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
    fn ctrl_j_and_ctrl_k_switch_agents_and_ctrl_h_leaves_by_default() {
        let (mut app, a, b) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.on_key(ctrl('j')),
            vec![Effect::Send(Request::Subscribe { id: b.id })]
        );
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.on_key(ctrl('k')),
            vec![Effect::Send(Request::Subscribe { id: a.id })]
        );
        assert_eq!(app.focus, Focus::Pane);
        assert!(app.on_key(ctrl('h')).is_empty());
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn ctrl_alt_j_and_k_switch_to_active_agents_and_never_reach_the_agent() {
        let p = project("p");
        let a = agent(&p, "a", Status::Working);
        let b = agent(&p, "b", Status::Idle);
        let c = agent(&p, "c", Status::Working);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b, c.clone()]);
        let _ = app.on_key(code(KeyCode::Enter));
        let ctrl_alt =
            |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(
            app.on_key(ctrl_alt('j')),
            vec![Effect::Send(Request::Subscribe { id: c.id })]
        );
        assert_eq!(
            app.on_key(ctrl_alt('k')),
            vec![Effect::Send(Request::Subscribe { id: a.id })]
        );
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn ctrl_cmd_j_and_prefix_big_j_switch_to_active_agents_in_the_pane() {
        let p = project("p");
        let a = agent(&p, "a", Status::Working);
        let b = agent(&p, "b", Status::Idle);
        let c = agent(&p, "c", Status::Working);
        let (mut app, _) = loaded(vec![p], vec![a.clone(), b, c.clone()]);
        let _ = app.on_key(code(KeyCode::Enter));
        let ctrl_cmd = |ch| {
            KeyEvent::new(
                KeyCode::Char(ch),
                KeyModifiers::CONTROL | KeyModifiers::SUPER,
            )
        };
        assert_eq!(
            app.on_key(ctrl_cmd('j')),
            vec![Effect::Send(Request::Subscribe { id: c.id })]
        );
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.on_key(ctrl_cmd('k')),
            vec![Effect::Send(Request::Subscribe { id: a.id })]
        );
        let _ = app.on_key(ctrl('q'));
        assert_eq!(
            app.on_key(key('K')),
            vec![Effect::Send(Request::Subscribe { id: c.id })]
        );
        let _ = app.on_key(ctrl('q'));
        assert_eq!(
            app.on_key(key('J')),
            vec![Effect::Send(Request::Subscribe { id: a.id })]
        );
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn ctrl_l_reaches_the_agent() {
        let (mut app, a, _) = two_agents();
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.on_key(ctrl('l')),
            vec![char_input(a.id, 'l', protocol::Modifiers::CTRL)]
        );
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn unbound_direct_keys_reach_the_agent() {
        let config = configured(
            r#"
            [keys.pane]
            direct_next = []
            direct_previous = []
            direct_leave = []
            "#,
        );
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&p, "beta", Status::Idle);
        let (mut app, _) = loaded_with(config, vec![p], vec![a.clone(), b]);
        let _ = app.on_key(code(KeyCode::Enter));
        for c in ['j', 'k', 'h'] {
            assert_eq!(
                app.on_key(ctrl(c)),
                vec![char_input(a.id, c, protocol::Modifiers::CTRL)]
            );
            assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
            assert_eq!(app.focus, Focus::Pane);
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
            app.message.as_ref().map(|m| m.text.as_str()),
            Some("ctrl-a: n/p switch · J/K active · x sidebar · s scroll · + screen · ctrl-a send")
        );
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

    #[test]
    fn scroll_mode_from_sidebar_and_prefix() {
        let (mut app, a, _) = running_pair();
        assert!(app.on_key(key('s')).is_empty());
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
        assert_eq!(
            app.scroll_mode(),
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
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
        assert!(!app.prefix_pending);
        assert_eq!(app.scroll_mode().map(|s| s.from_pane), Some(true));
    }

    #[test]
    fn scroll_keys_send_scroll_requests() {
        let (mut app, a, _) = running_pair();
        let _ = app.on_key(key('s'));
        for (event, lines) in [
            (key('j'), -1),
            (key('k'), 1),
            (code(KeyCode::Down), -1),
            (code(KeyCode::Up), 1),
            (ctrl('d'), -5),
            (ctrl('u'), 5),
            (code(KeyCode::PageDown), -10),
            (code(KeyCode::PageUp), 10),
            (key('g'), i32::MAX / 2),
            (key('G'), i32::MIN / 2),
        ] {
            assert_eq!(app.on_key(event), scroll_effects(a.id, lines), "{event:?}");
            assert!(matches!(app.focus.clone(), Focus::Scroll(_)), "{event:?}");
        }
        for event in [key('x'), key('d'), key('n'), code(KeyCode::Enter), key('s')] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert!(
                matches!((&app.focus, &app.popup), (Focus::Scroll(_), None)),
                "{event:?}"
            );
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
            assert_eq!(app.focus, Focus::Agents, "{exit:?}");

            let _ = app.on_key(code(KeyCode::Enter));
            let _ = app.on_key(ctrl('q'));
            let _ = app.on_key(key('s'));
            assert_eq!(app.on_key(exit), bottom, "{exit:?}");
            assert_eq!(app.focus, Focus::Pane, "{exit:?}");
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
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
        assert_eq!(app.on_key(key('q')), bottom);
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn scroll_mode_closes_when_its_agent_disappears() {
        let (mut app, a, b) = running_pair();
        let _ = app.on_key(key('s'));
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn scroll_needs_a_terminal() {
        let p = project("p");
        let stopped = agent(&p, "stopped", Status::Stopped);
        let exited = agent(&p, "exited", Status::Exited(1));
        let (mut app, _) = loaded(vec![p], vec![stopped, exited.clone()]);
        assert!(app.on_key(key('s')).is_empty());
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
        assert_eq!(app.scroll_mode().map(|s| s.agent), Some(exited.id));
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('k'));
        assert!(app.on_key(key('s')).is_empty());
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("stopped has no terminal yet — enter starts it")
        );
    }

    #[test]
    fn scroll_mode_bottoms_its_agent_when_the_selection_moves_away() {
        let (mut app, a, b) = running_pair();
        let _ = app.on_key(key('s'));
        app.selected = Some(b.id);
        let effects = app.apply_server(ServerMessage::AgentUpdated(b.clone()));
        assert_eq!(effects.first(), scroll_effects(a.id, i32::MIN / 2).first());
        assert!(effects.contains(&Effect::Send(Request::Subscribe { id: b.id })));
        assert_eq!(app.focus, Focus::Agents);
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
        assert_eq!(app.focus, Focus::Agents);
    }
}
