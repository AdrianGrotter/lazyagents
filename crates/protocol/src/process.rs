//! The process table, read natively: macOS through libproc, Linux from `/proc`.

use std::fmt;
use std::str::FromStr;

/// When a process started, precise enough that a recycled pid gets another value: the
/// microseconds since the Unix epoch on macOS, the clock ticks since boot on Linux.
/// Neither moves when the system clock or time zone changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Start(pub u64);

impl fmt::Display for Start {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Start {
    type Err = std::num::ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub start: Start,
}

/// Every process this user may inspect, which on macOS leaves out other users'
/// processes; `None` when the table cannot be read.
pub fn all() -> Option<Vec<Process>> {
    sys::all()
}

/// `None` when `pid` is not running or belongs to a process this user may not inspect.
pub fn start(pid: u32) -> Option<Start> {
    sys::process(pid).map(|process| process.start)
}

/// The whole seconds since the Unix epoch at which `pid` started, counted as
/// `ps -o lstart=` counts them.
pub fn started_at(pid: u32) -> Option<u64> {
    sys::started_at(pid)
}

#[cfg(target_os = "macos")]
mod sys {
    use libproc::bsd_info::BSDInfo;
    use libproc::proc_pid::pidinfo;
    use libproc::processes::{ProcFilter, pids_by_type};

    use super::{Process, Start};

    fn info(pid: u32) -> Option<BSDInfo> {
        pidinfo::<BSDInfo>(i32::try_from(pid).ok()?, 0).ok()
    }

    pub fn all() -> Option<Vec<Process>> {
        let pids = pids_by_type(ProcFilter::All).ok()?;
        Some(pids.into_iter().filter_map(process).collect())
    }

    pub fn process(pid: u32) -> Option<Process> {
        let info = info(pid)?;
        Some(Process {
            pid,
            ppid: info.pbi_ppid,
            start: Start(
                info.pbi_start_tvsec
                    .saturating_mul(1_000_000)
                    .saturating_add(info.pbi_start_tvusec),
            ),
        })
    }

    pub fn started_at(pid: u32) -> Option<u64> {
        info(pid).map(|info| info.pbi_start_tvsec)
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use super::{Process, Start, linux};

    pub fn all() -> Option<Vec<Process>> {
        let entries = std::fs::read_dir("/proc").ok()?;
        Some(
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .filter_map(process)
                .collect(),
        )
    }

    pub fn process(pid: u32) -> Option<Process> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (ppid, ticks) = linux::parse_stat(&stat)?;
        Some(Process {
            pid,
            ppid,
            start: Start(ticks),
        })
    }

    pub fn started_at(pid: u32) -> Option<u64> {
        let ticks = process(pid)?.start.0;
        let boot = linux::parse_btime(&std::fs::read_to_string("/proc/stat").ok()?)?;
        let per_second = rustix::param::clock_ticks_per_second();
        (per_second > 0).then(|| boot + ticks / per_second)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod sys {
    use super::Process;

    pub fn all() -> Option<Vec<Process>> {
        None
    }

    pub fn process(_pid: u32) -> Option<Process> {
        None
    }

    pub fn started_at(_pid: u32) -> Option<u64> {
        None
    }
}

#[cfg(any(target_os = "linux", test))]
mod linux {
    /// The parent pid and start time in clock ticks since boot from a
    /// `/proc/<pid>/stat` line. The command name sits in parentheses and may itself
    /// contain spaces and parentheses, so the fields are counted after its last `)`.
    pub fn parse_stat(line: &str) -> Option<(u32, u64)> {
        let (_, rest) = line.rsplit_once(')')?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
    }

    /// The boot time in seconds since the Unix epoch from `/proc/stat`.
    pub fn parse_btime(stat: &str) -> Option<u64> {
        stat.lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.trim().parse().ok())
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::Duration;

    use super::*;

    struct Sleeper(Child);

    impl Sleeper {
        fn spawn() -> Self {
            Self(
                Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .expect("spawn sleep"),
            )
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_table_lists_this_process_with_its_parent_and_start() {
        let pid = std::process::id();
        let table = all().expect("a process table");
        let ours = table.iter().find(|p| p.pid == pid).expect("our row");
        assert_eq!(ours.ppid, std::os::unix::process::parent_id());
        assert_eq!(Some(ours.start), start(pid));
    }

    #[test]
    fn the_table_lists_a_child_under_this_process() {
        let child = Sleeper::spawn();
        let table = all().expect("a process table");
        let row = table
            .iter()
            .find(|p| p.pid == child.0.id())
            .expect("child row");
        assert_eq!(row.ppid, std::process::id());
    }

    #[test]
    fn processes_started_apart_have_different_starts_and_keep_them() {
        let first = Sleeper::spawn();
        std::thread::sleep(Duration::from_millis(50));
        let second = Sleeper::spawn();
        let a = start(first.0.id()).expect("first start");
        let b = start(second.0.id()).expect("second start");
        assert!(a < b, "{a} then {b}");
        assert_eq!(start(first.0.id()), Some(a));
    }

    #[test]
    fn a_missing_pid_has_no_start() {
        let mut child = Command::new("true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        assert_eq!(start(pid), None);
        assert_eq!(started_at(pid), None);
    }

    #[test]
    fn started_at_is_the_start_in_whole_seconds_since_the_epoch() {
        let child = Sleeper::spawn();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs();
        let at = started_at(child.0.id()).expect("started_at");
        assert!(now.abs_diff(at) <= 2, "started {at}, now {now}");
    }

    #[test]
    fn starts_round_trip_through_text() {
        let start = Start(1_792_058_400_123_456);
        assert_eq!(start.to_string().parse(), Ok(start));
        assert!("Thu Oct 15 10:00:00 2026".parse::<Start>().is_err());
    }

    #[test]
    fn a_linux_stat_line_gives_the_parent_and_start_ticks() {
        let line = "4242 (a (weird) name) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 \
                    20 0 1 0 987654 1234567 89 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 \
                    17 3 0 0 0 0 0\n";
        assert_eq!(linux::parse_stat(line), Some((17, 987_654)));
        assert_eq!(linux::parse_stat("4242 (cut) S 17"), None);
        assert_eq!(linux::parse_stat(""), None);
    }

    #[test]
    fn the_linux_boot_time_comes_from_btime() {
        let stat = "cpu  1 2 3\nintr 5\nctxt 6\nbtime 1792000000\nprocesses 7\n";
        assert_eq!(linux::parse_btime(stat), Some(1_792_000_000));
        assert_eq!(linux::parse_btime("cpu 1\n"), None);
    }
}
