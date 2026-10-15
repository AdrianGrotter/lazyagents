//! Terminates agent process trees, including descendants that left the agent's
//! process group or session.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

use rustix::process::{
    Pid, Signal, kill_process, kill_process_group, test_kill_process, test_kill_process_group,
};

const POLL: Duration = Duration::from_millis(25);
const KILL_WAIT: Duration = Duration::from_millis(500);

/// A process identified by its pid and start time, so a recycled pid is never mistaken
/// for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracked {
    pub pid: u32,
    pub start: String,
}

/// One reading of `ps`: `(pid, ppid, start)` for every process, where `start` is the
/// `lstart` text.
pub struct ProcessTable {
    rows: Vec<(u32, u32, String)>,
}

impl ProcessTable {
    /// `None` when `ps` cannot run, fails or lists nothing, so a bad reading is never
    /// taken to mean that every process is gone.
    pub fn read() -> Option<Self> {
        match Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,lstart="])
            .output()
        {
            Ok(output) => Self::parse(output.status.success(), &output.stdout),
            Err(e) => {
                tracing::warn!(error = %e, "running ps failed");
                None
            }
        }
    }

    fn parse(success: bool, stdout: &[u8]) -> Option<Self> {
        if !success {
            tracing::warn!("ps exited with an error");
            return None;
        }
        let rows: Vec<(u32, u32, String)> = String::from_utf8_lossy(stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim_start();
                let (pid, rest) = line.split_once(char::is_whitespace)?;
                let rest = rest.trim_start();
                let (ppid, start) = rest.split_once(char::is_whitespace)?;
                Some((
                    pid.parse().ok()?,
                    ppid.parse().ok()?,
                    start.trim().to_string(),
                ))
            })
            .collect();
        if rows.is_empty() {
            tracing::warn!("ps listed no processes");
            return None;
        }
        Some(Self { rows })
    }

    /// `previous` entries still running with their recorded start time, plus the
    /// current descendants of `root`.
    pub fn track(&self, root: u32, previous: &[Tracked]) -> Vec<Tracked> {
        let starts: HashMap<u32, &str> = self
            .rows
            .iter()
            .map(|(pid, _, start)| (*pid, start.as_str()))
            .collect();
        let mut tracked: Vec<Tracked> = previous
            .iter()
            .filter(|t| starts.get(&t.pid) == Some(&t.start.as_str()))
            .cloned()
            .collect();
        for pid in self.descendants(&[root]) {
            if let Some(start) = starts.get(&pid)
                && !tracked.iter().any(|t| t.pid == pid)
            {
                tracked.push(Tracked {
                    pid,
                    start: start.to_string(),
                });
            }
        }
        tracked
    }

    fn descendants(&self, roots: &[u32]) -> Vec<u32> {
        let links: Vec<(u32, u32)> = self
            .rows
            .iter()
            .map(|(pid, ppid, _)| (*pid, *ppid))
            .collect();
        roots
            .iter()
            .flat_map(|&root| descendants_in(&links, root))
            .collect()
    }

    fn running(&self, tracked: &[Tracked]) -> Vec<u32> {
        tracked
            .iter()
            .filter(|t| {
                self.rows
                    .iter()
                    .any(|(pid, _, start)| *pid == t.pid && *start == t.start)
            })
            .map(|t| t.pid)
            .collect()
    }
}

pub fn descendants(root: u32) -> Vec<u32> {
    ProcessTable::read()
        .map(|table| table.descendants(&[root]))
        .unwrap_or_default()
}

/// The descendants of `root` with their start times.
pub fn snapshot(root: u32) -> Vec<Tracked> {
    ProcessTable::read()
        .map(|table| table.track(root, &[]))
        .unwrap_or_default()
}

pub fn terminate_tree(roots: &[u32], grace: Duration) {
    terminate(roots, &[], &[], grace);
}

pub fn terminate_tracked(tracked: &[Tracked], grace: Duration) {
    terminate(&[], &[], tracked, grace);
}

