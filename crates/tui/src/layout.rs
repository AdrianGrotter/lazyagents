//! Screen geometry shared by the model and the view: the boxes of the main screen, which
//! list row a click lands on, how text wraps, and how the setup script popup fits a screen.

use ratatui::layout::{Margin, Rect};
use ratatui::text::Span;

use crate::app::ScreenMode;

pub const SETUP_HINT: &str = "y run it and remember · s skip it this time · n cancel";
pub const SETUP_SCROLL_HINT: &str = "j/k scroll to the end to approve · s skip it · n cancel";
pub const SETUP_INDENT: usize = 2;
const SETUP_MIN_WIDTH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Areas {
    pub projects: Rect,
    pub agents: Rect,
    pub info: Rect,
    pub pane: Rect,
    pub pane_inner: Rect,
    pub status: Rect,
    pub screen: Rect,
    /// The open popup when the screen was last drawn, except the Agents filter, which is
    /// typed in the status line.
    pub popup: Option<Rect>,
}

const INFO_MIN_HEIGHT: u16 = 6;
const AGENTS_MIN_HEIGHT: u16 = 8;
/// Rows inside the Projects box while it is empty, for its wrapped hint.
const EMPTY_PROJECTS_ROWS: u16 = 2;

pub const WIDE_SIDEBAR: u16 = 18;

pub fn areas(area: Rect, project_count: usize, info_lines: usize) -> Areas {
    areas_in(area, project_count, info_lines, ScreenMode::Normal)
}

/// The Info box fits `info_lines` lines, but never takes the Agents box below
/// `AGENTS_MIN_HEIGHT` rows to do so; it keeps `INFO_MIN_HEIGHT` rows while they fit.
pub fn areas_in(area: Rect, project_count: usize, info_lines: usize, mode: ScreenMode) -> Areas {
    let body_height = area.height.saturating_sub(1);
    let status = Rect::new(area.x, area.y + body_height, area.width, area.height.min(1));
    let sidebar_width = match mode {
        ScreenMode::Normal => (area.width / 4).clamp(24, 40),
        ScreenMode::Wide => WIDE_SIDEBAR,
        ScreenMode::Full => 0,
    }
    .min(area.width);
    let project_rows = match project_count {
        0 => EMPTY_PROJECTS_ROWS,
        _ => 1,
    };
    let projects_height = (project_rows + 2).min(body_height);
    let below_projects = body_height - projects_height;
    let wanted = u16::try_from(info_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .max(INFO_MIN_HEIGHT);
    let room = below_projects
        .saturating_sub(AGENTS_MIN_HEIGHT)
        .max(INFO_MIN_HEIGHT);
    let info_height = wanted.min(room).min(below_projects);
    let agents_height = body_height - projects_height - info_height;
    let projects = Rect::new(area.x, area.y, sidebar_width, projects_height);
    let agents = Rect::new(area.x, projects.bottom(), sidebar_width, agents_height);
    let info = Rect::new(area.x, agents.bottom(), sidebar_width, info_height);
    let pane = Rect::new(
        area.x + sidebar_width,
        area.y,
        area.width - sidebar_width,
        body_height,
    );
    Areas {
        projects,
        agents,
        info,
        pane,
        pane_inner: pane.inner(Margin::new(1, 1)),
        status,
        screen: area,
        popup: None,
    }
}

/// The item of a bordered list at row `y`. ratatui scrolls a list only as far as needed to
/// show the selected item, starting from offset 0 on every frame.
pub fn row_at(area: Rect, selected: usize, len: usize, y: u16) -> Option<usize> {
    let inner = area.inner(Margin::new(0, 1));
    if inner.height == 0 || y < inner.y || y >= inner.bottom() {
        return None;
    }
    let offset = selected.saturating_sub(usize::from(inner.height) - 1);
    let index = offset + usize::from(y - inner.y);
    (index < len).then_some(index)
}

/// The approval popup for a worktree setup script, laid out for `screen`.
pub struct SetupLayout {
    pub width: u16,
    pub header: Vec<String>,
    pub body: Vec<String>,
    /// How many of `body`'s rows fit between the header and the footer.
    pub rows: usize,
    pub hint_width: usize,
    /// Whether the screen has room for the heading, one script row and the hint.
    pub fits: bool,
}

impl SetupLayout {
    pub fn new(heading: &str, script: &str, screen: (u16, u16)) -> Self {
        let lines: Vec<String> = script.lines().map(escaped).collect();
        let longest = lines.iter().map(|l| text_width(l)).max().unwrap_or(0);
        let width = (longest + SETUP_INDENT)
            .max(text_width(heading))
            .max(SETUP_MIN_WIDTH - 4)
            + 4;
        let width = width.min(usize::from(screen.0)).max(3);
        let inner = width - 2;
        let body: Vec<String> = lines
            .iter()
            .flat_map(|l| wrap(l, inner.saturating_sub(SETUP_INDENT).max(1)))
            .collect();
        let header = wrap(heading, inner);
        let hint_rows = wrap(SETUP_HINT, inner)
            .len()
            .max(wrap(SETUP_SCROLL_HINT, inner).len());
        let fixed = 2 + header.len() + 1 + 1 + hint_rows;
        let room = usize::from(screen.1).saturating_sub(fixed);
        let rows = body.len().min(room.max(1));
        SetupLayout {
            width: width as u16,
            header,
            body,
            rows,
            hint_width: inner,
            fits: room >= 1 && inner > SETUP_INDENT,
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.body.len() - self.rows
    }
}

/// Characters that draw nothing or reorder what is drawn around them.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{ad}' | '\u{34f}' | '\u{61c}' | '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{2800}' | '\u{3164}' | '\u{fe00}'..='\u{fe0f}'
        | '\u{feff}' | '\u{ffa0}' | '\u{fff9}'..='\u{fffb}' | '\u{e0000}'..='\u{e0fff}')
}

