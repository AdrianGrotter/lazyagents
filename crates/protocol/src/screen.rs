use serde::{Deserialize, Serialize};

use crate::AgentId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Style {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".to_string(),
            fg: Color::Default,
            bg: Color::Default,
            style: Style::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub index: u16,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub agent: AgentId,
    pub cols: u16,
    pub rows: u16,
    pub full: bool,
    pub lines: Vec<Row>,
    pub cursor: Option<(u16, u16)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Vec<Cell>>,
    pub cursor: Option<(u16, u16)>,
}

impl Screen {
    pub fn blank(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            lines: vec![vec![Cell::default(); usize::from(cols)]; usize::from(rows)],
            cursor: None,
        }
    }

    pub fn diff(&self, previous: Option<&Screen>, agent: AgentId) -> Option<Frame> {
        let full = previous.is_none_or(|p| p.cols != self.cols || p.rows != self.rows);
        let lines: Vec<Row> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(index, line)| {
                full || previous.is_some_and(|p| p.lines.get(*index) != Some(*line))
            })
            .map(|(index, line)| Row {
                index: index as u16,
                cells: line.clone(),
            })
            .collect();
        let cursor_moved = previous.is_none_or(|p| p.cursor != self.cursor);
        (full || !lines.is_empty() || cursor_moved).then_some(Frame {
            agent,
            cols: self.cols,
            rows: self.rows,
            full,
            lines,
            cursor: self.cursor,
        })
    }

    pub fn apply(&mut self, frame: &Frame) {
        if frame.full || frame.cols != self.cols || frame.rows != self.rows {
            *self = Screen::blank(frame.cols, frame.rows);
        }
        for row in &frame.lines {
            if let Some(line) = self.lines.get_mut(usize::from(row.index)) {
                *line = row.cells.clone();
            }
        }
        self.cursor = frame.cursor;
    }

    pub fn text(&self) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        lines.join("\n").trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_with(text_rows: &[&str], cols: u16) -> Screen {
        let mut screen = Screen::blank(cols, text_rows.len() as u16);
        for (row, text) in text_rows.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                screen.lines[row][col].text = ch.to_string();
            }
        }
        screen
    }

    #[test]
    fn blank_screen_is_spaces_and_empty_text() {
        let screen = Screen::blank(3, 2);
        assert_eq!(screen.lines.len(), 2);
        assert_eq!(screen.lines[1].len(), 3);
        assert_eq!(screen.text(), "");
    }

    #[test]
    fn first_diff_is_full() {
        let agent = AgentId::random();
        let screen = screen_with(&["ab", "cd"], 4);
        let frame = screen.diff(None, agent).expect("frame");
        assert!(frame.full);
        assert_eq!(frame.lines.len(), 2);
        assert_eq!(frame.agent, agent);
    }

    #[test]
    fn identical_screens_produce_no_frame() {
        let screen = screen_with(&["ab", "cd"], 4);
        assert_eq!(screen.diff(Some(&screen.clone()), AgentId::random()), None);
    }

    #[test]
    fn only_changed_rows_are_sent() {
        let before = screen_with(&["ab", "cd", "ef"], 4);
        let after = screen_with(&["ab", "cX", "ef"], 4);
        let frame = after.diff(Some(&before), AgentId::random()).expect("frame");
        assert!(!frame.full);
        assert_eq!(frame.lines.len(), 1);
        assert_eq!(frame.lines[0].index, 1);
    }

    #[test]
    fn cursor_move_alone_produces_a_frame_without_rows() {
        let before = screen_with(&["ab"], 4);
        let mut after = before.clone();
        after.cursor = Some((2, 0));
        let frame = after.diff(Some(&before), AgentId::random()).expect("frame");
        assert!(frame.lines.is_empty());
        assert_eq!(frame.cursor, Some((2, 0)));
    }

    #[test]
    fn resize_forces_a_full_frame() {
        let before = screen_with(&["ab"], 4);
        let after = screen_with(&["ab"], 5);
        assert!(
            after
                .diff(Some(&before), AgentId::random())
                .expect("frame")
                .full
        );
    }

    #[test]
    fn applying_diffs_reproduces_the_screen() {
        let agent = AgentId::random();
        let first = screen_with(&["hello", "world"], 6);
        let mut second = screen_with(&["hello", "there"], 6);
        second.cursor = Some((5, 1));

        let mut mirror = Screen::blank(0, 0);
        mirror.apply(&first.diff(None, agent).expect("frame"));
        assert_eq!(mirror, first);
        mirror.apply(&second.diff(Some(&first), agent).expect("frame"));
        assert_eq!(mirror, second);
        assert_eq!(mirror.text(), "hello\nthere");
    }

    #[test]
    fn wide_char_spacers_add_no_text() {
        let mut screen = Screen::blank(4, 1);
        screen.lines[0][0].text = "日".to_string();
        screen.lines[0][1].text = String::new();
        screen.lines[0][2].text = "b".to_string();
        assert_eq!(screen.text(), "日b");
    }
}
