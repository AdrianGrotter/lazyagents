use std::fs::OpenOptions;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use protocol::StatePaths;

const DEFAULT_IDLE_EXIT_SECS: u64 = 600;

pub fn run() -> anyhow::Result<()> {
    detach_from_terminal();
    let paths = StatePaths::from_env()?;
    paths
        .create_dir()
        .with_context(|| format!("creating {}", paths.dir().display()))?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())
        .with_context(|| format!("opening {}", paths.log_file().display()))?;
    tracing_subscriber::fmt()
        .with_writer(Mutex::new(log))
        .with_ansi(false)
        .init();
    let result = start(paths);
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}

/// Leaves the caller's session so the daemon has no controlling terminal. Fails with
/// EPERM for a process group leader, such as a daemon a job-control shell started in
/// the foreground, which then stays attached to that terminal.
fn detach_from_terminal() {
    let _ = rustix::process::setsid();
}

fn start(paths: StatePaths) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating the lazyagents binary")?;
    let idle_exit = idle_exit(std::env::var("LAZYAGENTS_IDLE_EXIT_SECS").ok().as_deref())?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(daemon::run(daemon::Config {
            paths,
            exe,
            idle_exit,
        }))
}

fn idle_exit(value: Option<&str>) -> anyhow::Result<Option<Duration>> {
    let secs = match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => value
            .parse::<u64>()
            .with_context(|| format!("LAZYAGENTS_IDLE_EXIT_SECS={value:?} is not a number"))?,
        None => DEFAULT_IDLE_EXIT_SECS,
    };
    Ok((secs > 0).then(|| Duration::from_secs(secs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_exit_defaults_to_ten_minutes_and_zero_disables_it() {
        assert_eq!(idle_exit(None).unwrap(), Some(Duration::from_secs(600)));
        assert_eq!(idle_exit(Some("")).unwrap(), Some(Duration::from_secs(600)));
        assert_eq!(idle_exit(Some("5")).unwrap(), Some(Duration::from_secs(5)));
        assert_eq!(idle_exit(Some("0")).unwrap(), None);
        assert!(idle_exit(Some("soon")).is_err());
    }
}
