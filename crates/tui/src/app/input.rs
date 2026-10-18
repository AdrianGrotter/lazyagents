use std::ops::RangeInclusive;

use protocol::{InputEvent, Request, Screen, Status};
use ratatui::layout::{Position, Rect};

use crate::keymap::Action;
use crate::layout::{self, Areas};

use super::forms::Field;
use super::pane::scroll_request;
use super::{App, Effect, Focus, Popup, start_verb};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Drag,
    Up,
    ScrollUp,
    ScrollDown,
}

/// A left-button or wheel event at absolute terminal coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub kind: MouseKind,
    pub col: u16,
    pub row: u16,
}

/// Cells of the pane as `(col, row)` relative to the pane's inner area; `anchor` is where
/// the drag started, so it may come after `end` in reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub end: (u16, u16),
}

impl Selection {
    fn ordered(self) -> ((u16, u16), (u16, u16)) {
        let reading = |(col, row): (u16, u16)| (row, col);
        if reading(self.anchor) <= reading(self.end) {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        }
    }

    pub fn rows(self) -> RangeInclusive<u16> {
        let ((_, first), (_, last)) = self.ordered();
        first..=last
    }

    /// The selected columns of `row` in a line `width` cells wide: from the start column
    /// on the first row, up to the end column on the last, the whole line in between.
    pub fn columns(self, row: u16, width: u16) -> Option<RangeInclusive<u16>> {
        let ((start_col, start_row), (end_col, end_row)) = self.ordered();
        if width == 0 || !(start_row..=end_row).contains(&row) {
            return None;
        }
        let first = if row == start_row { start_col } else { 0 };
        let last = if row == end_row { end_col } else { u16::MAX };
        let last = last.min(width - 1);
        (first <= last).then_some(first..=last)
    }

