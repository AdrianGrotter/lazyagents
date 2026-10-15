use std::collections::HashMap;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty::{self, Options, Shell};

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

#[derive(Clone)]
pub struct Proxy(mpsc::Sender<Event>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

pub struct Session {
    term: Arc<FairMutex<Term<Proxy>>>,
    sender: EventLoopSender,
    events: mpsc::Receiver<Event>,
    size: PaneSize,
    exit_status: Option<ExitStatus>,
}

impl Session {
    pub fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        mut env: HashMap<String, String>,
        size: PaneSize,
    ) -> anyhow::Result<Self> {
        env.entry("TERM".into())
            .or_insert_with(|| "xterm-256color".into());
        env.entry("COLORTERM".into())
            .or_insert_with(|| "truecolor".into());
        let options = Options {
            shell: Some(Shell::new(program.to_string(), args.to_vec())),
            working_directory: Some(cwd.to_path_buf()),
            drain_on_exit: true,
            env,
        };
        let pty = tty::new(&options, size.window_size(), 0)
            .map_err(|e| anyhow::anyhow!("failed to start `{program}`: {e}"))?;

        let (tx, events) = mpsc::channel();
        let proxy = Proxy(tx);
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
            events,
            size,
            exit_status: None,
        })
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

    pub fn size(&self) -> PaneSize {
        self.size
    }

    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Wakeup => changed = true,
                Event::PtyWrite(text) => self.write(text.into_bytes()),
                Event::TextAreaSizeRequest(format) => {
                    self.write(format(self.size.window_size()).into_bytes());
                }
                Event::ChildExit(status) => {
                    self.exit_status = Some(status);
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status
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

    pub fn term(&self) -> &Arc<FairMutex<Term<Proxy>>> {
        &self.term
    }

    pub fn screen_text(&self) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        let lines: Vec<String> = (0..grid.screen_lines())
            .map(|line| {
                let row = &grid[Line(line as i32)];
                let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
                text.trim_end().to_string()
            })
            .collect();
        lines.join("\n").trim_end().to_string()
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pump();
            if self.exit_status.is_some() {
                std::thread::sleep(Duration::from_millis(50));
                self.pump();
                return self.exit_status;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Session {
        Session::spawn(
            "sh",
            &["-c".to_string(), script.to_string()],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .expect("spawns")
    }

    #[test]
    fn output_reaches_the_screen() {
        let mut s = sh("printf 'hello from pty'");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(
            s.screen_text().contains("hello from pty"),
            "{}",
            s.screen_text()
        );
    }

    #[test]
    fn exit_code_is_reported() {
        let mut s = sh("exit 3");
        let status = s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert_eq!(status.code(), Some(3));
    }

    #[test]
    fn input_reaches_the_child() {
        let mut s = sh("read -r line; printf 'got:%s' \"$line\"");
        s.write(b"ping\r".to_vec());
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("got:ping"), "{}", s.screen_text());
    }

    #[test]
    fn terminal_replies_reach_the_child() {
        let mut s = Session::spawn(
            "bash",
            &[
                "-c".to_string(),
                r#"printf '\033[6n'; IFS= read -rs -d R reply; printf '\ncursor:%s' "${reply:1}""#
                    .to_string(),
            ],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .expect("spawns");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(
            s.screen_text().contains("cursor:[1;1"),
            "{}",
            s.screen_text()
        );
    }

    #[test]
    fn env_is_passed_through() {
        let mut env = HashMap::new();
        env.insert("LAZYAGENTS_SPIKE_PROBE".to_string(), "probe-ok".to_string());
        let mut s = Session::spawn(
            "sh",
            &[
                "-c".to_string(),
                "printf '%s' \"$LAZYAGENTS_SPIKE_PROBE\"".to_string(),
            ],
            Path::new("."),
            env,
            PaneSize::new(40, 10),
        )
        .expect("spawns");
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("probe-ok"), "{}", s.screen_text());
    }

    #[test]
    fn missing_command_is_an_error() {
        let err = Session::spawn(
            "/nonexistent/lazyagents-spike-cmd",
            &[],
            Path::new("."),
            HashMap::new(),
            PaneSize::new(40, 10),
        )
        .err()
        .expect("spawn fails");
        assert!(err.to_string().contains("lazyagents-spike-cmd"), "{err}");
    }

    #[test]
    fn resize_to_zero_is_clamped() {
        let mut s = sh("sleep 5");
        s.resize(PaneSize::new(0, 0));
        assert_eq!(s.size(), PaneSize { cols: 2, rows: 2 });
        s.pump();
    }

    #[test]
    fn resize_reaches_the_child() {
        let mut s = sh("sleep 0.3; stty size");
        s.resize(PaneSize::new(33, 7));
        s.wait_for_exit(Duration::from_secs(5)).expect("exits");
        assert!(s.screen_text().contains("7 33"), "{}", s.screen_text());
    }
}
