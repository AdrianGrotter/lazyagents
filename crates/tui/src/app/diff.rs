use std::time::Instant;

use protocol::{AgentId, DiffScope, InputEvent, Request};
use ratatui::crossterm::event::KeyEvent;

use crate::fixed_keys::{self, DiffKey, Motion};
use crate::keymap::Action;
use crate::review::{self, Comment, Location};

use super::pane::press;
use super::{App, DEFAULT_SIZE, Effect, Focus, Popup, label, printable};

pub const DIFF_LINE_CAP: usize = 20_000;
/// `lines` is `None` until the daemon's diff arrives; `locations` holds each line's place
/// in the files, for comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub agent: AgentId,
    pub scope: DiffScope,
    pub lines: Option<Vec<String>>,
    pub locations: Vec<Option<Location>>,
    pub scroll: usize,
    pub cursor: usize,
}

impl DiffView {
    pub fn files(&self) -> usize {
        self.lines.as_ref().map_or(0, |lines| {
            lines.iter().filter(|l| l.starts_with("diff --git")).count()
        })
    }
}

/// How many of `comments` sit on `location`: 0 or 1.
pub(super) fn comments_at(comments: &[Comment], location: Option<&Option<Location>>) -> usize {
    match location {
        Some(Some(location)) => comments.iter().filter(|c| c.location == *location).count(),
        _ => 0,
    }
}

/// Splits a diff into printable display lines, at most `DIFF_LINE_CAP` plus a truncation
/// notice.
pub(super) fn diff_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().take(DIFF_LINE_CAP).map(printable).collect();
    let rest = text.lines().skip(DIFF_LINE_CAP).count();
    if rest > 0 {
        lines.push(format!("… diff truncated ({rest} more lines)"));
    }
    lines
}

impl App {
    /// A reply for another scope than the view shows is dropped, since the user moved on.
    pub fn apply_scoped_diff(
        &mut self,
        id: AgentId,
        scope: DiffScope,
        result: Result<String, String>,
    ) {
        self.diffs_in_flight.remove(&(id, scope));
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        if view.agent != id || view.scope != scope || view.lines.is_some() {
            return;
        }
        match result {
            Ok(text) => {
                let lines = diff_lines(&text);
                view.locations = review::locations(&lines);
                view.lines = Some(lines);
            }
            Err(message) => {
                self.close_diff();
                self.fail(message);
            }
        }
    }

    pub(super) fn close_diff(&mut self) {
        self.focus = Focus::Agents;
    }

    /// The diff view, while it has the focus.
    pub fn diff(&self) -> Option<&DiffView> {
        match &self.focus {
            Focus::Diff(view) => Some(view),
            _ => None,
        }
    }

    fn closes_diff(&self, key: &KeyEvent) -> bool {
        fixed_keys::diff_key(key) == Some(DiffKey::Close)
            || self.keymap.sidebar(key) == Some(Action::Diff)
    }

    pub(super) fn diff_page(&self) -> usize {
        usize::from(self.pane_size.map_or(DEFAULT_SIZE.1, |(_, rows)| rows)).max(1)
    }

    /// Closes the view or quits; `None` for any other key.
    pub(super) fn leave_diff(&mut self, key: &KeyEvent) -> Option<Vec<Effect>> {
        if self.closes_diff(key) {
            self.close_diff();
            return Some(Vec::new());
        }
        (self.keymap.sidebar(key) == Some(Action::Quit)).then(|| vec![Effect::Quit])
    }

