//! The contents of `daemon.lock`: the holder's pid and, when known, its start time.

use std::path::Path;
use std::process::Command;

/// Who wrote the lock file. `start` is the `ps -o lstart=` text of `pid`, or `None` in
/// the pid-only format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    pub start: Option<String>,
}

pub fn format(pid: u32, start: Option<&str>) -> String {
    match start {
        Some(start) => format!("{pid}\n{start}\n"),
        None => format!("{pid}\n"),
    }
}

pub fn parse(text: &str) -> Option<Holder> {
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let start = lines
        .next()
        .map(str::trim)
        .filter(|start| !start.is_empty())
        .map(String::from);
    Some(Holder { pid, start })
}

/// A `ps` command in the C locale and UTC, so every reader of a start time gets the
/// same text whatever its own locale or time zone. Debug builds run `$LAZYAGENTS_PS`
/// in its place when that is set.
pub fn ps() -> Command {
    let mut command = if cfg!(debug_assertions)
        && let Some(program) = std::env::var_os("LAZYAGENTS_PS")
    {
        Command::new(program)
    } else {
        Command::new(ps_program(Path::new("/bin/ps").exists()))
    };
    command.env("LC_ALL", "C").env("TZ", "UTC0");
    command
}

fn ps_program(bin_ps_exists: bool) -> &'static str {
    if bin_ps_exists { "/bin/ps" } else { "ps" }
}

/// The start time of `pid` as `ps -o lstart=` prints it in the C locale and UTC, so
/// readers in any locale or time zone get the same text; `None` when `ps` cannot tell.
pub fn process_start(pid: u32) -> Option<String> {
    let output = ps()
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let start = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!start.is_empty()).then_some(start)
}

/// True unless the holder recorded a start time that `pid` no longer has, as when the
/// pid was recycled after the holder died.
pub fn is_current(holder: &Holder) -> bool {
    holder
        .start
        .as_ref()
        .is_none_or(|start| process_start(holder.pid).as_ref() == Some(start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_a_start() {
        assert_eq!(
            parse(&format(42, Some("Thu Oct 16 09:00:00 2026"))),
            Some(Holder {
                pid: 42,
                start: Some("Thu Oct 16 09:00:00 2026".into())
            })
        );
        assert_eq!(
            parse("42"),
            Some(Holder {
                pid: 42,
                start: None
            })
        );
        assert_eq!(parse(""), None);
        assert_eq!(parse("x"), None);
    }

    #[test]
    fn the_current_process_is_current_and_a_wrong_start_is_not() {
        let pid = std::process::id();
        let start = process_start(pid).expect("ps reads our own start");
        assert!(is_current(&Holder {
            pid,
            start: Some(start)
        }));
        assert!(!is_current(&Holder {
            pid,
            start: Some("Mon Jan  1 00:00:00 1990".into())
        }));
        assert!(is_current(&Holder { pid, start: None }));
    }

    #[test]
    fn ps_program_falls_back_to_path_without_bin_ps() {
        assert_eq!(ps_program(true), "/bin/ps");
        assert_eq!(ps_program(false), "ps");
    }

    #[test]
    fn ps_runs_in_the_c_locale_and_utc() {
        let command = ps();
        let envs: Vec<_> = command.get_envs().collect();
        assert!(
            envs.contains(&("LC_ALL".as_ref(), Some("C".as_ref()))),
            "{envs:?}"
        );
        assert!(
            envs.contains(&("TZ".as_ref(), Some("UTC0".as_ref()))),
            "{envs:?}"
        );
    }

    #[test]
    fn the_start_reads_the_same_in_any_locale_and_time_zone() {
        let pid = std::process::id();
        let output = ps()
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let expected = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(process_start(pid), Some(expected));
    }
}
