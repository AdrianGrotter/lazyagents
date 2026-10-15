use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Quit,
    StopDaemon,
    Help,
    Down,
    Up,
    FocusProjects,
    FocusAgents,
    ToggleFocus,
    Open,
    NewAgent,
    AddProject,
    Stop,
    Restart,
    Remove,
    Rename,
    NextAttention,
    ToggleAllAgents,
    Diff,
    Scroll,
    PaneNext,
    PanePrevious,
    PaneLeave,
    PaneScroll,
    PaneSendPrefix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeySpec {
    pub fn parse(s: &str) -> Result<KeySpec, String> {
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        let mut rest = s;
        while let Some((modifier, after)) = rest.split_once('-') {
            let flag = match modifier {
                _ if after.is_empty() => break,
                "ctrl" => &mut ctrl,
                "alt" => &mut alt,
                "shift" => &mut shift,
                _ => break,
            };
            if std::mem::replace(flag, true) {
                return Err(format!("{s:?}: {modifier} given twice"));
            }
            rest = after;
        }
        let code = match rest {
            "enter" => KeyCode::Enter,
            "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "space" => KeyCode::Char(' '),
            _ => match function_key(rest).or_else(|| single_char(rest)) {
                Some(code) => code,
                None => return Err(format!("{s:?}: unknown key {rest:?}")),
            },
        };
        let code = match code {
            KeyCode::Tab if shift => {
                shift = false;
                KeyCode::BackTab
            }
            KeyCode::Char(c) if shift => {
                shift = false;
                match single_char(&c.to_uppercase().to_string()) {
                    Some(KeyCode::Char(upper)) if c.is_alphabetic() => KeyCode::Char(upper),
                    _ => return Err(format!("{s:?}: write the shifted character itself")),
                }
            }
            code => code,
        };
        Ok(KeySpec {
            code,
            ctrl,
            alt,
            shift,
        })
    }

    /// Terminals report a shifted character as the character itself, with or without
    /// SHIFT, and shift-tab as BackTab, so SHIFT only counts for the other keys.
    pub fn matches(&self, key: &KeyEvent) -> bool {
        let shift_counts = !matches!(self.code, KeyCode::Char(_) | KeyCode::BackTab);
        self.code == key.code
            && self.ctrl == key.modifiers.contains(KeyModifiers::CONTROL)
            && self.alt == key.modifiers.contains(KeyModifiers::ALT)
            && (!shift_counts || self.shift == key.modifiers.contains(KeyModifiers::SHIFT))
    }

    pub fn event(&self) -> KeyEvent {
        let mut modifiers = KeyModifiers::NONE;
        for (on, flag) in [
            (self.ctrl, KeyModifiers::CONTROL),
            (self.alt, KeyModifiers::ALT),
            (self.shift, KeyModifiers::SHIFT),
        ] {
            if on {
                modifiers |= flag;
            }
        }
        KeyEvent::new(self.code, modifiers)
    }
}

fn function_key(name: &str) -> Option<KeyCode> {
    let n: u8 = name.strip_prefix('f')?.parse().ok()?;
    (1..=12).contains(&n).then_some(KeyCode::F(n))
}

