//! The keys that are not configurable: those of the diff view, scroll mode and the help
//! popup. Each mode has one table that both its key handler and its hints read.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::keymap::KeySpec;

const fn plain(code: KeyCode) -> KeySpec {
    KeySpec {
        code,
        ctrl: false,
        alt: false,
        shift: false,
    }
}

const fn char(c: char) -> KeySpec {
    plain(KeyCode::Char(c))
}

const fn ctrl(c: char) -> KeySpec {
    KeySpec {
        ctrl: true,
        ..char(c)
    }
}

/// A move through a view that is taller than the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Down,
    Up,
    HalfPageDown,
    HalfPageUp,
    PageDown,
    PageUp,
    Top,
    Bottom,
}

impl Motion {
    /// Where a view showing `page` rows at a time ends up from row `at`, never past `last`.
    pub fn position(self, at: usize, page: usize, last: usize) -> usize {
        let half = page.div_ceil(2).max(1);
        let page = page.max(1);
        let to = match self {
            Motion::Down => at.saturating_add(1),
            Motion::Up => at.saturating_sub(1),
            Motion::HalfPageDown => at.saturating_add(half),
            Motion::HalfPageUp => at.saturating_sub(half),
            Motion::PageDown => at.saturating_add(page),
            Motion::PageUp => at.saturating_sub(page),
            Motion::Top => 0,
            Motion::Bottom => last,
        };
        to.min(last)
    }
}

pub const MOTIONS: [(KeySpec, Motion); 10] = [
    (char('j'), Motion::Down),
    (plain(KeyCode::Down), Motion::Down),
    (char('k'), Motion::Up),
    (plain(KeyCode::Up), Motion::Up),
    (ctrl('d'), Motion::HalfPageDown),
    (ctrl('u'), Motion::HalfPageUp),
    (plain(KeyCode::PageDown), Motion::PageDown),
    (plain(KeyCode::PageUp), Motion::PageUp),
    (char('g'), Motion::Top),
    (char('G'), Motion::Bottom),
];

