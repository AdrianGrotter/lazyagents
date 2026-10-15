use std::fs::OpenOptions;
use std::sync::Mutex;

use anyhow::Context;
use protocol::StatePaths;

pub fn run() -> anyhow::Result<()> {
    let paths = StatePaths::from_env()?;
    std::fs::create_dir_all(paths.dir())
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
    let exe = std::env::current_exe().context("locating the lazyagents binary")?;
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(daemon::run(daemon::Config { paths, exe }));
    if let Err(e) = &result {
        tracing::error!("{e:#}");
    }
    result
}