    /// Spacer cells after wide characters have no text and are skipped; trailing spaces
    /// are trimmed from every line.
    pub fn text(self, screen: &Screen) -> String {
        self.rows()
            .map(|row| {
                let Some(line) = screen.lines.get(usize::from(row)) else {
                    return String::new();
                };
                let width = u16::try_from(line.len()).unwrap_or(u16::MAX);
                let Some(columns) = self.columns(row, width) else {
                    return String::new();
                };
                let text: String = columns
                    .map(|col| line[usize::from(col)].text.as_str())
                    .collect();
                text.trim_end_matches(' ').to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(super) const WHEEL_LINES: i32 = 3;
/// `at` as a cell of `area`, moved inside it when the pointer has left it.
pub(super) fn clamped(at: Position, area: Rect) -> (u16, u16) {
    let col =
        at.x.clamp(area.x, area.right().saturating_sub(1).max(area.x));
    let row =
        at.y.clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
    (col - area.x, row - area.y)
}

impl App {
    pub fn on_paste(&mut self, text: String) -> Vec<Effect> {
        if self.too_small || self.popup.as_ref().is_some_and(Popup::is_sending) {
            return Vec::new();
        }
        if self.popup.is_none() {
            self.prefix_pending = false;
            return match (&self.focus, self.selected_agent()) {
                (Focus::Pane, Some(agent)) => {
                    vec![Effect::Send(Request::Input {
                        id: agent.id,
                        event: InputEvent::Paste(text),
                    })]
                }
                _ => Vec::new(),
            };
        }
        let line = text
            .split(['\r', '\n'])
            .next()
            .unwrap_or_default()
            .to_string();
        match &mut self.popup {
            Some(Popup::Rename { input, .. }) => input.push_str(&line),
            Some(Popup::CommandPrompt(run)) => run.input.push_str(&line),
            Some(Popup::Comment { input, .. }) => input.push_str(&line),
            Some(Popup::Help(help)) if help.typing => {
                help.filter.push_str(&line);
                help.scroll = 0;
            }
            Some(Popup::Reply { input, .. }) => {
                input.push_str(&text.replace("\r\n", " ").replace(['\r', '\n'], " "))
            }
            Some(Popup::AddProject(form)) => form.focused().push_str(&line),
            Some(Popup::NewAgent(form)) => match form.field {
                Field::Name => form.name.push_str(&line),
                Field::Command => form.command.push_str(&line),
                Field::Base => form.base.push_str(&line),
                Field::Prompt => form
                    .prompt
                    .push_str(&text.replace("\r\n", " ").replace(['\r', '\n'], " ")),
                Field::Preset | Field::Worktree => {}
            },
            Some(Popup::Filter) => {
                let filter = format!("{}{}", self.filter, line);
                let filter: String = filter.chars().filter(|c| !c.is_control()).collect();
                return self.set_filter(filter);
            }
            Some(
                Popup::Help(_)
                | Popup::ProjectPicker { .. }
                | Popup::Confirm { .. }
                | Popup::Commands { .. }
                | Popup::Finish { .. }
                | Popup::ApproveSetup { .. }
                | Popup::Output { .. },
            )
            | None => {}
        }
        Vec::new()
    }

    pub fn on_wheel(&mut self, up: bool, col: u16, row: u16) -> Vec<Effect> {
        if self.popup.is_some() {
            return Vec::new();
        }
        if matches!(self.focus, Focus::Diff(_)) {
            self.scroll_diff(if up {
                -i64::from(WHEEL_LINES)
            } else {
                i64::from(WHEEL_LINES)
            });
            return Vec::new();
        }
        if let Some(mode) = self.scroll_mode() {
            let lines = if up { WHEEL_LINES } else { -WHEEL_LINES };
            return vec![scroll_request(mode.agent, lines)];
        }
        let Some(agent) = self.selected_agent() else {
            return Vec::new();
        };
        if agent.status == Status::Stopped {
            return Vec::new();
        }
        let request = if agent.status.is_running() {
            Request::Input {
                id: agent.id,
                event: InputEvent::Wheel { up, col, row },
            }
        } else {
            Request::Scroll {
                id: agent.id,
                lines: if up { WHEEL_LINES } else { -WHEEL_LINES },
            }
        };
        vec![Effect::Send(request)]
    }

    pub fn on_mouse(&mut self, event: MouseInput, areas: &Areas) -> Vec<Effect> {
        if event.kind == MouseKind::Down {
            self.message_seen = true;
        }
        if self.too_small {
            if event.kind == MouseKind::Up {
                self.selection = None;
            }
            return Vec::new();
        }
        let at = Position::new(event.col, event.row);
        let pane = areas.pane_inner;
        match event.kind {
            MouseKind::ScrollUp | MouseKind::ScrollDown if pane.contains(at) => self.on_wheel(
                event.kind == MouseKind::ScrollUp,
                event.col - pane.x,
                event.row - pane.y,
            ),
            MouseKind::ScrollUp | MouseKind::ScrollDown => Vec::new(),
            MouseKind::Down => {
                self.selection = None;
                if self.popup.is_some() {
                    let outside = areas.popup.is_some_and(|r| !r.contains(at));
                    if outside || !self.popup.as_ref().is_some_and(Popup::is_sending) {
                        self.interactions += 1;
                    }
                    if outside {
                        self.popup = None;
                    }
                    return Vec::new();
                }
                self.interactions += 1;
                self.on_click(at, areas)
            }
            MouseKind::Drag => {
                if let Some(selection) = self.selection.as_mut() {
                    selection.end = clamped(at, pane);
                }
                Vec::new()
            }
            MouseKind::Up => {
                let Some(mut selection) = self.selection.take() else {
                    return Vec::new();
                };
                selection.end = clamped(at, pane);
                if selection.anchor == selection.end {
                    return self.click_agent(selection.end);
                }
                let text = selection.text(&self.screen);
                if text.is_empty() {
                    return Vec::new();
                }
                self.inform(format!("copied {} characters", text.chars().count()));
                vec![Effect::Copy(text)]
            }
        }
    }

    fn click_agent(&self, (col, row): (u16, u16)) -> Vec<Effect> {
        match self.selected_agent() {
            Some(agent) if self.focus == Focus::Pane && agent.status.is_running() => {
                vec![Effect::Send(Request::Input {
                    id: agent.id,
                    event: InputEvent::Click { col, row },
                })]
            }
            _ => Vec::new(),
        }
    }

    /// A click is an explicit action, so a click in the pane on a running agent focuses
    /// it; a click on an Agents row never does.
    fn on_click(&mut self, at: Position, areas: &Areas) -> Vec<Effect> {
        if areas.projects.contains(at) {
            let mut effects = self.leave_for_sidebar(Focus::Projects);
            let row = layout::row_at(
                areas.projects,
                self.project_index,
                self.projects.len(),
                at.y,
            );
            if let Some(index) = row {
                self.highlight_project(index);
            }
            effects.extend(self.sync());
            return effects;
        }
        if areas.agents.contains(at) {
            let mut effects = self.leave_for_sidebar(Focus::Agents);
            let agents = self.visible_agents();
            let selected = self.selected_row().unwrap_or(0);
            let row = layout::row_at(areas.agents, selected, agents.len(), at.y);
            if let Some(id) = row.map(|index| agents[index].id) {
                self.select_agent(id);
            }
            effects.extend(self.sync());
            return effects;
        }
        if !areas.pane_inner.contains(at) || matches!(self.focus, Focus::Diff(_)) {
            return Vec::new();
        }
        let Some((name, running, status)) = self
            .selected_agent()
            .map(|a| (a.name.clone(), a.status.is_running(), a.status))
        else {
            return Vec::new();
        };
        let cell = clamped(at, areas.pane_inner);
        self.selection = Some(Selection {
            anchor: cell,
            end: cell,
        });
        if matches!(self.focus, Focus::Scroll(_)) {
            return Vec::new();
        }
        if running {
            self.focus = Focus::Pane;
        } else {
            let message = format!(
                "{name} is not running — {} {}s it",
                self.keymap.first_key(Action::Open),
                start_verb(status)
            );
            self.fail(message);
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        agent, code, ctrl, in_pane, key, loaded, mouse, open_diff, project, running_pair,
        screen_areas, scroll_effects, submit_shell_agent, two_agents,
    };
    use protocol::Cell;
    use protocol::{AgentInfo, Response, ServerMessage};
    use ratatui::crossterm::event::KeyCode;
    use std::time::Instant;

    #[test]
    fn paste_into_a_field_takes_the_first_line_of_any_line_ending() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('R'));
        let _ = app.on_key(ctrl('u'));
        let _ = app.on_paste("one\rtwo".into());
        assert!(
            matches!(&app.popup, Some(Popup::Rename { input, .. }) if input == "one"),
            "{:?}",
            app.popup
        );
        let _ = app.on_key(code(KeyCode::Esc));
        let _ = app.on_key(key('a'));
        let _ = app.on_key(ctrl('u'));
        let _ = app.on_paste("/tmp/x\r\n/tmp/y".into());
        assert!(
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.path == "/tmp/x"),
            "{:?}",
            app.popup
        );
        let _ = app.on_key(code(KeyCode::Tab));
        let _ = app.on_paste("My repo\nnext".into());
        assert!(
            matches!(&app.popup, Some(Popup::AddProject(form)) if form.name == "My repo"),
            "{:?}",
            app.popup
        );
    }

    #[test]
    fn a_click_inside_a_waiting_form_does_not_count_as_moving_on() {
        let (mut app, _, _) = two_agents();
        let (info, request) = submit_shell_agent(&mut app, "zed");
        let areas = screen_areas(&app);
        let form = crate::ui::popup_area(&app, areas.screen).expect("form on screen");
        let _ = app.on_mouse(mouse(MouseKind::Down, (form.x + 1, form.y + 1)), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Up, (form.x + 1, form.y + 1)), &areas);
        assert!(app.popup.as_ref().is_some_and(Popup::is_sending));
        let _ = app.apply_reply(&request, Ok(Response::Agent(info.clone())));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(info.id));
        assert_eq!(app.focus, Focus::Pane);
    }