/// The motion hints, most useful first.
const MOTION_HINTS: [(&[Motion], &str); 4] = [
    (&[Motion::Down, Motion::Up], "move"),
    (&[Motion::HalfPageDown, Motion::HalfPageUp], "half page"),
    (&[Motion::Top, Motion::Bottom], "top/bottom"),
    (&[Motion::PageDown, Motion::PageUp], "page"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKey {
    Close,
    Comment,
    SendComments,
    NextComment,
    PreviousComment,
    Scope,
    NextFile,
    PreviousFile,
    NextHunk,
    PreviousHunk,
    Move(Motion),
}

pub const DIFF_KEYS: [(KeySpec, DiffKey); 12] = [
    (char('q'), DiffKey::Close),
    (plain(KeyCode::Esc), DiffKey::Close),
    (char('D'), DiffKey::Close),
    (char('c'), DiffKey::Comment),
    (char('S'), DiffKey::SendComments),
    (char('n'), DiffKey::NextComment),
    (char('N'), DiffKey::PreviousComment),
    (char('s'), DiffKey::Scope),
    (char(']'), DiffKey::NextFile),
    (char('['), DiffKey::PreviousFile),
    (char('}'), DiffKey::NextHunk),
    (char('{'), DiffKey::PreviousHunk),
];

/// The diff view's hints, most useful first. The comment hints apply only while the diff
/// has comments.
pub const DIFF_HINTS: [(&[DiffKey], &str); 7] = [
    (&[DiffKey::Close], "close"),
    (&[DiffKey::Comment], "comment"),
    (&[DiffKey::SendComments], "send comments"),
    (
        &[DiffKey::NextComment, DiffKey::PreviousComment],
        "comments",
    ),
    (&[DiffKey::Scope], "scope"),
    (&[DiffKey::NextFile, DiffKey::PreviousFile], "file"),
    (&[DiffKey::NextHunk, DiffKey::PreviousHunk], "hunk"),
];

pub fn diff_key(key: &KeyEvent) -> Option<DiffKey> {
    lookup(&DIFF_KEYS, key).or_else(|| lookup(&MOTIONS, key).map(DiffKey::Move))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollKey {
    Exit,
    Move(Motion),
}

pub const SCROLL_KEYS: [(KeySpec, ScrollKey); 2] = [
    (char('q'), ScrollKey::Exit),
    (plain(KeyCode::Esc), ScrollKey::Exit),
];

pub fn scroll_key(key: &KeyEvent) -> Option<ScrollKey> {
    lookup(&SCROLL_KEYS, key).or_else(|| lookup(&MOTIONS, key).map(ScrollKey::Move))
}

/// Scroll mode's hints, most useful first.
pub fn scroll_hints() -> Vec<String> {
    let mut hints: Vec<String> = hint(&SCROLL_KEYS, &[ScrollKey::Exit], "exit")
        .into_iter()
        .collect();
    hints.extend(motion_hints());
    hints
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpKey {
    Close,
    Filter,
    Move(Motion),
}

pub const HELP_KEYS: [(KeySpec, HelpKey); 5] = [
    (plain(KeyCode::Esc), HelpKey::Close),
    (char('q'), HelpKey::Close),
    (plain(KeyCode::Enter), HelpKey::Close),
    (ctrl('c'), HelpKey::Close),
    (char('/'), HelpKey::Filter),
];

pub fn help_key(key: &KeyEvent) -> Option<HelpKey> {
    lookup(&HELP_KEYS, key).or_else(|| lookup(&MOTIONS, key).map(HelpKey::Move))
}

/// The help popup's hints, most useful first.
pub fn help_hints() -> Vec<String> {
    let mut hints = Vec::new();
    hints.extend(hint(&MOTIONS, &[Motion::Down, Motion::Up], "scroll"));
    hints.extend(hint(&HELP_KEYS, &[HelpKey::Filter], "filter"));
    hints.extend(hint(&HELP_KEYS, &[HelpKey::Close], "close"));
    hints
}

pub fn motion_hints() -> Vec<String> {
    MOTION_HINTS
        .iter()
        .filter_map(|(motions, label)| hint(&MOTIONS, motions, label))
        .collect()
}

pub fn lookup<A: Copy>(table: &[(KeySpec, A)], key: &KeyEvent) -> Option<A> {
    table
        .iter()
        .find(|(spec, _)| spec.matches(key))
        .map(|(_, action)| *action)
}

pub fn keys<A: PartialEq>(table: &[(KeySpec, A)], action: A) -> Vec<KeySpec> {
    table
        .iter()
        .filter(|(_, a)| *a == action)
        .map(|(spec, _)| *spec)
        .collect()
}

/// `keys label`: every key of a lone action, or the first key of each of several, joined
/// with `/`. `None` when no action has a key.
pub fn hint<A: PartialEq + Copy>(
    table: &[(KeySpec, A)],
    actions: &[A],
    label: &str,
) -> Option<String> {
    let keys: Vec<KeySpec> = match actions {
        [action] => keys(table, *action),
        actions => actions
            .iter()
            .filter_map(|action| keys(table, *action).first().copied())
            .collect(),
    };
    (!keys.is_empty()).then(|| format!("{} {label}", crate::keymap::join(&keys)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distinct<A>(table: &[(KeySpec, A)]) -> bool {
        table
            .iter()
            .enumerate()
            .all(|(i, (key, _))| table[..i].iter().all(|(other, _)| other != key))
    }

    #[test]
    fn no_key_is_in_a_mode_twice() {
        let motions: Vec<KeySpec> = MOTIONS.iter().map(|(key, _)| *key).collect();
        assert!(distinct(&MOTIONS));
        for table in [
            DIFF_KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            SCROLL_KEYS.iter().map(|(k, _)| *k).collect(),
            HELP_KEYS.iter().map(|(k, _)| *k).collect(),
        ] {
            let pairs: Vec<(KeySpec, ())> = table.iter().map(|k| (*k, ())).collect();
            assert!(distinct(&pairs), "{table:?}");
            assert!(table.iter().all(|k| !motions.contains(k)), "{table:?}");
        }
    }

    #[test]
    fn every_diff_action_has_a_hint_and_a_key() {
        let hinted: Vec<DiffKey> = DIFF_HINTS
            .iter()
            .flat_map(|(keys, _)| keys.iter().copied())
            .collect();
        for (_, action) in DIFF_KEYS {
            assert!(hinted.contains(&action), "{action:?}");
        }
        for (keys, label) in DIFF_HINTS {
            assert!(hint(&DIFF_KEYS, keys, label).is_some(), "{label}");
        }
    }

    #[test]
    fn motions_stop_at_both_ends() {
        assert_eq!(Motion::Up.position(0, 10, 30), 0);
        assert_eq!(Motion::Down.position(30, 10, 30), 30);
        assert_eq!(Motion::HalfPageDown.position(3, 9, 30), 8);
        assert_eq!(Motion::PageUp.position(25, 10, 30), 15);
        assert_eq!(Motion::Bottom.position(0, 10, 30), 30);
        assert_eq!(Motion::Top.position(12, 10, 30), 0);
    }
}