/// Stops, in one pass, every process in the `trees`, every process group led by a
/// tree root or listed in `groups`, and every `tracked` process still running with its
/// recorded start time. Sends SIGHUP to the groups and SIGTERM to everything, waits up
/// to `grace` until all of it is gone, then SIGKILLs what is left, including
/// descendants forked meanwhile, and waits briefly for it to go. The trees are captured
/// before any signal is sent, because a dying parent hands its children to init.
pub fn terminate(trees: &[u32], groups: &[u32], tracked: &[Tracked], grace: Duration) {
    let trees: Vec<u32> = trees
        .iter()
        .copied()
        .filter(|&pid| signalable(pid))
        .collect();
    let mut groups: Vec<u32> = trees.iter().chain(groups).copied().collect();
    settle(&mut groups);
    groups.retain(|&leader| group_exists(leader));
    if trees.is_empty() && groups.is_empty() && tracked.is_empty() {
        return;
    }
    let mut pids = trees.clone();
    match ProcessTable::read().or_else(|| {
        std::thread::sleep(POLL);
        ProcessTable::read()
    }) {
        Some(table) => {
            pids.extend(table.descendants(&trees));
            pids.extend(table.running(tracked));
        }
        None => tracing::error!(
            ?trees,
            ?groups,
            ?tracked,
            "no process table; signalling only the trees' roots and the groups"
        ),
    }
    settle(&mut pids);
    if pids.is_empty() && groups.is_empty() {
        return;
    }

    for &leader in &groups {
        signal_group(leader, Signal::HUP);
        signal_group(leader, Signal::TERM);
    }
    for &pid in &pids {
        signal(pid, Signal::TERM);
    }
    if wait_gone(&mut pids, &groups, grace) {
        return;
    }
    let survivors: Vec<u32> = trees
        .iter()
        .copied()
        .filter(|&pid| alive(pid))
        .chain(pids.iter().copied())
        .collect();
    if let Some(table) = ProcessTable::read() {
        pids.extend(table.descendants(&survivors));
    }
    settle(&mut pids);
    groups.retain(|&leader| group_exists(leader));
    tracing::warn!(
        ?pids,
        ?groups,
        "agent processes outlived SIGTERM; sending SIGKILL"
    );
    for &pid in &pids {
        signal(pid, Signal::KILL);
    }
    for &leader in &groups {
        signal_group(leader, Signal::KILL);
    }
    if !wait_gone(&mut pids, &groups, KILL_WAIT) {
        tracing::error!(?pids, ?groups, "agent processes outlived SIGKILL");
    }
}

fn settle(pids: &mut Vec<u32>) {
    pids.retain(|&pid| signalable(pid));
    pids.sort_unstable();
    pids.dedup();
}

/// Polls until every pid and every process group is gone or `limit` passes; leaves the
/// surviving pids in `pids`.
fn wait_gone(pids: &mut Vec<u32>, groups: &[u32], limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        pids.retain(|&pid| alive(pid));
        if pids.is_empty() && !groups.iter().any(|&leader| group_exists(leader)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

fn descendants_in(table: &[(u32, u32)], root: u32) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in table {
        children.entry(ppid).or_default().push(pid);
    }
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    let mut found = Vec::new();
    while let Some(parent) = stack.pop() {
        for &child in children.get(&parent).into_iter().flatten() {
            if seen.insert(child) {
                found.push(child);
                stack.push(child);
            }
        }
    }
    found
}

fn signalable(pid: u32) -> bool {
    pid > 1 && pid != std::process::id()
}

fn to_pid(pid: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(pid).ok()?)
}

fn signal(target: u32, signal: Signal) {
    if let Some(target) = to_pid(target) {
        let _ = kill_process(target, signal);
    }
}

fn signal_group(leader: u32, signal: Signal) {
    if let Some(leader) = to_pid(leader) {
        let _ = kill_process_group(leader, signal);
    }
}

fn alive(target: u32) -> bool {
    to_pid(target).is_some_and(|target| test_kill_process(target).is_ok())
}

