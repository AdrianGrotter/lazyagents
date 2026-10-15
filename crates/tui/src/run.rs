use std::io::{IsTerminal, stdout};
use std::mem::ManuallyDrop;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use client::Client;
use protocol::{Request, StatePaths};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEvent, KeyEventKind, KeyboardEnhancementFlags, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::{Position, Rect};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::MissedTickBehavior;

use crate::app::{App, Effect};
use crate::connection::{self, Incoming};
use crate::ui::{self, Areas};

enum Term {
    Key(KeyEvent),
    Paste(String),
    Wheel { up: bool, col: u16, row: u16 },
    Resize,
    Gone,
}

struct TerminalGuard {
    /// Never dropped: `Terminal`'s own `Drop` prints to stderr when it cannot show the
    /// cursor, and printing to a vanished tty panics.
    terminal: ManuallyDrop<DefaultTerminal>,
    enhanced: bool,
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        let previous = std::panic::take_hook();
        let terminal = ratatui::try_init();
        let enhanced =
            terminal.is_ok() && terminal::supports_keyboard_enhancement().unwrap_or(false);
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(enhanced);
            previous(info);
        }));
        let terminal = terminal.inspect_err(|_| restore_terminal(false))?;
        let guard = TerminalGuard {
            terminal: ManuallyDrop::new(terminal),
            enhanced,
        };
        execute!(stdout(), EnableBracketedPaste, EnableMouseCapture)?;
        if enhanced {
            execute!(
                stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        restore_terminal(self.enhanced);
    }
}

/// Ignores every error: the terminal may already be gone, and nothing here may print.
fn restore_terminal(enhanced: bool) {
    if enhanced {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    let _ = ratatui::try_restore();
}

pub async fn run(paths: StatePaths, exe: PathBuf) -> anyhow::Result<()> {
    if !stdout().is_terminal() {
        anyhow::bail!(
            "lazyagents needs an interactive terminal; run lazyagents --help for headless commands"
        );
    }
    let client = Client::connect_or_spawn(&paths, &exe).await?;
    let (requests, mut incoming) = connection::spawn(client);
    let _ = requests.send(Request::ListProjects);
    let _ = requests.send(Request::ListAgents);

    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut frame_counter: u64 = 0;

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut app = App::new(cwd, shell, home);

    let mut guard = TerminalGuard::enter()?;
    let (input_tx, mut input) = mpsc::unbounded_channel();
    std::thread::spawn(move || read_terminal(&input_tx));

    let mut layout = ui::areas(Rect::default(), 0);
    let mut redraw = true;
    let mut message_shown = false;
    loop {
        if redraw {
            let size = guard.terminal.size()?;
            layout = ui::areas(Rect::new(0, 0, size.width, size.height), app.projects.len());
            send_all(
                &requests,
                app.set_pane_size(layout.pane_inner.width, layout.pane_inner.height),
            );
            let now = Instant::now();
            guard
                .terminal
                .draw(|frame| ui::draw(frame, &app, &layout, now, frame_counter))?;
            message_shown = app.current_message(now).is_some();
        }

        let effects = tokio::select! {
            event = input.recv() => match event {
                Some(Term::Key(key)) => app.on_key(key),
                Some(Term::Paste(text)) => app.on_paste(text),
                Some(Term::Wheel { up, col, row }) => wheel(&mut app, &layout, up, col, row),
                Some(Term::Resize) => Vec::new(),
                Some(Term::Gone) | None => return Ok(()),
            },
            message = incoming.recv() => match message {
                Some(Incoming::Server(message)) => app.apply_server(message),
                Some(Incoming::Response(response)) => app.apply_response(response),
                Some(Incoming::Failed(error)) => {
                    app.request_failed(error);
                    Vec::new()
                }
                Some(Incoming::Closed) | None => anyhow::bail!("the daemon stopped"),
            },
            _ = hangup.recv() => return Ok(()),
            _ = terminate.recv() => return Ok(()),
            _ = interrupt.recv() => return Ok(()),
            _ = tick.tick() => {
                frame_counter = frame_counter.wrapping_add(1);
                redraw = app.animating() || message_shown;
                continue;
            }
        };
        redraw = true;
        for effect in effects {
            match effect {
                Effect::Send(request) => {
                    let _ = requests.send(request);
                }
                Effect::Quit => return Ok(()),
                Effect::StopDaemonAndQuit => {
                    let _ = requests.send(Request::Shutdown);
                    let _ = tokio::time::timeout(Duration::from_secs(3), async {
                        while let Some(message) = incoming.recv().await {
                            if matches!(message, Incoming::Closed) {
                                break;
                            }
                        }
                    })
                    .await;
                    return Ok(());
                }
            }
        }
    }
}

fn send_all(requests: &UnboundedSender<Request>, effects: Vec<Effect>) {
    for effect in effects {
        if let Effect::Send(request) = effect {
            let _ = requests.send(request);
        }
    }
}

fn wheel(app: &mut App, layout: &Areas, up: bool, col: u16, row: u16) -> Vec<Effect> {
    let pane = layout.pane_inner;
    if !pane.contains(Position::new(col, row)) {
        return Vec::new();
    }
    app.on_wheel(up, col - pane.x, row - pane.y)
}

/// Blocks on terminal input; a read error means the terminal is gone.
fn read_terminal(tx: &UnboundedSender<Term>) {
    loop {
        let term = match event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => Term::Key(key),
            Ok(Event::Paste(text)) => Term::Paste(text),
            Ok(Event::Mouse(mouse)) => match mouse.kind {
                MouseEventKind::ScrollUp => Term::Wheel {
                    up: true,
                    col: mouse.column,
                    row: mouse.row,
                },
                MouseEventKind::ScrollDown => Term::Wheel {
                    up: false,
                    col: mouse.column,
                    row: mouse.row,
                },
                _ => continue,
            },
            Ok(Event::Resize(..)) => Term::Resize,
            Ok(_) => continue,
            Err(_) => {
                let _ = tx.send(Term::Gone);
                return;
            }
        };
        if tx.send(term).is_err() {
            return;
        }
    }
}
