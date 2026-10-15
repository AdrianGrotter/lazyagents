use std::collections::HashMap;
use std::io::stdout;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use spike::session::{PaneSize, Session};
use spike::{hooks, input, render};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Pane,
}

struct App {
    session: Session,
    label: String,
    focus: Focus,
    hook_log: Option<PathBuf>,
    status: &'static str,
    keyboard_enhanced: bool,
    pane: Rect,
    quit: bool,
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        args.push("claude".to_string());
    }
    let program = args.remove(0);

    let scratch = std::env::temp_dir().join(format!("lazyagents-spike-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).context("creating spike temp dir")?;
    let is_claude = Path::new(&program)
        .file_name()
        .is_some_and(|name| name == "claude");
    let hook_log = if is_claude {
        let log = scratch.join("hooks.log");
        let settings = scratch.join("hooks.json");
        std::fs::write(&settings, hooks::settings_json(&log).to_string())?;
        args.push("--settings".to_string());
        args.push(settings.display().to_string());
        Some(log)
    } else {
        None
    };

    let (cols, rows) = terminal::size()?;
    let pane = pane_area(Rect::new(0, 0, cols, rows));
    let session = Session::spawn(
        &program,
        &args,
        &std::env::current_dir()?,
        HashMap::new(),
        PaneSize::new(pane.width, pane.height),
    )?;

    let mut terminal = ratatui::init();
    let keyboard_enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    if keyboard_enhanced {
        execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let mut app = App {
        session,
        label: std::iter::once(program)
            .chain(args)
            .collect::<Vec<_>>()
            .join(" "),
        focus: Focus::Pane,
        hook_log,
        status: "starting",
        keyboard_enhanced,
        pane,
        quit: false,
    };
    let result = run(&mut terminal, &mut app);

    if keyboard_enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn pane_area(screen: Rect) -> Rect {
    let [_, main] = Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(screen);
    Block::bordered().inner(main)
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    let mut last_status_read = Instant::now() - Duration::from_secs(1);
    while !app.quit {
        app.session.pump();
        if let Some(log) = &app.hook_log
            && last_status_read.elapsed() > Duration::from_millis(250)
        {
            app.status = hooks::status_from_log(&std::fs::read_to_string(log).unwrap_or_default());
            last_status_read = Instant::now();
        }
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(Duration::from_millis(16))? {
            handle(app, event::read()?);
        }
    }
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(30), Constraint::Fill(1)]).areas(frame.area());

    let pane_border = if app.focus == Focus::Pane {
        Color::Green
    } else {
        Color::DarkGray
    };
    let sidebar_border = if app.focus == Focus::Sidebar {
        Color::Green
    } else {
        Color::DarkGray
    };

    let mode = app.session.mode();
    let flag = |on: bool| if on { "on" } else { "off" };
    let exit = match app.session.exit_status() {
        Some(status) => format!(
            "exited ({})",
            status.code().map_or("signal".into(), |c| c.to_string())
        ),
        None => "running".to_string(),
    };
    let focus_name = if app.focus == Focus::Pane {
        "PANE"
    } else {
        "SIDEBAR"
    };
    let lines = vec![
        Line::from(app.label.clone().bold()),
        Line::from(format!("process: {exit}")),
        Line::from(format!(
            "status: {}",
            if app.hook_log.is_some() {
                app.status
            } else {
                "n/a (not claude)"
            }
        )),
        Line::from(""),
        Line::from(format!("mode: {focus_name}")),
        Line::from(format!("kbd enhancement: {}", flag(app.keyboard_enhanced))),
        Line::from(format!(
            "app cursor: {}",
            flag(mode.contains(alacritty_terminal::term::TermMode::APP_CURSOR))
        )),
        Line::from(format!(
            "bracketed paste: {}",
            flag(mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE))
        )),
        Line::from(format!(
            "mouse report: {}",
            flag(mode.intersects(alacritty_terminal::term::TermMode::MOUSE_MODE))
        )),
        Line::from(format!(
            "alt screen: {}",
            flag(mode.contains(alacritty_terminal::term::TermMode::ALT_SCREEN))
        )),
        Line::from(""),
        Line::from("enter  focus pane".dark_gray()),
        Line::from("ctrl-] back to sidebar".dark_gray()),
        Line::from("q      quit (sidebar)".dark_gray()),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(" lazyagents spike ")
                .border_style(Style::new().fg(sidebar_border)),
        ),
        sidebar,
    );

    let block = Block::bordered()
        .title(" agent ")
        .border_style(Style::new().fg(pane_border));
    let inner = block.inner(main);
    frame.render_widget(block, main);
    if inner != app.pane {
        app.pane = inner;
        app.session.resize(PaneSize::new(inner.width, inner.height));
    }
    let cursor = render::render_term(&app.session.term().lock(), inner, frame.buffer_mut());
    if app.focus == Focus::Pane
        && let Some(position) = cursor
    {
        frame.set_cursor_position(position);
    }
}

fn handle(app: &mut App, event: Event) {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => match app.focus {
            Focus::Sidebar => match key.code {
                KeyCode::Enter => app.focus = Focus::Pane,
                KeyCode::Char('q') => app.quit = true,
                _ => {}
            },
            Focus::Pane if input::is_detach(&key) => app.focus = Focus::Sidebar,
            Focus::Pane => {
                if app.session.exit_status().is_none()
                    && let Some(bytes) = input::encode_key(key, app.session.mode())
                {
                    app.session.scroll_to_bottom();
                    app.session.write(bytes);
                }
            }
        },
        Event::Paste(text) if app.focus == Focus::Pane => {
            app.session.scroll_to_bottom();
            app.session
                .write(input::encode_paste(&text, app.session.mode()));
        }
        Event::Mouse(mouse) if app.pane.contains((mouse.column, mouse.row).into()) => {
            let up = match mouse.kind {
                MouseEventKind::ScrollUp => true,
                MouseEventKind::ScrollDown => false,
                _ => return,
            };
            let (col, row) = (mouse.column - app.pane.x, mouse.row - app.pane.y);
            match input::encode_wheel(up, col, row, app.session.mode()) {
                Some(bytes) => app.session.write(bytes),
                None => app.session.scroll(if up { 3 } else { -3 }),
            }
        }
        _ => {}
    }
}