pub fn group_exists(leader: u32) -> bool {
    to_pid(leader).is_some_and(|leader| test_kill_process_group(leader).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_are_transitive_and_exclude_unrelated_processes() {
        let table = [(10, 1), (11, 10), (12, 11), (13, 10), (20, 1), (21, 20)];
        let mut found = descendants_in(&table, 10);
        found.sort_unstable();
        assert_eq!(found, [11, 12, 13]);
        assert_eq!(descendants_in(&table, 12), Vec::<u32>::new());
    }

    #[test]
    fn descendants_survive_a_cycle_in_the_table() {
        let table = [(10, 11), (11, 10)];
        assert_eq!(descendants_in(&table, 10), [11]);
    }

    #[test]
    fn track_keeps_reparented_entries_and_drops_exited_or_reused_ones() {
        let table = ProcessTable {
            rows: vec![
                (10, 1, "t10".into()),
                (11, 10, "t11".into()),
                (20, 1, "t20".into()),
                (21, 1, "new21".into()),
            ],
        };
        let tracked = |pid: u32, start: &str| Tracked {
            pid,
            start: start.into(),
        };
        let previous = [
            tracked(11, "t11"),
            tracked(20, "t20"),
            tracked(21, "old21"),
            tracked(30, "t30"),
        ];
        assert_eq!(
            table.track(10, &previous),
            [tracked(11, "t11"), tracked(20, "t20")]
        );
        assert_eq!(table.track(10, &[]), [tracked(11, "t11")]);
    }

    #[test]
    fn a_failed_or_empty_ps_reading_is_no_table() {
        let output = b"  10     1 Thu Oct 15 10:00:00 2026\n";
        assert!(ProcessTable::parse(true, output).is_some());
        assert!(ProcessTable::parse(false, output).is_none());
        assert!(ProcessTable::parse(true, b"").is_none());
        assert!(ProcessTable::parse(true, b"garbage\n").is_none());
    }

    #[test]
    fn never_signals_init_or_itself() {
        assert!(!signalable(0));
        assert!(!signalable(1));
        assert!(!signalable(std::process::id()));
        assert!(signalable(4242));
    }

    #[test]
    fn sigkills_a_tree_that_ignores_sigterm() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM HUP; sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        let root = child.id();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().expect("stdout")),
            &mut line,
        )
        .expect("leaf pid");
        let leaf: u32 = line.trim().parse().expect("pid");
        let found = descendants(root);
        let reaper = std::thread::spawn(move || child.wait());
        let started = Instant::now();
        terminate_tree(&[root], Duration::from_millis(300));
        let took = started.elapsed();
        reaper.join().expect("reaper").expect("wait");
        let deadline = Instant::now() + Duration::from_secs(1);
        while alive(leaf) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let survived = alive(leaf);
        if survived {
            signal(leaf, Signal::KILL);
        }
        assert_eq!(found, [leaf]);
        assert!(!survived, "leaf {leaf} survived");
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(2),
            "took {took:?}"
        );
    }

    #[test]
    fn snapshot_records_start_times_and_terminate_tracked_skips_reused_pids() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn");
        let root = child.id();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().expect("stdout")),
            &mut line,
        )
        .expect("leaf pid");
        let leaf: u32 = line.trim().parse().expect("pid");

        let recorded = snapshot(root).into_iter().find(|t| t.pid == leaf);
        let stale = Tracked {
            pid: leaf,
            start: "Thu Jan  1 00:00:00 1970".into(),
        };
        terminate_tracked(&[stale], Duration::from_millis(200));
        let survived_stale = alive(leaf);
        if let Some(recorded) = &recorded {
            terminate_tracked(std::slice::from_ref(recorded), Duration::from_secs(1));
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while alive(leaf) && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
        let survived = alive(leaf);

        signal(leaf, Signal::KILL);
        signal(root, Signal::KILL);
        child.wait().expect("wait");

        let recorded = recorded.expect("snapshot lists the sleep");
        assert!(!recorded.start.is_empty(), "{recorded:?}");
        assert!(
            survived_stale,
            "killed {leaf} despite a different start time"
        );
        assert!(!survived, "leaf {leaf} survived terminate_tracked");
    }
}
