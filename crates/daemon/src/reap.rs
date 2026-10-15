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

pub fn descendants(root: u32) -> Vec<u32> {
    descendants_in(&links(&process_table()), root)
}

/// The descendants of `root` with their start times.
pub fn snapshot(root: u32) -> Vec<Tracked> {
    let table = process_table();
    let starts: HashMap<u32, &str> = table
        .iter()
        .map(|(pid, _, start)| (*pid, start.as_str()))
        .collect();
    descendants_in(&links(&table), root)
        .into_iter()
        .filter_map(|pid| {
            Some(Tracked {
                pid,
                start: starts.get(&pid)?.to_string(),
            })
        })
        .collect()
}

/// Signals every root's process group and every process in the roots' trees, waits up
/// to `grace` until all of them and the groups are gone, then SIGKILLs whatever is left,
/// including descendants forked meanwhile, and waits briefly for it to go. The trees are
/// captured before any signal is sent, because a dying parent hands its children to init.
pub fn terminate_tree(roots: &[u32], grace: Duration) {
    let roots: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|&pid| signalable(pid))
        .collect();
    if roots.is_empty() {
        return;
    }
    let mut pids = roots.clone();
    pids.extend(descendants_of(&process_table(), &roots));
    settle(&mut pids);

    for &root in &roots {
        signal_group(root, Signal::HUP);
        signal_group(root, Signal::TERM);
    }
    for &pid in &pids {
        signal(pid, Signal::TERM);
    }
    if wait_gone(&mut pids, &roots, grace) {
        return;
    }
    let survivors: Vec<u32> = roots.iter().chain(&pids).copied().collect();
    pids.extend(descendants_of(&process_table(), &survivors));
    settle(&mut pids);
    tracing::warn!(?pids, "agent processes outlived SIGTERM; sending SIGKILL");
    for &pid in &pids {
        signal(pid, Signal::KILL);
    }
    for &root in &roots {
        signal_group(root, Signal::KILL);
    }
    if !wait_gone(&mut pids, &roots, KILL_WAIT) {
        tracing::error!(?pids, "agent processes outlived SIGKILL");
    }
}

/// Sends SIGTERM to each tracked process still running with its recorded start time,
/// waits up to `grace`, then SIGKILLs those still matching.
pub fn terminate_tracked(tracked: &[Tracked], grace: Duration) {
    if tracked.is_empty() {
        return;
    }
    let mut pids = matching(tracked);
    if pids.is_empty() {
        return;
    }
    for &pid in &pids {
        signal(pid, Signal::TERM);
    }
    if wait_gone(&mut pids, &[], grace) {
        return;
    }
    let mut pids = matching(tracked);
    tracing::warn!(
        ?pids,
        "detached agent processes outlived SIGTERM; sending SIGKILL"
    );
    for &pid in &pids {
        signal(pid, Signal::KILL);
    }
    if !wait_gone(&mut pids, &[], KILL_WAIT) {
        tracing::error!(?pids, "detached agent processes outlived SIGKILL");
    }
}

fn matching(tracked: &[Tracked]) -> Vec<u32> {
    let table = process_table();
    let mut pids: Vec<u32> = tracked
        .iter()
        .filter(|t| {
            table
                .iter()
                .any(|(pid, _, start)| *pid == t.pid && *start == t.start)
        })
        .map(|t| t.pid)
        .collect();
    settle(&mut pids);
    pids
}

fn settle(pids: &mut Vec<u32>) {
    pids.retain(|&pid| signalable(pid));
    pids.sort_unstable();
    pids.dedup();
}

/// Polls until every pid and every root's process group is gone or `limit` passes;
/// leaves the surviving pids in `pids`.
fn wait_gone(pids: &mut Vec<u32>, roots: &[u32], limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        pids.retain(|&pid| alive(pid));
        if pids.is_empty() && groups_gone(roots) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

fn groups_gone(roots: &[u32]) -> bool {
    roots.iter().all(|&root| !group_alive(root))
}

/// `(pid, ppid, start)` for every process; `start` is `ps`'s `lstart` text.
fn process_table() -> Vec<(u32, u32, String)> {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,lstart="])
        .output()
    else {
        tracing::warn!("ps failed; only agent process groups will be signalled");
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
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
        .collect()
}

fn links(table: &[(u32, u32, String)]) -> Vec<(u32, u32)> {
    table.iter().map(|(pid, ppid, _)| (*pid, *ppid)).collect()
}

fn descendants_of(table: &[(u32, u32, String)], roots: &[u32]) -> Vec<u32> {
    let links = links(table);
    roots
        .iter()
        .flat_map(|&root| descendants_in(&links, root))
        .collect()
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

fn group_alive(leader: u32) -> bool {
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
