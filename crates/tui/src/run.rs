use std::io::{IsTerminal, stdout};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use client::{Client, VersionMismatch};
use protocol::{Request, SHUTDOWN_WAIT, StatePaths};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::{execute, terminal};
use ratatui::layout::Rect;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::MissedTickBehavior;

use crate::app::{App, Effect, MouseInput, MouseKind};
use crate::clipboard;
use crate::config;
use crate::connection::{self, Incoming};
use crate::known::{self, KnownProject};
use crate::ui::{self, Mismatch};

enum Term {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseInput),
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
    let (client, mut guard, mut input) = match Client::connect_or_spawn(&paths, &exe).await {
        Ok(client) => {
            let (guard, input) = open_terminal()?;
            (client, guard, input)
        }
        Err(error) => {
            let Some(mismatch) = error.downcast_ref::<VersionMismatch>() else {
                return Err(error);
            };
            let (mut guard, mut input) = open_terminal()?;
            let mut signals = Signals::new()?;
            match ask_restart(&mut guard, &mut input, mismatch, &mut signals).await? {
                Answer::Restart => {}
                Answer::Quit => return Err(error),
                Answer::Leave => return Ok(()),
            }
            let lock = paths.clone();
            let pid = tokio::task::spawn_blocking(move || client::daemon_pid(&lock)).await?;
            guard.terminal.draw(|frame| {
                ui::draw_mismatch(
                    frame,
                    &mismatch.daemon_version,
                    mismatch.protocol_version,
                    Mismatch::Stopping(pid),
                )
            })?;
            let stop = paths.clone();
            let mut stopping =
                tokio::task::spawn_blocking(move || client::stop_daemon(&stop, SHUTDOWN_WAIT));
            loop {
                tokio::select! {
                    stopped = &mut stopping => {
                        stopped?.with_context(|| {
                            format!(
                                "stopping daemon {} (protocol {})",
                                mismatch.daemon_version, mismatch.protocol_version
                            )
                        })?;
                        break;
                    }
                    event = input.recv() => match event {
                        Some(Term::Key(key)) if is_ctrl_c(&key) => return Ok(()),
                        Some(Term::Gone) | None => return Ok(()),
                        Some(_) => {}
                    },
                    _ = signals.hangup.recv() => return Ok(()),
                    _ = signals.terminate.recv() => return Ok(()),
                    _ = signals.interrupt.recv() => return Ok(()),
                }
            }
            let client = tokio::select! {
                client = Client::connect_or_spawn(&paths, &exe) => {
                    client.context("starting a new daemon")?
                }
                _ = signals.hangup.recv() => return Ok(()),
                _ = signals.terminate.recv() => return Ok(()),
                _ = signals.interrupt.recv() => return Ok(()),
            };
            while input.try_recv().is_ok() {}
            (client, guard, input)
        }
    };
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
    let config = config::load(config::default_path().as_deref());
    let mut app = App::new(cwd, shell, home, config);
    let known_file = paths.known_projects();
    match known::load(&known_file) {
        Ok(list) => app.known = list,
        Err(error) => app.fail(format!("{error:#}")),
    }

    let mut layout = ui::areas(Rect::default(), 0, 0);
    let mut redraw = true;
    let mut message_shown = false;
    loop {
        if redraw {
            let size = guard.terminal.size()?;
            layout = ui::layout(Rect::new(0, 0, size.width, size.height), &app);
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
                Some(Term::Mouse(mouse)) => app.on_mouse(mouse, &layout),
                Some(Term::Resize) => Vec::new(),
                Some(Term::Gone) | None => return Ok(()),
            },
            message = incoming.recv() => match message {
                Some(Incoming::Server(message)) => app.apply_server(message),
                Some(Incoming::Response(response)) => app.apply_response(response),
                Some(Incoming::GitStat { id, result }) => {
                    app.apply_git_stat(id, result);
                    Vec::new()
                }
                Some(Incoming::Diff { id, result }) => {
                    app.apply_diff(id, result);
                    Vec::new()
                }
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
                send_all(&requests, app.on_tick(Instant::now()));
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
                Effect::Copy(text) => clipboard::copy(guard.terminal.backend_mut(), &text),
                Effect::RememberProjects(added) => {
                    update_known(&mut app, &known_file, |list| known::remember(list, &added))
                }
                Effect::ForgetProject(path) => {
                    update_known(&mut app, &known_file, |list| known::forget(list, &path))
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

/// Takes the merged list back, so projects another TUI remembered show up here too.
fn update_known(app: &mut App, file: &Path, change: impl FnOnce(&mut Vec<KnownProject>) -> bool) {
    match known::update(file, change) {
        Ok(list) => app.known = list,
        Err(error) => app.fail(format!("{error:#}")),
    }
}

fn open_terminal() -> anyhow::Result<(TerminalGuard, UnboundedReceiver<Term>)> {
    let guard = TerminalGuard::enter()?;
    let (input_tx, input) = mpsc::unbounded_channel();
    std::thread::spawn(move || read_terminal(&input_tx));
    Ok((guard, input))
}

enum Answer {
    Restart,
    Quit,
    /// The terminal is gone, so there is no one to tell.
    Leave,
}

/// Kept from the mismatch prompt until a new daemon answers: once a handler is
/// installed the default action no longer applies, so the signals must stay watched.
struct Signals {
    hangup: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}

impl Signals {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            hangup: signal(SignalKind::hangup())?,
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }
}

async fn ask_restart(
    guard: &mut TerminalGuard,
    input: &mut UnboundedReceiver<Term>,
    mismatch: &VersionMismatch,
    signals: &mut Signals,
) -> anyhow::Result<Answer> {
    loop {
        guard.terminal.draw(|frame| {
            ui::draw_mismatch(
                frame,
                &mismatch.daemon_version,
                mismatch.protocol_version,
                Mismatch::Asking,
            )
        })?;
        tokio::select! {
            event = input.recv() => match event {
                Some(Term::Key(key)) => match key.code {
                    KeyCode::Char('y') => return Ok(Answer::Restart),
                    KeyCode::Char('n' | 'q') | KeyCode::Esc => return Ok(Answer::Quit),
                    _ if is_ctrl_c(&key) => return Ok(Answer::Quit),
                    _ => {}
                },
                Some(Term::Gone) | None => return Ok(Answer::Leave),
                Some(_) => {}
            },
            _ = signals.hangup.recv() => return Ok(Answer::Leave),
            _ = signals.terminate.recv() => return Ok(Answer::Quit),
            _ = signals.interrupt.recv() => return Ok(Answer::Quit),
        }
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn send_all(requests: &UnboundedSender<Request>, effects: Vec<Effect>) {
    for effect in effects {
        if let Effect::Send(request) = effect {
            let _ = requests.send(request);
        }
    }
}

/// Blocks on terminal input; a read error means the terminal is gone.
fn read_terminal(tx: &UnboundedSender<Term>) {
    loop {
        let term = match event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => Term::Key(key),
            Ok(Event::Paste(text)) => Term::Paste(text),
            Ok(Event::Mouse(mouse)) => {
                let kind = match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => MouseKind::Down,
                    MouseEventKind::Drag(MouseButton::Left) => MouseKind::Drag,
                    MouseEventKind::Up(MouseButton::Left) => MouseKind::Up,
                    MouseEventKind::ScrollUp => MouseKind::ScrollUp,
                    MouseEventKind::ScrollDown => MouseKind::ScrollDown,
                    _ => continue,
                };
                Term::Mouse(MouseInput {
                    kind,
                    col: mouse.column,
                    row: mouse.row,
                })
            }
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