    pub(super) fn on_diff_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if let Some(effects) = self.leave_diff(&key) {
            return effects;
        }
        let page = self.diff_page() as i64;
        let Some(action) = fixed_keys::diff_key(&key) else {
            return Vec::new();
        };
        let step = match action {
            DiffKey::Move(Motion::Down) => 1,
            DiffKey::Move(Motion::Up) => -1,
            DiffKey::Move(Motion::HalfPageDown) => page / 2,
            DiffKey::Move(Motion::HalfPageUp) => -page / 2,
            DiffKey::Move(Motion::PageDown) => page,
            DiffKey::Move(Motion::PageUp) => -page,
            DiffKey::Move(Motion::Top) => i64::MIN / 2,
            DiffKey::Move(Motion::Bottom) => i64::MAX / 2,
            DiffKey::Close => return Vec::new(),
            DiffKey::Comment => {
                self.open_comment();
                return Vec::new();
            }
            DiffKey::NextComment => {
                self.jump_to_comment(true);
                return Vec::new();
            }
            DiffKey::PreviousComment => {
                self.jump_to_comment(false);
                return Vec::new();
            }
            DiffKey::SendComments => return self.send_comments(),
            DiffKey::Scope => {
                let Some(view) = self.diff() else {
                    return Vec::new();
                };
                let (id, scope) = (view.agent, view.scope.next());
                return self.show_diff(id, scope);
            }
            DiffKey::NextFile => return self.jump_diff("diff --git", true),
            DiffKey::PreviousFile => return self.jump_diff("diff --git", false),
            DiffKey::NextHunk => return self.jump_diff("@@", true),
            DiffKey::PreviousHunk => return self.jump_diff("@@", false),
        };
        self.move_diff_cursor(step);
        Vec::new()
    }

    fn move_diff_cursor(&mut self, step: i64) {
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        let last = view.lines.as_ref().map_or(0, |l| l.len().saturating_sub(1)) as i64;
        view.cursor = (view.cursor as i64).saturating_add(step).clamp(0, last) as usize;
        self.keep_cursor_visible();
    }

    /// Scrolls the least that shows the cursor line, counting the comment rows drawn
    /// under commented lines.
    fn keep_cursor_visible(&mut self) {
        let page = self.diff_page();
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        let commented = self
            .comments
            .get(&view.agent)
            .map_or(&[][..], Vec::as_slice);
        let rows = |from: usize, to: usize| {
            (from..=to)
                .map(|i| 1 + comments_at(commented, view.locations.get(i)))
                .sum::<usize>()
        };
        if view.cursor < view.scroll {
            view.scroll = view.cursor;
        }
        while view.scroll < view.cursor && rows(view.scroll, view.cursor) > page {
            view.scroll += 1;
        }
    }

    pub(super) fn scroll_diff(&mut self, step: i64) {
        let page = self.diff_page();
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        let last = view
            .lines
            .as_ref()
            .map_or(0, |lines| lines.len().saturating_sub(page)) as i64;
        view.scroll = (view.scroll as i64).saturating_add(step).clamp(0, last) as usize;
        view.cursor = view
            .cursor
            .clamp(view.scroll, view.scroll + page.saturating_sub(1));
        let lines = view.lines.as_ref().map_or(0, Vec::len);
        view.cursor = view.cursor.min(lines.saturating_sub(1));
    }

    fn open_comment(&mut self) {
        let Focus::Diff(view) = &self.focus else {
            return;
        };
        let (Some(Some(location)), Some(code)) = (
            view.locations.get(view.cursor),
            view.lines.as_ref().and_then(|l| l.get(view.cursor)),
        ) else {
            self.fail("comment on a changed or context line");
            return;
        };
        let input = self
            .comments
            .get(&view.agent)
            .and_then(|c| c.iter().find(|c| c.location == *location))
            .map(|c| c.text.clone())
            .unwrap_or_default();
        self.popup = Some(Popup::Comment {
            agent: view.agent,
            location: location.clone(),
            code: code.clone(),
            input,
        });
    }

    /// An empty comment removes the one on that line.
    pub(super) fn save_comment(
        &mut self,
        agent: AgentId,
        location: Location,
        code: String,
        text: String,
    ) {
        let comments = self.comments.entry(agent).or_default();
        comments.retain(|c| c.location != location);
        let text = text.trim().to_string();
        if !text.is_empty() {
            comments.push(Comment {
                location,
                code,
                text,
            });
        }
        if comments.is_empty() {
            self.comments.remove(&agent);
        }
        self.keep_cursor_visible();
    }

    fn jump_to_comment(&mut self, forward: bool) {
        let Focus::Diff(view) = &mut self.focus else {
            return;
        };
        let commented = self
            .comments
            .get(&view.agent)
            .map_or(&[][..], Vec::as_slice);
        let count = view.locations.len();
        let target = (1..=count)
            .map(|offset| match forward {
                true => (view.cursor + offset) % count,
                false => (view.cursor + count - offset % count) % count,
            })
            .find(|&i| comments_at(commented, view.locations.get(i)) > 0);
        match target {
            Some(line) => {
                view.cursor = line;
                self.keep_cursor_visible();
            }
            None => self.fail("no comments on this diff"),
        }
    }

    /// Opens the diff view on `scope` of `id`'s changes, asking the daemon unless that
    /// diff is already on its way.
    pub(super) fn show_diff(&mut self, id: AgentId, scope: DiffScope) -> Vec<Effect> {
        self.focus = Focus::Diff(DiffView {
            agent: id,
            scope,
            lines: None,
            locations: Vec::new(),
            scroll: 0,
            cursor: 0,
        });
        if self.diffs_in_flight.contains_key(&(id, scope)) {
            return Vec::new();
        }
        self.diffs_in_flight.insert((id, scope), Instant::now());
        vec![Effect::Send(Request::GitDiff { id, scope })]
    }

    /// Moves the cursor to the next or previous line starting with `prefix`: a file
    /// header or a hunk header. It stays put when there is none that way.
    fn jump_diff(&mut self, prefix: &str, forward: bool) -> Vec<Effect> {
        let Focus::Diff(view) = &mut self.focus else {
            return Vec::new();
        };
        let Some(lines) = &view.lines else {
            return Vec::new();
        };
        let found = if forward {
            (view.cursor + 1..lines.len()).find(|&i| lines[i].starts_with(prefix))
        } else {
            (0..view.cursor)
                .rev()
                .find(|&i| lines[i].starts_with(prefix))
        };
        if let Some(line) = found {
            view.cursor = line;
            self.keep_cursor_visible();
        }
        Vec::new()
    }

    /// Sends every comment on the agent's diff as one prompt, in diff order, and returns
    /// to the sidebar to watch it work.
    fn send_comments(&mut self) -> Vec<Effect> {
        let Focus::Diff(view) = &self.focus else {
            return Vec::new();
        };
        let id = view.agent;
        let order = |location: &Location| {
            view.locations
                .iter()
                .position(|l| l.as_ref() == Some(location))
        };
        let Some(mut comments) = self.comments.get(&id).cloned() else {
            self.fail("no comments to send");
            return Vec::new();
        };
        comments.sort_by_key(|c| order(&c.location));
        let Some(agent) = self.agents.iter().find(|a| a.id == id) else {
            return Vec::new();
        };
        if !agent.status.is_running() {
            let message = format!("{} is not running", label(agent));
            self.fail(message);
            return Vec::new();
        }
        let message = format!(
            "sent {} comment{} to {}",
            comments.len(),
            if comments.len() == 1 { "" } else { "s" },
            label(agent)
        );
        self.comments.remove(&id);
        self.close_diff();
        self.inform(message);
        vec![
            Effect::Send(Request::Input {
                id,
                event: InputEvent::Paste(review::prompt(&comments)),
            }),
            press(id, protocol::KeyCode::Enter),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{
        apply_diff, code, ctrl, key, open_diff, running_pair, scroll_effects, two_agents,
    };
    use protocol::{AgentInfo, ServerMessage, Status};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use std::time::Duration;

    fn diff_place(app: &App) -> (usize, usize) {
        let view = app.diff().expect("diff view");
        (view.cursor, view.scroll)
    }

    #[test]
    fn modified_page_arrow_and_escape_keys_still_act_but_alt_letters_do_not() {
        let with = |code: KeyCode, modifiers: KeyModifiers| KeyEvent::new(code, modifiers);
        let (mut app, a, _) = two_agents();
        let _ = app.set_pane_size(80, 10);
        open_diff(&mut app, a.id);
        let text: Vec<String> = (0..100).map(|i| format!(" line {i}")).collect();
        apply_diff(&mut app, a.id, Ok(text.join("\n")));
        for (event, expected) in [
            (with(KeyCode::PageDown, KeyModifiers::CONTROL), (10, 1)),
            (with(KeyCode::PageUp, KeyModifiers::SHIFT), (0, 0)),
            (with(KeyCode::Down, KeyModifiers::SHIFT), (1, 0)),
            (with(KeyCode::Up, KeyModifiers::ALT), (0, 0)),
            (with(KeyCode::Char('j'), KeyModifiers::ALT), (0, 0)),
        ] {
            let _ = app.on_key(event);
            assert_eq!(diff_place(&app), expected, "{event:?}");
        }
        let _ = app.on_key(with(KeyCode::Esc, KeyModifiers::ALT));
        assert_eq!(app.focus, Focus::Agents);

        let (mut app, a, _) = running_pair();
        let _ = app.on_key(key('s'));
        assert_eq!(
            app.on_key(with(KeyCode::PageDown, KeyModifiers::CONTROL)),
            scroll_effects(a.id, -10)
        );
        assert!(
            app.on_key(with(KeyCode::Char('j'), KeyModifiers::ALT))
                .is_empty()
        );
        let _ = app.on_key(with(KeyCode::Esc, KeyModifiers::SHIFT));
        assert_eq!(app.focus, Focus::Agents);

        let (mut app, _, _) = two_agents();
        app.set_screen_size(60, 20);
        let _ = app.on_key(key('?'));
        let _ = app.on_key(with(KeyCode::Down, KeyModifiers::SHIFT));
        let _ = app.on_key(with(KeyCode::PageDown, KeyModifiers::CONTROL));
        assert!(matches!(app.popup, Some(Popup::Help(_))), "{:?}", app.popup);
        let Some(Popup::Help(help)) = &app.popup else {
            panic!("no help: {:?}", app.popup);
        };
        assert!(help.scroll > 1, "{}", help.scroll);
    }

    #[test]
    fn diff_view_opens_scrolls_and_closes() {
        let (mut app, a, b) = two_agents();
        let _ = app.set_pane_size(80, 10);
        open_diff(&mut app, a.id);
        assert_eq!(app.diff().and_then(|d| d.lines.as_ref()), None);
        let text: Vec<String> = (0..100).map(|i| format!(" line {i}")).collect();
        apply_diff(&mut app, b.id, Ok("+other".into()));
        assert_eq!(app.diff().and_then(|d| d.lines.as_ref()), None);
        apply_diff(&mut app, a.id, Ok(text.join("\n")));
        assert_eq!(app.diff().and_then(|d| d.lines.as_ref()), Some(&text));
        for (event, expected) in [
            (key('j'), (1, 0)),
            (key('k'), (0, 0)),
            (key('k'), (0, 0)),
            (ctrl('d'), (5, 0)),
            (ctrl('d'), (10, 1)),
            (ctrl('u'), (5, 1)),
            (code(KeyCode::PageDown), (15, 6)),
            (code(KeyCode::PageUp), (5, 5)),
            (key('G'), (99, 90)),
            (key('j'), (99, 90)),
            (code(KeyCode::PageDown), (99, 90)),
            (key('g'), (0, 0)),
        ] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert_eq!(diff_place(&app), expected, "{event:?}");
        }
        for event in [key('x'), key('d'), key('n'), key('S'), code(KeyCode::Enter)] {
            assert!(app.on_key(event).is_empty(), "{event:?}");
            assert!(
                matches!((&app.focus, &app.popup), (Focus::Diff(_), None)),
                "{event:?}"
            );
        }
        for close in [key('D'), key('q'), code(KeyCode::Esc)] {
            assert!(app.on_key(close).is_empty());
            assert_eq!(app.focus, Focus::Agents, "{close:?}");
            assert_eq!(app.diff(), None, "{close:?}");
            assert!(!app.prefix_pending);
            open_diff(&mut app, a.id);
            apply_diff(&mut app, a.id, Ok(String::new()));
        }
        apply_diff(&mut app, a.id, Ok("+late".into()));
        let _ = app.on_key(key('q'));
        apply_diff(&mut app, a.id, Ok("+late".into()));
        assert_eq!(app.focus, Focus::Agents);

        open_diff(&mut app, a.id);
        let _ = app.apply_server(ServerMessage::AgentUpdated(AgentInfo {
            status: Status::Exited(0),
            ..a.clone()
        }));
        assert!(
            matches!(app.focus.clone(), Focus::Diff(_)),
            "{:?}",
            app.focus
        );
        let _ = app.apply_server(ServerMessage::AgentRemoved(a.id));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(app.selected_agent().map(|x| x.id), Some(b.id));
    }

    #[test]
    fn diff_is_requested_once_while_loading() {
        let (mut app, a, b) = two_agents();
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        assert!(app.on_key(key('D')).is_empty(), "a's diff is still loading");
        assert!(
            matches!(app.focus.clone(), Focus::Diff(_)),
            "{:?}",
            app.focus
        );
        assert_eq!(app.diff().map(|d| d.agent), Some(a.id));
        apply_diff(&mut app, a.id, Ok("+one".into()));
        assert_eq!(
            app.diff().and_then(|d| d.lines.clone()),
            Some(vec!["+one".to_string()])
        );
        let _ = app.on_key(key('q'));
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('j'));
        open_diff(&mut app, b.id);
    }

    #[test]
    fn unanswered_diff_is_given_up_after_30s() {
        let (mut app, a, _) = two_agents();
        let t0 = Instant::now();
        open_diff(&mut app, a.id);
        let _ = app.on_key(key('q'));
        let _ = app.on_tick(t0 + Duration::from_secs(29));
        assert!(app.on_key(key('D')).is_empty());
        let _ = app.on_key(key('q'));
        let _ = app.on_tick(t0 + Duration::from_secs(31));
        open_diff(&mut app, a.id);
    }

    #[test]
    fn quit_keys_quit_from_the_diff_view() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);
        let _ = app.set_pane_size(10, 3);
        assert_eq!(app.on_key(ctrl('c')), vec![Effect::Quit]);
        assert!(app.on_key(key('q')).is_empty());
        assert_eq!(app.focus, Focus::Agents);
    }

    #[test]
    fn diff_view_caps_huge_diffs() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        let text: Vec<String> = (0..DIFF_LINE_CAP + 5).map(|i| format!("+{i}")).collect();
        apply_diff(&mut app, a.id, Ok(text.join("\n")));
        let lines = app.diff().and_then(|d| d.lines.clone()).expect("lines");
        assert_eq!(lines.len(), DIFF_LINE_CAP + 1);
        assert_eq!(lines[DIFF_LINE_CAP - 1], format!("+{}", DIFF_LINE_CAP - 1));
        assert_eq!(lines[DIFF_LINE_CAP], "… diff truncated (5 more lines)");

        let _ = app.on_key(key('q'));
        open_diff(&mut app, a.id);
        apply_diff(&mut app, a.id, Ok("+\tx\u{1b}[2J\r".into()));
        let lines = app.diff().and_then(|d| d.lines.clone()).expect("lines");
        assert_eq!(lines, ["+    x[2J"]);
    }

    #[test]
    fn diff_error_closes_with_message() {
        let (mut app, a, _) = two_agents();
        open_diff(&mut app, a.id);
        apply_diff(&mut app, a.id, Err("not a git repository".into()));
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("not a git repository")
        );
    }
}

