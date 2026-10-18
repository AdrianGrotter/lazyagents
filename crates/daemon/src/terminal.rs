use std::sync::Arc;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};
use protocol::{AgentId, Cell, Color, Screen, Style};
use tokio::sync::mpsc::UnboundedSender;

use crate::spawn::SpawnSpec;
use crate::status::exit_code;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSize {
    pub cols: u16,
    pub rows: u16,
}

impl PaneSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(2),
            rows: rows.max(2),
        }
    }

    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: 1,
            cell_height: 1,
        }
    }
}

impl Dimensions for PaneSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Debug)]
pub struct TerminalEvent {
    pub agent: AgentId,
    pub generation: u64,
    pub event: Event,
}

#[derive(Clone)]
pub struct Proxy {
    agent: AgentId,
    generation: u64,
    tx: UnboundedSender<TerminalEvent>,
}

impl Proxy {
    pub fn new(agent: AgentId, generation: u64, tx: UnboundedSender<TerminalEvent>) -> Self {
        Self {
            agent,
            generation,
            tx,
        }
    }
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send(TerminalEvent {
            agent: self.agent,
            generation: self.generation,
            event,
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Output,
    Exited(i32),
    Title(String),
    Bell,
    Nothing,
}

/// Claude Code titles its terminal with a status glyph and the session's title, or
/// `Claude Code` before the session has one.
pub fn session_title(raw: &str) -> Option<String> {
    let title = raw
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    (!title.is_empty() && title != "Claude Code").then(|| title.to_string())
}

pub struct Terminal {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    size: PaneSize,
    pid: u32,
}

impl Terminal {
    pub fn spawn(spec: &SpawnSpec, size: PaneSize, proxy: Proxy) -> anyhow::Result<Self> {
        let program = &spec.program;
        let mut env = spec.env.clone();
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        let options = Options {
            shell: Some(Shell::new(program.clone(), spec.args.clone())),
            working_directory: Some(spec.cwd.clone()),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size.window_size(), 0)
            .map_err(|e| anyhow::anyhow!("failed to start `{program}`: {e}"))?;
        let pid = pty.child().id();

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            proxy.clone(),
        )));
        let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, true, false)?;
        let sender = event_loop.channel();
        event_loop.spawn();

        Ok(Self {
            term,
            sender,
            size,
            pid,
        })
    }

    pub fn handle(&mut self, event: Event) -> Outcome {
        match event {
            Event::Wakeup => Outcome::Output,
            Event::PtyWrite(text) => {
                self.write(text.into_bytes());
                Outcome::Nothing
            }
            Event::TextAreaSizeRequest(format) => {
                self.write(format(self.size.window_size()).into_bytes());
                Outcome::Nothing
            }
            Event::ChildExit(status) => Outcome::Exited(exit_code(status)),
            Event::Title(title) => Outcome::Title(title),
            Event::Bell => Outcome::Bell,
            _ => Outcome::Nothing,
        }
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn resize(&mut self, size: PaneSize) {
        let size = PaneSize::new(size.cols, size.rows);
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    /// The agent's process, which leads its own session and process group.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn size(&self) -> PaneSize {
        self.size
    }

    pub fn mode(&self) -> TermMode {
        *self.term.lock().mode()
    }

    pub fn scroll(&self, lines: i32) {
        self.term.lock().scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    pub fn screen(&self) -> Screen {
        screen_of(&self.term.lock())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

pub fn screen_of<T: EventListener>(term: &Term<T>) -> Screen {
    let mut screen = Screen::blank(term.columns() as u16, term.screen_lines() as u16);
    let content = term.renderable_content();
    let offset = content.display_offset as i32;
    for indexed in content.display_iter {
        let Ok(row) = usize::try_from(indexed.point.line.0 + offset) else {
            continue;
        };
        if let Some(target) = screen
            .lines
            .get_mut(row)
            .and_then(|line| line.get_mut(indexed.point.column.0))
        {
            *target = convert(indexed.cell);
        }
    }
    let cursor = content.cursor;
    let visible =
        content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden;
    screen.cursor = visible
        .then(|| {
            let row = u16::try_from(cursor.point.line.0 + offset).ok()?;
            let col = u16::try_from(cursor.point.column.0).ok()?;
            (row < screen.rows && col < screen.cols).then_some((col, row))
        })
        .flatten();
    screen
}

fn convert(cell: &alacritty_terminal::term::cell::Cell) -> Cell {
    let text = if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        String::new()
    } else if cell.flags.contains(Flags::HIDDEN) {
        " ".to_string()
    } else {
        let mut text = cell.c.to_string();
        if let Some(zero_width) = cell.zerowidth() {
            text.extend(zero_width);
        }
        text
    };
    Cell {
        text,
        fg: map_color(cell.fg),
        bg: map_color(cell.bg),
        style: Style {
            bold: cell.flags.contains(Flags::BOLD),
            dim: cell.flags.contains(Flags::DIM),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            reverse: cell.flags.contains(Flags::INVERSE),
            strikethrough: cell.flags.contains(Flags::STRIKEOUT),
        },
    }
}

fn map_color(color: AnsiColor) -> Color {
    match color {
        AnsiColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => Color::Indexed(i),
        AnsiColor::Named(named) => {
            let index = named as usize;
            let dim_black = NamedColor::DimBlack as usize;
            match index {
                0..=15 => Color::Indexed(index as u8),
                i if (dim_black..dim_black + 8).contains(&i) => {
                    Color::Indexed((i - dim_black) as u8)
                }
                _ => Color::Default,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

    use super::*;

    fn term_with(bytes: &[u8]) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &PaneSize::new(20, 4), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, bytes);
        term
    }

    #[test]
    fn text_lands_in_cells() {
        let screen = screen_of(&term_with(b"hi"));
        assert_eq!((screen.cols, screen.rows), (20, 4));
        assert_eq!(screen.lines[0][0].text, "h");
        assert_eq!(screen.lines[0][1].text, "i");
        assert_eq!(screen.lines[0][2].text, " ");
    }

    #[test]
    fn sgr_colors_and_style_map() {
        let screen = screen_of(&term_with(b"\x1b[1;31mR\x1b[0m\x1b[38;2;1;2;3;48;5;200mX"));
        let r = &screen.lines[0][0];
        assert_eq!(r.fg, Color::Indexed(1));
        assert!(r.style.bold);
        let x = &screen.lines[0][1];
        assert_eq!(x.fg, Color::Rgb(1, 2, 3));
        assert_eq!(x.bg, Color::Indexed(200));
        assert!(!x.style.bold);
    }

    #[test]
    fn default_colors_are_default() {
        let screen = screen_of(&term_with(b"d"));
        assert_eq!(screen.lines[0][0].fg, Color::Default);
        assert_eq!(screen.lines[0][0].bg, Color::Default);
    }

    #[test]
    fn wide_char_spacer_has_empty_text() {
        let screen = screen_of(&term_with("a日b".as_bytes()));
        let texts: Vec<&str> = screen.lines[0][..4]
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(texts, ["a", "日", "", "b"]);
    }

    #[test]
    fn combining_marks_join_their_base() {
        let screen = screen_of(&term_with("e\u{301}x".as_bytes()));
        assert_eq!(screen.lines[0][0].text, "e\u{301}");
        assert_eq!(screen.lines[0][1].text, "x");
    }

    #[test]
    fn cursor_is_reported_as_col_row() {
        let screen = screen_of(&term_with(b"abc\r\nde"));
        assert_eq!(screen.cursor, Some((2, 1)));
    }

    #[test]
    fn hidden_cursor_is_none() {
        let screen = screen_of(&term_with(b"abc\x1b[?25l"));
        assert_eq!(screen.cursor, None);
    }

    #[test]
    fn scrolled_back_view_shows_history() {
        let lines: Vec<String> = (0..10).map(|i| format!("line{i}")).collect();
        let mut term = term_with(lines.join("\r\n").as_bytes());
        term.scroll_display(Scroll::Delta(3));
        let screen = screen_of(&term);
        assert!(screen.text().starts_with("line3"), "{}", screen.text());
        assert_eq!(screen.cursor, None);
    }

    fn spec(program: &str, args: &[&str]) -> SpawnSpec {
        SpawnSpec {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: PathBuf::from("."),
            env: HashMap::new(),
        }
    }

    fn start(spec: &SpawnSpec) -> (Terminal, UnboundedReceiver<TerminalEvent>) {
        let (tx, rx) = unbounded_channel();
        let proxy = Proxy::new(AgentId::random(), 0, tx);
        let terminal = Terminal::spawn(spec, PaneSize::new(40, 10), proxy).expect("spawns");
        (terminal, rx)
    }

    fn sh(script: &str) -> (Terminal, UnboundedReceiver<TerminalEvent>) {
        start(&spec("sh", &["-c", script]))
    }

    fn drive(
        terminal: &mut Terminal,
        rx: &mut UnboundedReceiver<TerminalEvent>,
        until: impl Fn(&Terminal, Option<i32>) -> bool,
        timeout: Duration,
    ) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        let mut exit = None;
        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if let Outcome::Exited(code) = terminal.handle(event.event) {
                        exit = Some(code);
                    }
                }
                Err(_) => {
                    if until(terminal, exit) || Instant::now() >= deadline {
                        return exit;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn exited(_: &Terminal, code: Option<i32>) -> bool {
        code.is_some()
    }

    const TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn session_title_drops_claudes_status_glyph() {
        assert_eq!(
            session_title("\u{2733} Lazyagents TUI project setup").as_deref(),
            Some("Lazyagents TUI project setup")
        );
        assert_eq!(
            session_title("\u{2810} Fixing tests").as_deref(),
            Some("Fixing tests")
        );
        assert_eq!(session_title("Plain title").as_deref(), Some("Plain title"));
        assert_eq!(session_title("2 bugs left").as_deref(), Some("2 bugs left"));
    }

    #[test]
    fn default_and_blank_titles_name_no_session() {
        for raw in ["\u{2733} Claude Code", "Claude Code", "", "  ", "\u{2733}"] {
            assert_eq!(session_title(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_title_sequence_is_reported() {
        let (mut t, mut rx) = sh("printf '\\033]0;hello title\\007'; sleep 1");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(event) = rx.try_recv()
                && let Outcome::Title(title) = t.handle(event.event)
            {
                assert_eq!(title, "hello title");
                return;
            }
            assert!(Instant::now() < deadline, "no title event");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn output_reaches_the_screen() {
        let (mut t, mut rx) = sh("printf 'hello from pty'");
        drive(&mut t, &mut rx, exited, TIMEOUT).expect("exits");
        let text = t.screen().text();
        assert!(text.contains("hello from pty"), "{text}");
    }

    #[test]
    fn exit_code_is_reported() {
        let (mut t, mut rx) = sh("exit 3");
        assert_eq!(drive(&mut t, &mut rx, exited, TIMEOUT), Some(3));
    }

    #[test]
    fn input_reaches_the_child() {
        let (mut t, mut rx) = sh("read -r line; printf 'got:%s' \"$line\"");
        t.write(b"ping\r".to_vec());
        drive(&mut t, &mut rx, exited, TIMEOUT).expect("exits");
        let text = t.screen().text();
        assert!(text.contains("got:ping"), "{text}");
    }

    #[test]
    fn terminal_replies_reach_the_child() {
        let (mut t, mut rx) = start(&spec(
            "bash",
            &[
                "-c",
                r#"printf '\033[6n'; IFS= read -rs -d R reply; printf '\ncursor:%s' "${reply:1}""#,
            ],
        ));
        drive(&mut t, &mut rx, exited, TIMEOUT).expect("exits");
        let text = t.screen().text();
        assert!(text.contains("cursor:[1;1"), "{text}");
    }

    #[test]
    fn missing_command_is_an_error() {
        let (tx, _rx) = unbounded_channel();
        let proxy = Proxy::new(AgentId::random(), 0, tx);
        let err = Terminal::spawn(
            &spec("/nonexistent/lazyagents-daemon-missing-cmd", &[]),
            PaneSize::new(40, 10),
            proxy,
        )
        .err()
        .expect("spawn fails");
        assert!(
            err.to_string().contains("lazyagents-daemon-missing-cmd"),
            "{err}"
        );
    }

    #[test]
    fn resize_to_zero_is_clamped() {
        let (mut t, mut rx) = sh("sleep 5");
        t.resize(PaneSize::new(0, 0));
        assert_eq!(t.size(), PaneSize { cols: 2, rows: 2 });
        drive(&mut t, &mut rx, |_, _| true, Duration::ZERO);
    }

    #[test]
    fn resize_reaches_the_child() {
        let (mut t, mut rx) = sh("sleep 0.3; stty size");
        t.resize(PaneSize::new(33, 7));
        drive(&mut t, &mut rx, exited, TIMEOUT).expect("exits");
        let text = t.screen().text();
        assert!(text.contains("7 33"), "{text}");
    }

    #[test]
    fn events_carry_agent_and_generation() {
        let agent = AgentId::random();
        let (tx, mut rx) = unbounded_channel();
        let mut t = Terminal::spawn(
            &spec("sh", &["-c", "printf 'tagged'"]),
            PaneSize::new(40, 10),
            Proxy::new(agent, 7, tx),
        )
        .expect("spawns");
        let deadline = Instant::now() + TIMEOUT;
        let mut seen = 0;
        let mut exited = false;
        while !exited && Instant::now() < deadline {
            match rx.try_recv() {
                Ok(event) => {
                    assert_eq!(event.agent, agent);
                    assert_eq!(event.generation, 7);
                    seen += 1;
                    exited = matches!(t.handle(event.event), Outcome::Exited(_));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(exited, "child never exited");
        assert!(seen > 1, "only {seen} events");
    }

    #[test]
    fn dropping_terminal_stops_the_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > {}; exec sleep 30", pid_file.display());
        let (mut t, mut rx) = sh(&script);
        let pid = {
            let read_pid = || std::fs::read_to_string(&pid_file).unwrap_or_default();
            drive(
                &mut t,
                &mut rx,
                |_, _| !read_pid().trim().is_empty(),
                TIMEOUT,
            );
            read_pid().trim().to_string()
        };
        assert!(!pid.is_empty(), "child never wrote its pid");
        drop(t);
        let deadline = Instant::now() + TIMEOUT;
        let mut gone = false;
        while Instant::now() < deadline {
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid])
                .status()
                .map(|st| st.success())
                .unwrap_or(false);
            if !alive {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(gone, "child {pid} still alive after drop");
    }
}