    #[test]
    fn paste_goes_to_the_agent_or_the_popup() {
        let (mut app, a, _) = two_agents();
        assert!(app.on_paste("x".into()).is_empty());
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(
            app.on_paste("hello\nworld".into()),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Paste("hello\nworld".into())
            })]
        );
        let _ = app.on_key(ctrl(']'));
        let _ = app.on_key(key('n'));
        assert!(app.on_paste("pasted\nignored".into()).is_empty());
        match &app.popup {
            Some(Popup::NewAgent(form)) => assert_eq!(form.name, "pasted"),
            other => panic!("unexpected popup {other:?}"),
        }
    }

    #[test]
    fn wheel_inputs_running_agents_and_scrolls_exited_ones() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        assert_eq!(
            app.on_wheel(true, 3, 4),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Wheel {
                    up: true,
                    col: 3,
                    row: 4
                }
            })]
        );
        let stopped = AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        };
        let _ = app.apply_server(ServerMessage::AgentUpdated(stopped));
        assert_eq!(
            app.on_wheel(true, 0, 0),
            vec![Effect::Send(Request::Scroll { id: a.id, lines: 3 })]
        );
        assert_eq!(
            app.on_wheel(false, 0, 0),
            vec![Effect::Send(Request::Scroll {
                id: a.id,
                lines: -3
            })]
        );
        let _ = app.on_key(key('?'));
        assert!(app.on_wheel(true, 0, 0).is_empty());
    }

    fn agent_row_at(areas: &Areas, index: u16) -> (u16, u16) {
        (areas.agents.x + 3, areas.agents.y + 1 + index)
    }

    fn project_row_at(areas: &Areas, index: u16) -> (u16, u16) {
        (areas.projects.x + 3, areas.projects.y + 1 + index)
    }

    /// `世` is wide: it is followed by an empty spacer cell, as the daemon sends it.
    fn screen_of(lines: &[&str]) -> Screen {
        let mut screen = Screen::blank(20, lines.len() as u16);
        for (row, line) in lines.iter().enumerate() {
            let mut cells = Vec::new();
            for c in line.chars() {
                cells.push(Cell {
                    text: c.to_string(),
                    ..Cell::default()
                });
                if c == '世' {
                    cells.push(Cell {
                        text: String::new(),
                        ..Cell::default()
                    });
                }
            }
            for (col, cell) in cells.into_iter().enumerate() {
                screen.lines[row][col] = cell;
            }
        }
        screen
    }

    #[test]
    fn click_project_row_selects_and_focuses_projects() {
        let p = project("p");
        let q = project("q");
        let a = agent(&p, "alpha", Status::Idle);
        let b = agent(&q, "beta", Status::Idle);
        let (mut app, _) = loaded(vec![p, q], vec![a.clone(), b.clone()]);
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 1)), &areas);
        assert_eq!((app.focus.clone(), app.project_index), (Focus::Projects, 1));
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        assert!(
            app.on_mouse(mouse(MouseKind::Up, project_row_at(&areas, 1)), &areas)
                .is_empty()
        );
        assert_eq!(app.focus, Focus::Projects);

        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let _ = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas);
        assert_eq!(
            (app.focus.clone(), app.prefix_pending),
            (Focus::Projects, false)
        );
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
    }

    #[test]
    fn click_agent_row_selects_and_focuses_agents_not_pane() {
        let (mut app, a, b) = two_agents();
        app.focus = Focus::Projects;
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 1)), &areas);
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
        assert_eq!(effects, vec![Effect::Send(Request::Subscribe { id: b.id })]);
        assert!(
            app.on_mouse(mouse(MouseKind::Up, agent_row_at(&areas, 1)), &areas)
                .is_empty()
        );
        assert_eq!((app.focus.clone(), app.selection), (Focus::Agents, None));

        app.focus = Focus::Pane;
        app.prefix_pending = true;
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 0)), &areas);
        assert_eq!(
            (app.focus.clone(), app.prefix_pending),
            (Focus::Agents, false)
        );
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));

        app.focus = Focus::Projects;
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 5)), &areas);
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(a.id));
    }

    #[test]
    fn click_leaves_the_diff_and_scroll_views() {
        let (mut app, a, b) = two_agents();
        let areas = screen_areas(&app);
        open_diff(&mut app, a.id);
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert!(matches!(app.focus.clone(), Focus::Diff(_)) && app.selection.is_none());
        let _ = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 1)), &areas);
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));

        let _ = app.on_key(key('s'));
        let effects = app.on_mouse(mouse(MouseKind::Down, agent_row_at(&areas, 0)), &areas);
        assert_eq!(
            effects,
            vec![
                Effect::Send(Request::Scroll {
                    id: b.id,
                    lines: i32::MIN / 2
                }),
                Effect::Send(Request::Subscribe { id: a.id }),
            ]
        );
        assert_eq!(app.focus, Focus::Agents);

        let _ = app.on_key(key('s'));
        let effects = app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas);
        assert_eq!(effects, scroll_effects(a.id, i32::MIN / 2));
        assert_eq!(app.focus, Focus::Projects);
    }

    #[test]
    fn click_in_scroll_mode_selects_history_without_leaving() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["older line"]);
        let _ = app.on_key(key('s'));
        assert!(
            app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas)
                .is_empty()
        );
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 4, 0)), &areas);
        assert_eq!(effects, vec![Effect::Copy("older".into())]);
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
    }

    #[test]
    fn click_pane_focuses_running_agent_and_reaches_it() {
        let (mut app, a, _) = two_agents();
        let areas = screen_areas(&app);
        let at = in_pane(&areas, 4, 2);
        assert!(app.on_mouse(mouse(MouseKind::Down, at), &areas).is_empty());
        assert_eq!(app.focus, Focus::Pane);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: (4, 2),
                end: (4, 2)
            })
        );
        assert_eq!(
            app.on_mouse(mouse(MouseKind::Up, at), &areas),
            vec![Effect::Send(Request::Input {
                id: a.id,
                event: InputEvent::Click { col: 4, row: 2 }
            })]
        );
        assert_eq!((app.focus.clone(), app.selection), (Focus::Pane, None));

        let p = project("p");
        let (mut empty, _) = loaded(vec![p], Vec::new());
        assert!(
            empty
                .on_mouse(mouse(MouseKind::Down, at), &areas)
                .is_empty()
        );
        assert_eq!((empty.focus, empty.selection), (Focus::Agents, None));
    }

    #[test]
    fn click_pane_on_stopped_agent_keeps_focus() {
        let p = project("p");
        let a = agent(&p, "alpha", Status::Stopped);
        let (mut app, _) = loaded(vec![p], vec![a]);
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 1, 1)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("alpha is not running — enter starts it")
        );
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 1, 1)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
    }

    #[test]
    fn drag_selects_and_release_copies_text() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello world   ", "a世b   ", "last line here"]);
        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 6, 0)), &areas);
        assert!(
            app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 10, 1)), &areas)
                .is_empty()
        );
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 2, 2)), &areas);
        assert_eq!(
            app.selection,
            Some(Selection {
                anchor: (6, 0),
                end: (2, 2)
            })
        );
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 2, 2)), &areas);
        assert_eq!(effects, vec![Effect::Copy("world\na世b\nlas".into())]);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("copied 13 characters")
        );
        assert_eq!(app.selection, None);

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 2, 2)), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 1, 1)), &areas);
        assert_eq!(effects, vec![Effect::Copy("世b\nlas".into())]);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("copied 6 characters")
        );

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 2, 1)), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 9, 1)), &areas);
        assert_eq!(effects, vec![Effect::Copy("b".into())]);
    }

    #[test]
    fn plain_click_copies_nothing() {
        let (mut app, a, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello"]);
        let at = in_pane(&areas, 1, 0);
        let click = vec![Effect::Send(Request::Input {
            id: a.id,
            event: InputEvent::Click { col: 1, row: 0 },
        })];
        let _ = app.on_mouse(mouse(MouseKind::Down, at), &areas);
        assert_eq!(app.on_mouse(mouse(MouseKind::Up, at), &areas), click);
        let _ = app.on_mouse(mouse(MouseKind::Down, at), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 3, 0)), &areas);
        let _ = app.on_mouse(mouse(MouseKind::Drag, at), &areas);
        assert_eq!(app.on_mouse(mouse(MouseKind::Up, at), &areas), click);
        assert_eq!(app.selection, None);
        assert_eq!(app.current_message(Instant::now()), None);
    }

    #[test]
    fn a_click_in_scroll_mode_stays_out_of_the_agent() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        let _ = app.on_key(key('s'));
        let at = in_pane(&areas, 1, 0);
        let _ = app.on_mouse(mouse(MouseKind::Down, at), &areas);
        let effects = app.on_mouse(mouse(MouseKind::Up, at), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
    }

    #[test]
    fn click_outside_popup_closes_it() {
        let (mut app, _, _) = two_agents();
        let _ = app.on_key(key('?'));
        let areas = screen_areas(&app);
        let popup = areas.popup.expect("popup area");
        let inside = (popup.x + 1, popup.y + 1);
        assert!(
            app.on_mouse(mouse(MouseKind::Down, inside), &areas)
                .is_empty()
        );
        assert!(
            app.on_mouse(mouse(MouseKind::Up, inside), &areas)
                .is_empty()
        );
        assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
        assert!(
            app.on_mouse(mouse(MouseKind::Down, project_row_at(&areas, 0)), &areas)
                .is_empty()
        );
        assert_eq!((&app.popup, app.focus.clone()), (&None, Focus::Agents));

        let _ = app.on_key(key('x'));
        let areas = screen_areas(&app);
        let effects = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 0, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(
            (&app.popup, app.focus.clone(), app.selection),
            (&None, Focus::Agents, None)
        );
        assert!(
            app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 5, 0)), &areas)
                .is_empty()
        );
    }

    #[test]
    fn a_release_while_too_small_drops_the_selection() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        app.screen = screen_of(&["hello"]);
        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 1, 0)), &areas);
        assert!(app.selection.is_some());
        let _ = app.set_pane_size(5, 2);
        let effects = app.on_mouse(mouse(MouseKind::Up, in_pane(&areas, 3, 0)), &areas);
        assert!(effects.is_empty(), "{effects:?}");
        assert_eq!(app.selection, None);
    }

    #[test]
    fn the_wheel_over_a_stopped_agent_does_nothing() {
        let p = project("p");
        let stopped = agent(&p, "halted", Status::Stopped);
        let exited = agent(&p, "done", Status::Exited(0));
        let (mut app, _) = loaded(vec![p], vec![stopped.clone(), exited.clone()]);
        let areas = screen_areas(&app);
        app.select_agent(stopped.id);
        let wheel = mouse(MouseKind::ScrollUp, in_pane(&areas, 1, 1));
        assert_eq!(app.on_mouse(wheel, &areas), Vec::new());
        assert_eq!(app.current_message(Instant::now()), None);
        app.select_agent(exited.id);
        assert_eq!(
            app.on_mouse(wheel, &areas),
            vec![Effect::Send(Request::Scroll {
                id: exited.id,
                lines: WHEEL_LINES
            })]
        );
    }

    #[test]
    fn drag_is_clamped_to_the_pane() {
        let (mut app, _, _) = two_agents();
        let areas = screen_areas(&app);
        let (width, height) = (areas.pane_inner.width, areas.pane_inner.height);
        let _ = app.on_mouse(mouse(MouseKind::Drag, in_pane(&areas, 1, 1)), &areas);
        assert_eq!(app.selection, None);
        let _ = app.on_mouse(mouse(MouseKind::Down, (0, 29)), &areas);
        assert_eq!(app.selection, None);

        let _ = app.on_mouse(mouse(MouseKind::Down, in_pane(&areas, 5, 5)), &areas);
        let end = |app: &App| app.selection.map(|s| s.end);
        let _ = app.on_mouse(mouse(MouseKind::Drag, (0, 29)), &areas);
        assert_eq!(end(&app), Some((0, height - 1)));
        let _ = app.on_mouse(mouse(MouseKind::Drag, (99, 0)), &areas);
        assert_eq!(end(&app), Some((width - 1, 0)));
        let _ = app.on_mouse(mouse(MouseKind::Drag, (500, 500)), &areas);
        assert_eq!(end(&app), Some((width - 1, height - 1)));
    }

    #[test]
    fn wheel_scrolls_history_in_scroll_mode() {
        let (mut app, a, _) = running_pair();
        let areas = screen_areas(&app);
        let _ = app.on_key(key('s'));
        let at = in_pane(&areas, 1, 1);
        assert_eq!(
            app.on_mouse(mouse(MouseKind::ScrollUp, at), &areas),
            scroll_effects(a.id, 3)
        );
        assert_eq!(
            app.on_mouse(mouse(MouseKind::ScrollDown, at), &areas),
            scroll_effects(a.id, -3)
        );
        assert!(
            matches!(app.focus.clone(), Focus::Scroll(_)),
            "{:?}",
            app.focus
        );
    }

    #[test]
    fn wheel_outside_the_pane_does_nothing() {
        let (mut app, _, _) = running_pair();
        let areas = screen_areas(&app);
        assert!(
            app.on_mouse(mouse(MouseKind::ScrollUp, agent_row_at(&areas, 0)), &areas)
                .is_empty()
        );
    }
}