/// Shows control and invisible characters as escapes and a backslash as `\\`, so the
/// script cannot repaint the screen, hide text or pass an escape for plain text.
fn escaped(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\\' => "\\\\".to_string(),
            c if c.is_control() => c.escape_default().to_string(),
            c if is_invisible(c) => c.escape_unicode().to_string(),
            c => c.to_string(),
        })
        .collect()
}

pub fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

pub fn char_width(c: char) -> usize {
    Span::raw(c.to_string()).width()
}

/// Splits `text` into rows of at most `width` cells, losing nothing.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("a row").push(c);
        used += w;
    }
    rows
}

/// Splits each line of `text` into rows of at most `width` cells, between words where a word
/// fits; a line's leading spaces stay.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for (index, word) in line.trim_end_matches('\r').split(' ').enumerate() {
            if index > 0
                && !row.trim_start().is_empty()
                && text_width(&row) + 1 + text_width(word) > width
            {
                rows.extend(wrap(&std::mem::take(&mut row), width));
            } else if index > 0 {
                row.push(' ');
            }
            row.push_str(word);
        }
        rows.extend(wrap(&row, width));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_at_maps_rows_with_scrolling() {
        let area = Rect::new(0, 3, 25, 6);
        assert_eq!(row_at(area, 0, 10, 3), None);
        assert_eq!(row_at(area, 0, 10, 4), Some(0));
        assert_eq!(row_at(area, 0, 10, 7), Some(3));
        assert_eq!(row_at(area, 0, 10, 8), None);
        assert_eq!(row_at(area, 3, 10, 7), Some(3));
        assert_eq!(row_at(area, 5, 10, 4), Some(2));
        assert_eq!(row_at(area, 5, 10, 7), Some(5));
        assert_eq!(row_at(area, 9, 10, 7), Some(9));
        assert_eq!(row_at(area, 0, 2, 5), Some(1));
        assert_eq!(row_at(area, 0, 2, 6), None);
        assert_eq!(row_at(area, 0, 0, 4), None);
        assert_eq!(row_at(Rect::new(0, 0, 25, 2), 0, 3, 0), None);
        assert_eq!(row_at(Rect::new(0, 0, 25, 2), 0, 3, 1), None);
    }

    #[test]
    fn info_panel_fits_its_content_and_shrinks_on_short_terminals() {
        let tall = areas(Rect::new(0, 0, 100, 30), 2, 7);
        assert_eq!((tall.info.height, tall.agents.height), (9, 17));
        assert_eq!(tall.info.bottom(), tall.status.y);
        let medium = areas(Rect::new(0, 0, 100, 20), 2, 7);
        assert_eq!((medium.info.height, medium.agents.height), (8, 8));
        let short = areas(Rect::new(0, 0, 100, 16), 2, 7);
        assert_eq!((short.info.height, short.agents.height), (6, 6));
        let tiny = areas(Rect::new(0, 0, 100, 8), 2, 7);
        assert_eq!((tiny.info.height, tiny.agents.height), (4, 0));
    }

    #[test]
    fn layout_reserves_status_row_and_sidebar() {
        let a = areas(Rect::new(0, 0, 100, 30), 2, 4);
        assert_eq!(a.status, Rect::new(0, 29, 100, 1));
        assert_eq!(a.projects.width, 25);
        assert_eq!(a.projects.height, 3);
        assert_eq!(a.info.height, 6);
        assert_eq!(a.pane.x, 25);
        assert_eq!(a.pane_inner, Rect::new(26, 1, 73, 27));
        assert_eq!(areas(Rect::new(0, 0, 100, 30), 2, 1).info.height, 6);
    }

    #[test]
    fn screen_modes_narrow_or_hide_the_sidebar() {
        let screen = Rect::new(0, 0, 120, 30);
        let normal = areas_in(screen, 2, 4, ScreenMode::Normal);
        assert_eq!(normal.agents.width, 30);
        let wide = areas_in(screen, 2, 4, ScreenMode::Wide);
        assert_eq!(wide.agents.width, WIDE_SIDEBAR);
        assert_eq!(
            wide.pane,
            Rect::new(WIDE_SIDEBAR, 0, 120 - WIDE_SIDEBAR, 29)
        );
        let full = areas_in(screen, 2, 4, ScreenMode::Full);
        assert_eq!(full.agents.width, 0);
        assert_eq!(full.pane, Rect::new(0, 0, 120, 29));
    }
}