#[cfg(test)]
mod comment_tests {
    use super::*;
    use crate::app::tests::{agent, apply_diff, loaded, project};
    use protocol::{AgentInfo, Status};
    use protocol::{InputEvent, Key as ProtoKey, KeyCode as ProtoCode, Modifiers};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n\
                        --- a/src/a.rs\n\
                        +++ b/src/a.rs\n\
                        @@ -10,2 +10,2 @@\n \
                        keep\n\
                        -old\n\
                        +new";

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            let _ = app.on_key(key(c));
        }
    }

    fn with_diff() -> (App, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "api", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let _ = app.set_pane_size(80, 10);
        let _ = app.on_key(key('D'));
        apply_diff(&mut app, a.id, Ok(DIFF.into()));
        (app, a)
    }

    fn to_line(app: &mut App, line: usize) {
        let _ = app.on_key(key('g'));
        for _ in 0..line {
            let _ = app.on_key(key('j'));
        }
    }

    fn comment(app: &mut App, line: usize, text: &str) {
        to_line(app, line);
        let _ = app.on_key(key('c'));
        typed(app, text);
        let _ = app.on_key(code(KeyCode::Enter));
    }

    fn texts(app: &App, id: AgentId) -> Vec<String> {
        app.comments
            .get(&id)
            .map(|c| c.iter().map(|c| c.text.clone()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn c_comments_on_the_cursor_line_and_edits_or_drops_it() {
        let (mut app, a) = with_diff();
        comment(&mut app, 6, "name this better");
        assert!(
            matches!(app.focus.clone(), Focus::Diff(_)),
            "{:?}",
            app.focus
        );
        let comments = &app.comments[&a.id];
        assert_eq!(comments.len(), 1);
        assert_eq!(
            comments[0].location,
            crate::review::Location {
                file: "src/a.rs".into(),
                line: 11,
                removed: false
            }
        );
        assert_eq!(comments[0].code, "+new");
        to_line(&mut app, 6);
        let _ = app.on_key(key('c'));
        assert!(matches!(
            &app.popup,
            Some(Popup::Comment { input, .. }) if input == "name this better"
        ));
        let _ = app.on_key(ctrl_u());
        typed(&mut app, "rename to fresh");
        let _ = app.on_key(code(KeyCode::Enter));
        assert_eq!(texts(&app, a.id), ["rename to fresh"]);
        to_line(&mut app, 6);
        let _ = app.on_key(key('c'));
        let _ = app.on_key(ctrl_u());
        let _ = app.on_key(code(KeyCode::Enter));
        assert!(texts(&app, a.id).is_empty());
    }

    fn ctrl_u() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_header_line_takes_no_comment() {
        let (mut app, _) = with_diff();
        to_line(&mut app, 3);
        assert!(app.on_key(key('c')).is_empty());
        assert_eq!(app.popup, None);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("comment on a changed or context line")
        );
    }

    #[test]
    fn capital_s_sends_the_comments_in_diff_order_and_clears_them() {
        let (mut app, a) = with_diff();
        comment(&mut app, 6, "second");
        comment(&mut app, 4, "first");
        let effects = app.on_key(key('S'));
        let expected = crate::review::prompt(&app_comments_sorted());
        assert_eq!(
            effects,
            vec![
                Effect::Send(Request::Input {
                    id: a.id,
                    event: InputEvent::Paste(expected),
                }),
                Effect::Send(Request::Input {
                    id: a.id,
                    event: InputEvent::Key(ProtoKey::new(ProtoCode::Enter, Modifiers::NONE)),
                }),
            ]
        );
        assert!(texts(&app, a.id).is_empty());
        assert_eq!(app.focus, Focus::Agents);
        assert_eq!(
            app.current_message(Instant::now()),
            Some("sent 2 comments to api")
        );
    }

    fn app_comments_sorted() -> Vec<crate::review::Comment> {
        let at = |line, removed| crate::review::Location {
            file: "src/a.rs".into(),
            line,
            removed,
        };
        vec![
            crate::review::Comment {
                location: at(10, false),
                code: " keep".into(),
                text: "first".into(),
            },
            crate::review::Comment {
                location: at(11, false),
                code: "+new".into(),
                text: "second".into(),
            },
        ]
    }

    #[test]
    fn n_and_capital_n_jump_between_commented_lines() {
        let (mut app, _) = with_diff();
        comment(&mut app, 4, "a");
        comment(&mut app, 6, "b");
        to_line(&mut app, 0);
        let cursor = |app: &App| app.diff().map(|d| d.cursor);
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(4));
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(6));
        let _ = app.on_key(key('n'));
        assert_eq!(cursor(&app), Some(4));
        let _ = app.on_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT));
        assert_eq!(cursor(&app), Some(6));
    }

    #[test]
    fn comments_outlive_the_diff_view() {
        let (mut app, a) = with_diff();
        comment(&mut app, 5, "why?");
        let _ = app.on_key(key('q'));
        let _ = app.on_key(key('D'));
        apply_diff(&mut app, a.id, Ok(DIFF.into()));
        assert_eq!(texts(&app, a.id), ["why?"]);
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    use crate::app::tests::{agent, apply_diff, loaded, project};
    use protocol::DiffScope;
    use protocol::{AgentInfo, Status};
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    const DIFF: &str = "diff --git a/a.rs b/a.rs\n\
                        --- a/a.rs\n\
                        +++ b/a.rs\n\
                        @@ -1 +1 @@\n\
                        -x\n\
                        +y\n\
                        @@ -9 +9 @@\n\
                        -p\n\
                        +q\n\
                        diff --git a/b.rs b/b.rs\n\
                        --- a/b.rs\n\
                        +++ b/b.rs\n\
                        @@ -1 +1 @@\n\
                        -m\n\
                        +n";

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn with_diff() -> (App, AgentInfo) {
        let p = project("p");
        let a = agent(&p, "api", Status::Idle);
        let (mut app, _) = loaded(vec![p], vec![a.clone()]);
        let _ = app.set_pane_size(80, 30);
        assert_eq!(
            app.on_key(key('D')),
            vec![Effect::Send(Request::GitDiff {
                id: a.id,
                scope: DiffScope::All
            })]
        );
        apply_diff(&mut app, a.id, Ok(DIFF.into()));
        (app, a)
    }

    fn cursor(app: &App) -> usize {
        app.diff().expect("diff").cursor
    }

    #[test]
    fn brackets_jump_between_files_and_braces_between_hunks() {
        let (mut app, _) = with_diff();
        let _ = app.on_key(key(']'));
        assert_eq!(cursor(&app), 9);
        let _ = app.on_key(key(']'));
        assert_eq!(cursor(&app), 9);
        let _ = app.on_key(key('['));
        assert_eq!(cursor(&app), 0);
        let _ = app.on_key(key('}'));
        assert_eq!(cursor(&app), 3);
        let _ = app.on_key(key('}'));
        assert_eq!(cursor(&app), 6);
        let _ = app.on_key(key('}'));
        assert_eq!(cursor(&app), 12);
        let _ = app.on_key(key('{'));
        assert_eq!(cursor(&app), 6);
    }

    #[test]
    fn s_cycles_the_scope_and_asks_for_that_diff() {
        let (mut app, a) = with_diff();
        for scope in [DiffScope::Uncommitted, DiffScope::Committed, DiffScope::All] {
            assert_eq!(
                app.on_key(key('s')),
                vec![Effect::Send(Request::GitDiff { id: a.id, scope })]
            );
            let view = app.diff().expect("diff");
            assert_eq!((view.scope, view.lines.as_ref()), (scope, None));
            apply_diff(&mut app, a.id, Ok(DIFF.into()));
        }
    }

    #[test]
    fn the_diff_counts_its_files() {
        let (app, _) = with_diff();
        assert_eq!(app.diff().map(DiffView::files), Some(2));
    }
}