fn single_char(name: &str) -> Option<KeyCode> {
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(KeyCode::Char(c)),
        _ => None,
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "ctrl-"),
            (self.alt, "alt-"),
            (self.shift, "shift-"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        match self.code {
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::BackTab => f.write_str("backtab"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Up => f.write_str("up"),
            KeyCode::Down => f.write_str("down"),
            KeyCode::Left => f.write_str("left"),
            KeyCode::Right => f.write_str("right"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pageup"),
            KeyCode::PageDown => f.write_str("pagedown"),
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Keymap {
    sidebar: Vec<(KeySpec, Action)>,
    prefix: KeySpec,
    pane: Vec<(KeySpec, Action)>,
    leave: Vec<KeySpec>,
}

fn key(text: &str) -> KeySpec {
    KeySpec::parse(text).unwrap_or_else(|error| panic!("default key {error}"))
}

fn bindings(table: &[(&str, Action)]) -> Vec<(KeySpec, Action)> {
    table
        .iter()
        .map(|(text, action)| (key(text), *action))
        .collect()
}

impl Keymap {
    pub fn defaults() -> Keymap {
        use Action::*;
        Keymap {
            sidebar: bindings(&[
                ("q", Quit),
                ("ctrl-c", Quit),
                ("Q", StopDaemon),
                ("?", Help),
                ("j", Down),
                ("down", Down),
                ("k", Up),
                ("up", Up),
                ("h", FocusProjects),
                ("left", FocusProjects),
                ("backtab", FocusProjects),
                ("l", FocusAgents),
                ("right", FocusAgents),
                ("tab", ToggleFocus),
                ("enter", Open),
                ("n", NewAgent),
                ("a", AddProject),
                ("x", Stop),
                ("r", Restart),
                ("d", Remove),
                ("R", Rename),
                ("g", NextAttention),
                ("A", ToggleAllAgents),
                ("D", Diff),
                ("s", Scroll),
            ]),
            prefix: key("ctrl-q"),
            pane: bindings(&[
                ("j", PaneNext),
                ("k", PanePrevious),
                ("q", PaneLeave),
                ("esc", PaneLeave),
                ("s", PaneScroll),
                ("ctrl-q", PaneSendPrefix),
            ]),
            leave: vec![key("ctrl-]"), key("ctrl-5")],
        }
    }

    pub fn sidebar(&self, key: &KeyEvent) -> Option<Action> {
        lookup(&self.sidebar, key)
    }

    pub fn prefix(&self) -> KeySpec {
        self.prefix
    }

    pub fn is_prefix(&self, key: &KeyEvent) -> bool {
        self.prefix.matches(key)
    }

    pub fn after_prefix(&self, key: &KeyEvent) -> Option<Action> {
        lookup(&self.pane, key)
    }

    pub fn is_direct_leave(&self, key: &KeyEvent) -> bool {
        self.leave.iter().any(|spec| spec.matches(key))
    }

    pub fn direct_leave(&self) -> &[KeySpec] {
        &self.leave
    }

    /// All keys bound to `action`, joined with `/`; pane actions start with the prefix.
    pub fn describe(&self, action: Action) -> String {
        let pane = self.pane.iter().any(|(_, a)| *a == action);
        let table = if pane { &self.pane } else { &self.sidebar };
        let keys: Vec<String> = table
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(spec, _)| spec.to_string())
            .collect();
        match (keys.is_empty(), pane) {
            (true, _) => "unbound".to_string(),
            (false, true) => format!("{} {}", self.prefix, keys.join("/")),
            (false, false) => keys.join("/"),
        }
    }
}

fn lookup(table: &[(KeySpec, Action)], key: &KeyEvent) -> Option<Action> {
    table
        .iter()
        .find(|(spec, _)| spec.matches(key))
        .map(|(_, action)| *action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn spec(code: KeyCode, ctrl: bool, alt: bool, shift: bool) -> KeySpec {
        KeySpec {
            code,
            ctrl,
            alt,
            shift,
        }
    }

    #[test]
    fn parse_accepts_modifiers_named_keys_and_characters() {
        for (text, expected) in [
            ("ctrl-q", spec(KeyCode::Char('q'), true, false, false)),
            ("alt-j", spec(KeyCode::Char('j'), false, true, false)),
            ("ctrl-alt-x", spec(KeyCode::Char('x'), true, true, false)),
            ("shift-tab", spec(KeyCode::BackTab, false, false, false)),
            ("shift-a", spec(KeyCode::Char('A'), false, false, false)),
            ("shift-up", spec(KeyCode::Up, false, false, true)),
            ("ctrl-]", spec(KeyCode::Char(']'), true, false, false)),
            ("ctrl-5", spec(KeyCode::Char('5'), true, false, false)),
            ("A", spec(KeyCode::Char('A'), false, false, false)),
            ("a", spec(KeyCode::Char('a'), false, false, false)),
            ("?", spec(KeyCode::Char('?'), false, false, false)),
            ("-", spec(KeyCode::Char('-'), false, false, false)),
            ("ctrl--", spec(KeyCode::Char('-'), true, false, false)),
            ("f", spec(KeyCode::Char('f'), false, false, false)),
            ("ø", spec(KeyCode::Char('ø'), false, false, false)),
            ("enter", spec(KeyCode::Enter, false, false, false)),
            ("esc", spec(KeyCode::Esc, false, false, false)),
            ("tab", spec(KeyCode::Tab, false, false, false)),
            ("backtab", spec(KeyCode::BackTab, false, false, false)),
            ("backspace", spec(KeyCode::Backspace, false, false, false)),
            ("up", spec(KeyCode::Up, false, false, false)),
            ("down", spec(KeyCode::Down, false, false, false)),
            ("left", spec(KeyCode::Left, false, false, false)),
            ("right", spec(KeyCode::Right, false, false, false)),
            ("home", spec(KeyCode::Home, false, false, false)),
            ("end", spec(KeyCode::End, false, false, false)),
            ("pageup", spec(KeyCode::PageUp, false, false, false)),
            ("pagedown", spec(KeyCode::PageDown, false, false, false)),
            ("space", spec(KeyCode::Char(' '), false, false, false)),
            ("f1", spec(KeyCode::F(1), false, false, false)),
            ("f12", spec(KeyCode::F(12), false, false, false)),
        ] {
            assert_eq!(KeySpec::parse(text), Ok(expected), "{text}");
        }
    }

    #[test]
    fn parse_rejects_malformed_strings() {
        for text in [
            "",
            "ctrl-",
            "ab",
            "Enter",
            "f0",
            "f13",
            "hyper-a",
            "ctrl-ctrl-a",
            "ctrl-foo",
            "shift-1",
        ] {
            assert!(KeySpec::parse(text).is_err(), "{text:?} parsed");
        }
    }

    #[test]
    fn display_round_trips_through_parse() {
        for text in [
            "ctrl-q",
            "alt-j",
            "ctrl-alt-x",
            "shift-up",
            "A",
            "ctrl-]",
            "enter",
            "esc",
            "backtab",
            "pagedown",
            "space",
            "f5",
        ] {
            let parsed = KeySpec::parse(text).expect(text);
            assert_eq!(parsed.to_string(), text);
            assert_eq!(KeySpec::parse(&parsed.to_string()), Ok(parsed));
        }
    }

    #[test]
    fn matches_is_case_sensitive_and_ignores_shift_on_characters() {
        let big_a = KeySpec::parse("A").expect("A");
        assert!(big_a.matches(&ev(KeyCode::Char('A'), KeyModifiers::NONE)));
        assert!(big_a.matches(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)));
        assert!(!big_a.matches(&ev(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(!big_a.matches(&ev(KeyCode::Char('a'), KeyModifiers::SHIFT)));

        let small_a = KeySpec::parse("a").expect("a");
        assert!(small_a.matches(&ev(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(!small_a.matches(&ev(KeyCode::Char('A'), KeyModifiers::SHIFT)));
        assert!(!small_a.matches(&ev(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert!(!small_a.matches(&ev(KeyCode::Char('a'), KeyModifiers::ALT)));

        let backtab = KeySpec::parse("backtab").expect("backtab");
        assert!(backtab.matches(&ev(KeyCode::BackTab, KeyModifiers::SHIFT)));
        assert!(backtab.matches(&ev(KeyCode::BackTab, KeyModifiers::NONE)));

        let shift_up = KeySpec::parse("shift-up").expect("shift-up");
        assert!(shift_up.matches(&ev(KeyCode::Up, KeyModifiers::SHIFT)));
        assert!(!shift_up.matches(&ev(KeyCode::Up, KeyModifiers::NONE)));
        let up = KeySpec::parse("up").expect("up");
        assert!(!up.matches(&ev(KeyCode::Up, KeyModifiers::SHIFT)));

        let ctrl_q = KeySpec::parse("ctrl-q").expect("ctrl-q");
        assert!(ctrl_q.matches(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(!ctrl_q.matches(&ev(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!ctrl_q.matches(&ev(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }

    #[test]
    fn event_builds_the_key_a_spec_describes() {
        let ctrl_q = KeySpec::parse("ctrl-q").expect("ctrl-q");
        assert_eq!(
            ctrl_q.event(),
            ev(KeyCode::Char('q'), KeyModifiers::CONTROL)
        );
        assert!(ctrl_q.matches(&ctrl_q.event()));
    }

    #[test]
    fn sidebar_defaults_keep_the_existing_bindings() {
        let keymap = Keymap::defaults();
        let none = KeyModifiers::NONE;
        for (event, expected) in [
            (ev(KeyCode::Char('q'), none), Action::Quit),
            (ev(KeyCode::Char('c'), KeyModifiers::CONTROL), Action::Quit),
            (
                ev(KeyCode::Char('Q'), KeyModifiers::SHIFT),
                Action::StopDaemon,
            ),
            (ev(KeyCode::Char('?'), none), Action::Help),
            (ev(KeyCode::Char('j'), none), Action::Down),
            (ev(KeyCode::Down, none), Action::Down),
            (ev(KeyCode::Char('k'), none), Action::Up),
            (ev(KeyCode::Up, none), Action::Up),
            (ev(KeyCode::Char('h'), none), Action::FocusProjects),
            (ev(KeyCode::Left, none), Action::FocusProjects),
            (
                ev(KeyCode::BackTab, KeyModifiers::SHIFT),
                Action::FocusProjects,
            ),
            (ev(KeyCode::Char('l'), none), Action::FocusAgents),
            (ev(KeyCode::Right, none), Action::FocusAgents),
            (ev(KeyCode::Tab, none), Action::ToggleFocus),
            (ev(KeyCode::Enter, none), Action::Open),
            (ev(KeyCode::Char('n'), none), Action::NewAgent),
            (ev(KeyCode::Char('a'), none), Action::AddProject),
            (ev(KeyCode::Char('x'), none), Action::Stop),
            (ev(KeyCode::Char('r'), none), Action::Restart),
            (ev(KeyCode::Char('d'), none), Action::Remove),
            (ev(KeyCode::Char('R'), none), Action::Rename),
            (ev(KeyCode::Char('g'), none), Action::NextAttention),
            (ev(KeyCode::Char('A'), none), Action::ToggleAllAgents),
            (ev(KeyCode::Char('D'), none), Action::Diff),
            (ev(KeyCode::Char('s'), none), Action::Scroll),
        ] {
            assert_eq!(keymap.sidebar(&event), Some(expected), "{event:?}");
        }
        assert_eq!(keymap.sidebar(&ev(KeyCode::Char('z'), none)), None);
        assert_eq!(
            keymap.sidebar(&ev(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn pane_defaults_use_the_ctrl_q_prefix() {
        let keymap = Keymap::defaults();
        let none = KeyModifiers::NONE;
        let ctrl_q = ev(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(keymap.is_prefix(&ctrl_q));
        assert!(!keymap.is_prefix(&ev(KeyCode::Char('q'), none)));
        for (event, expected) in [
            (ev(KeyCode::Char('j'), none), Action::PaneNext),
            (ev(KeyCode::Char('k'), none), Action::PanePrevious),
            (ev(KeyCode::Char('q'), none), Action::PaneLeave),
            (ev(KeyCode::Esc, none), Action::PaneLeave),
            (ev(KeyCode::Char('s'), none), Action::PaneScroll),
            (ctrl_q, Action::PaneSendPrefix),
        ] {
            assert_eq!(keymap.after_prefix(&event), Some(expected), "{event:?}");
        }
        assert_eq!(keymap.after_prefix(&ev(KeyCode::Char('x'), none)), None);
    }

    #[test]
    fn ctrl_bracket_and_legacy_ctrl_5_leave_directly() {
        let keymap = Keymap::defaults();
        assert!(keymap.is_direct_leave(&ev(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(keymap.is_direct_leave(&ev(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!keymap.is_direct_leave(&ev(KeyCode::Char(']'), KeyModifiers::NONE)));
    }

    #[test]
    fn alt_brackets_are_unbound_by_default() {
        let keymap = Keymap::defaults();
        for c in ['[', ']'] {
            let event = ev(KeyCode::Char(c), KeyModifiers::ALT);
            assert!(!keymap.is_prefix(&event));
            assert!(!keymap.is_direct_leave(&event));
            assert_eq!(keymap.sidebar(&event), None);
            assert_eq!(keymap.after_prefix(&event), None);
        }
    }

    #[test]
    fn describe_lists_every_key_and_the_prefix_for_pane_actions() {
        let keymap = Keymap::defaults();
        assert_eq!(keymap.describe(Action::PaneNext), "ctrl-q j");
        assert_eq!(keymap.describe(Action::PanePrevious), "ctrl-q k");
        assert_eq!(keymap.describe(Action::PaneLeave), "ctrl-q q/esc");
        assert_eq!(keymap.describe(Action::PaneSendPrefix), "ctrl-q ctrl-q");
        assert_eq!(keymap.describe(Action::Quit), "q/ctrl-c");
        assert_eq!(keymap.describe(Action::Down), "j/down");
        assert_eq!(keymap.describe(Action::ToggleAllAgents), "A");
    }
}
