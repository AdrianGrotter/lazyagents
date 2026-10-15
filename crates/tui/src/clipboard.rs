use std::ffi::OsStr;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

const NATIVE: [&str; 2] = ["pbcopy", "wl-copy"];

pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}

/// Writes OSC 52 to `out`, the terminal, and also pipes the text to `pbcopy` or `wl-copy`
/// when one is on `PATH`. Errors are ignored and nothing blocks: the copier runs on its
/// own thread, which waits for it so it is reaped.
pub fn copy(out: &mut impl Write, text: &str) {
    let _ = out.write_all(osc52(text).as_bytes());
    let _ = out.flush();
    if let Some(program) = std::env::var_os("PATH").and_then(|path| native_copier(&path)) {
        let _ = pipe_to(Command::new(program), text.to_string());
    }
}

fn native_copier(path: &OsStr) -> Option<PathBuf> {
    NATIVE.iter().find_map(|name| {
        std::env::split_paths(path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// Closing stdin before waiting is what lets the copier see the end of the text and exit.
fn pipe_to(mut command: Command, text: String) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let spawned = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            return;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(osc52("a世\nb"), "\x1b]52;c;YeS4lgpi\x07");
    }

    #[test]
    fn piped_text_reaches_the_program_and_it_is_reaped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("copied");
        let mut command = Command::new("sh");
        command.args(["-c", "cat > \"$1\"", "sh"]).arg(&path);
        pipe_to(command, "copied text".into())
            .join()
            .expect("copy thread");
        assert_eq!(
            std::fs::read_to_string(&path).expect("copied file"),
            "copied text"
        );
    }

    #[test]
    fn a_missing_program_is_ignored() {
        pipe_to(Command::new("/nonexistent/copier"), "x".into())
            .join()
            .expect("copy thread");
    }

    #[test]
    fn native_copier_is_found_on_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("wl-copy"), "").expect("fake copier");
        let path = std::env::join_paths([empty.path(), dir.path()]).expect("path");
        assert_eq!(native_copier(&path), Some(dir.path().join("wl-copy")));
        assert_eq!(native_copier(empty.path().as_os_str()), None);
    }
}
