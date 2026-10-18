//! What the help popup lists, and how much of it fits a screen.

use ratatui::text::Span;

use crate::app::{App, Help};
use crate::fixed_keys;
use crate::keymap::{self, Action};
use crate::layout::{text_width, wrap_words};

const SIDEBAR_HELP: [(&[Action], &str); 20] = [
    (
        &[Action::Down, Action::Up],
        "move down / up in the focused box",
    ),
    (
        &[Action::FocusProjects, Action::FocusAgents],
        "focus Projects / Agents",
    ),
    (&[Action::ToggleFocus], "switch between Projects and Agents"),
    (
        &[Action::Open],
        "open the agent's pane, restarting it if it is not running",
    ),
    (&[Action::NewAgent], "new agent"),
    (&[Action::Stop, Action::Restart], "stop / restart agent"),
    (&[Action::Remove], "remove agent (in Projects: the project)"),
    (&[Action::Rename], "rename agent"),
    (&[Action::ScreenMode], "wider pane / full screen / normal"),
    (&[Action::AddProject], "add project"),
    (&[Action::ToggleAllAgents], "all agents / this project"),
    (
        &[Action::NextAttention],
        "next agent needing input, then next done",
    ),
    (&[Action::Filter], "filter the agents (esc clears)"),
    (&[Action::Commands], "custom commands"),
    (
        &[Action::Approve, Action::Deny, Action::Reply],
        "approve / deny / reply to the agent",
    ),
    (
        &[Action::Diff, Action::Finish],
        "diff / finish (merge, PR, clean up) a worktree agent",
    ),
    (&[Action::Scroll], "scroll the agent's history"),
    (
        &[Action::PeekDown, Action::PeekUp],
        "scroll the agent's pane down / up from here",
    ),
    (&[Action::Help], "help"),
    (
        &[Action::Quit, Action::StopDaemon],
        "quit (agents keep running) / stop the daemon too",
    ),
];

/// Descriptions never wrap narrower than this.
const MIN_TEXT: usize = 16;

/// A row of the help popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Section(String),
    Blank,
    /// A key and its description, or a wrapped part of the description; `applies` is false
    /// for a sidebar action that cannot act on the selection.
    Key {
        text: String,
        applies: bool,
    },
    NoMatch(String),
}

impl Row {
    pub fn text(&self) -> &str {
        match self {
            Row::Section(text) | Row::Key { text, .. } | Row::NoMatch(text) => text,
            Row::Blank => "",
        }
    }
}

/// `filter` keeps only the keys whose keys or description contain it, ignoring case, and
/// the key column keeps its width while filtering. Descriptions wrap under themselves to
/// fit `width` cells.
pub fn rows(app: &App, filter: &str, width: usize) -> Vec<Row> {
    let keymap = &app.keymap;
    let sidebar: Vec<(String, String, bool)> = SIDEBAR_HELP
        .iter()
        .map(|(actions, text)| {
            let keys: Vec<String> = actions.iter().map(|a| keymap.describe(*a)).collect();
            let applies = actions.iter().any(|a| app.unavailable(*a).is_none());
            (keys.join(", "), text.to_string(), applies)
        })
        .collect();
    let mut pane = Vec::new();
    for (action, text) in [
        (Action::PaneLeave, "back to the sidebar"),
        (Action::PaneNext, "next agent"),
        (Action::PanePrevious, "previous agent"),
    ] {
        pane.push((keymap.describe(action), text.to_string(), true));
        let direct = keymap.direct_keys(action);
        if !direct.is_empty() {
            pane.push((keymap::join(&direct), text.to_string(), true));
        }
    }
    pane.extend([
        (
            keymap.describe(Action::PaneScroll),
            "scroll the agent's history".to_string(),
            true,
        ),
        (
            keymap.describe(Action::PaneScreenMode),
            "wider pane / full screen / normal".to_string(),
            true,
        ),
        (
            keymap.describe(Action::PaneSendPrefix),
            format!("send {} to the agent", keymap.prefix()),
            true,
        ),
    ]);
    let custom: Vec<(String, String, bool)> = app
        .commands
        .iter()
        .filter_map(|c| Some((c.key?.to_string(), c.description.clone(), true)))
        .collect();
    let key_width = sidebar
        .iter()
        .chain(&pane)
        .chain(&custom)
        .map(|(key, ..)| key.chars().count())
        .max()
        .unwrap_or(0);
    let indent = 2 + key_width + 2;
    let room = width.saturating_sub(indent).max(MIN_TEXT);
    let needle = filter.to_lowercase();
    let matches = |key: &str, text: &str| {
        key.to_lowercase().contains(&needle) || text.to_lowercase().contains(&needle)
    };
    let mut lines = Vec::new();
    for (title, keys) in [
        ("Sidebar", sidebar),
        ("Agent pane", pane),
        ("Commands", custom),
    ] {
        let keys: Vec<Row> = keys
            .into_iter()
            .filter(|(key, text, _)| matches(key, text))
            .flat_map(|(key, text, applies)| {
                wrap_words(&text, room)
                    .into_iter()
                    .enumerate()
                    .map(move |(index, part)| {
                        let text = match index {
                            0 => format!("  {key:<key_width$}  {part}"),
                            _ => format!("{:indent$}{part}", ""),
                        };
                        Row::Key { text, applies }
                    })
            })
            .collect();
        if keys.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Row::Blank);
        }
        lines.push(Row::Section(title.to_string()));
        lines.extend(keys);
    }
    if lines.is_empty() {
        lines.push(Row::NoMatch(format!("no key matches \"{filter}\"")));
    }
    lines
}

/// The help popup laid out for a screen: as many of its rows as fit, a row saying how many
/// are above and below when not all do, and its hint.
pub struct Layout {
    pub width: u16,
    pub rows: Vec<Row>,
    /// How many of `rows` fit on screen at once.
    pub room: usize,
}

impl Layout {
    pub fn new(app: &App, help: &Help, screen: (u16, u16)) -> Self {
        let inner = usize::from(screen.0).saturating_sub(4);
        let rows = rows(app, &help.filter, inner);
        let widest = rows
            .iter()
            .map(|row| Span::raw(row.text()).width())
            .max()
            .unwrap_or(0);
        let hint = fixed_keys::help_hints().join(" · ");
        let width = (widest.max(text_width(&hint)) + 4).min(usize::from(screen.0));
        let height = usize::from(screen.1).saturating_sub(2);
        let room = if rows.len() < height {
            rows.len()
        } else {
            height.saturating_sub(2).max(1)
        };
        Layout {
            width: width as u16,
            rows,
            room,
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.rows.len().saturating_sub(self.room)
    }
}
