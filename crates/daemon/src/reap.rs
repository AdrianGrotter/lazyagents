//! Terminates agent process trees, including descendants that left the agent's
//! process group or session.

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};

const POLL: Duration = Duration::from_millis(25);
const KILL_WAIT: Duration = Duration::from_millis(500);

pub fn descendants(root: u32) -> Vec<u32> {
    descendants_in(&process_table(), root)
}

/// Signals every root's process group and every process in the roots' trees, waits up
/// to `grace` for them to exit, then SIGKILLs whatever is left and waits briefly for it
/// to go. The trees are captured before any signal is sent, because a dying parent hands
/// its children to init.
pub fn terminate_tree(roots: &[u32], grace: Duration) {
    let roots: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|&pid| signalable(pid))
        .collect();
    if roots.is_empty() {
        return;
    }
    let table = process_table();
    let mut pids = roots.clone();
    for &root in &roots {
        pids.extend(descendants_in(&table, root));
    }
    pids.retain(|&pid| signalable(pid));
    pids.sort_unstable();
    pids.dedup();

    for &root in &roots {
        signal_group(root, Signal::HUP);
    }
    for &pid in &pids {
        signal(pid, Signal::TERM);
    }
    if wait_gone(&mut pids, grace) {
        return;
    }
    tracing::warn!(?pids, "agent processes outlived SIGTERM; sending SIGKILL");
    for &pid in &pids {
        signal(pid, Signal::KILL);
    }
    for &root in &roots {
        signal_group(root, Signal::KILL);
    }
    if !wait_gone(&mut pids, KILL_WAIT) {
        tracing::error!(?pids, "agent processes outlived SIGKILL");
    }
}

/// Polls until every pid is gone or `limit` passes; leaves the survivors in `pids`.
fn wait_gone(pids: &mut Vec<u32>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        pids.retain(|&pid| alive(pid));
        if pids.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

fn process_table() -> Vec<(u32, u32)> {
    let Ok(output) = Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output() else {
        tracing::warn!("ps failed; only agent process groups will be signalled");
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
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
}
