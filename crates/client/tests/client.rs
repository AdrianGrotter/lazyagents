use std::path::PathBuf;
use std::time::Duration;

use client::Client;
use protocol::PROTOCOL_VERSION;
use protocol::{
    ClientMessage, Request, Response, ServerMessage, StatePaths, read_message, write_message,
};
use tokio::net::UnixListener;

async fn start_daemon(dir: &std::path::Path) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let task = tokio::spawn(daemon::run(daemon::Config {
        paths: StatePaths::new(dir),
        exe: PathBuf::from("/usr/bin/true"),
        idle_exit: None,
        direnv: None,
        claude_config: None,
    }));
    let paths = StatePaths::new(dir);
    for _ in 0..250 {
        if Client::connect(&paths).await.is_ok() {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("daemon never came up");
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_get_their_own_responses_while_events_flow() {
    let dir = tempfile::tempdir().expect("tempdir");
    let task = start_daemon(dir.path()).await;
    let paths = StatePaths::new(dir.path());
    let mut a = Client::connect(&paths).await.expect("a");
    let mut b = Client::connect(&paths).await.expect("b");
    let project_dir = tempfile::tempdir().expect("project");
    let Response::Project(project) = a
        .request(Request::AddProject {
            path: project_dir.path().into(),
            name: None,
        })
        .await
        .expect("add")
    else {
        panic!("project");
    };
    let event = tokio::time::timeout(Duration::from_secs(5), b.next_event())
        .await
        .expect("event in time");
    assert_eq!(event, Some(ServerMessage::ProjectsChanged(vec![project])));
    let err = a
        .request(Request::RemoveProject {
            id: protocol::ProjectId::random(),
        })
        .await
        .expect_err("unknown");
    assert!(err.to_string().contains("unknown project"), "{err}");
    a.request(Request::Shutdown).await.expect("shutdown");
    task.await.expect("join").expect("clean");
}

#[tokio::test(flavor = "multi_thread")]
async fn version_mismatch_is_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let listener = UnixListener::bind(paths.socket()).expect("bind");
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        write_message(
            &mut stream,
            &ServerMessage::Welcome {
                protocol_version: 999,
                daemon_version: "9".into(),
            },
        )
        .await
        .expect("welcome");
        let _: Option<ClientMessage> = read_message(&mut stream).await.ok().flatten();
    });
    let err = Client::connect(&paths).await.err().expect("mismatch");
    assert!(err.to_string().contains("protocol"), "{err}");
    assert!(err.to_string().contains("lazyagents shutdown"), "{err}");
    assert!(
        err.downcast_ref::<client::VersionMismatch>().is_some(),
        "{err}"
    );
    fake.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn sent_requests_get_their_own_replies_in_any_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let listener = UnixListener::bind(paths.socket()).expect("bind");
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        write_message(
            &mut stream,
            &ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "test".into(),
            },
        )
        .await
        .expect("welcome");
        let _: Option<ClientMessage> = read_message(&mut stream).await.expect("hello");
        let mut ids = Vec::new();
        for _ in 0..2 {
            match read_message(&mut stream).await.expect("request") {
                Some(ClientMessage::Request { id, request }) => ids.push((id, request)),
                other => panic!("unexpected {other:?}"),
            }
        }
        for (id, request) in ids.into_iter().rev() {
            let result = match request {
                Request::GitDiff { .. } => Ok(Response::Diff("+slow".into())),
                _ => Err("fast".into()),
            };
            write_message(&mut stream, &ServerMessage::Response { id, result })
                .await
                .expect("reply");
        }
        let _: Option<ClientMessage> = read_message(&mut stream).await.ok().flatten();
    });
    let mut client = Client::connect(&paths).await.expect("connect");
    let id = protocol::AgentId::random();
    let slow = client
        .send(Request::GitDiff {
            id,
            scope: protocol::DiffScope::All,
        })
        .await
        .expect("send");
    let fast = client.send(Request::ListAgents).await.expect("send");
    let fast = tokio::time::timeout(Duration::from_secs(2), fast.wait())
        .await
        .expect("fast reply in time")
        .expect_err("fast fails");
    assert_eq!(fast.to_string(), "fast");
    let slow = tokio::time::timeout(Duration::from_secs(2), slow.wait())
        .await
        .expect("slow reply in time")
        .expect("slow succeeds");
    assert_eq!(slow, Response::Diff("+slow".into()));
    drop(client);
    fake.await.expect("fake server");
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_without_daemon_fails_fast() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(Client::connect(&StatePaths::new(dir.path())).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_fail_when_the_daemon_goes_away() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let listener = UnixListener::bind(paths.socket()).expect("bind");
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        write_message(
            &mut stream,
            &ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "test".into(),
            },
        )
        .await
        .expect("welcome");
        let _: Option<ClientMessage> = read_message(&mut stream).await.expect("hello");
        let _: Option<ClientMessage> = read_message(&mut stream).await.expect("request");
    });
    let mut client = Client::connect(&paths).await.expect("connect");
    let first = tokio::time::timeout(
        Duration::from_secs(2),
        client.request(Request::ListProjects),
    )
    .await
    .expect("first request must not hang")
    .expect_err("daemon went away");
    assert!(first.to_string().contains("closed"), "{first}");
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        client.request(Request::ListProjects),
    )
    .await
    .expect("second request must not hang")
    .expect_err("still closed");
    assert!(second.to_string().contains("closed"), "{second}");
    fake.await.expect("fake server");
}

fn is_zombie_or_alive(pid: &str) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .is_ok_and(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

#[tokio::test(flavor = "multi_thread")]
async fn spawned_daemon_is_reaped_after_it_exits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path().join("state"));
    let pidfile = dir.path().join("pid");
    let exe = dir.path().join("fake-daemon");
    std::fs::write(
        &exe,
        format!("#!/bin/sh\necho $$ > '{}'\nsleep 0.5\n", pidfile.display()),
    )
    .expect("fake daemon");
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let socket = paths.socket();
    let fake = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let listener = UnixListener::bind(socket).expect("bind");
        let (mut stream, _) = listener.accept().await.expect("accept");
        write_message(
            &mut stream,
            &ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "test".into(),
            },
        )
        .await
        .expect("welcome");
        let _: Option<ClientMessage> = read_message(&mut stream).await.ok().flatten();
    });
    let client = Client::connect_or_spawn(&paths, &exe)
        .await
        .expect("connect");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut pid = String::new();
    while pid.is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        pid = std::fs::read_to_string(&pidfile).unwrap_or_default();
    }
    let pid = pid.trim();
    assert!(!pid.is_empty(), "fake daemon never ran");
    while is_zombie_or_alive(pid) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!is_zombie_or_alive(pid), "daemon {pid} was never reaped");
    drop(client);
    fake.abort();
}

#[test]
fn daemon_pid_ignores_an_unlocked_lock_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    std::fs::write(paths.lock_file(), "12345").expect("lock file");
    assert_eq!(client::daemon_pid(&paths), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_exiting_at_startup_fails_fast() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let exe = dir.path().join("fake-daemon");
    std::fs::write(&exe, "#!/bin/sh\nexit 3\n").expect("fake daemon");
    std::fs::set_permissions(&exe, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod");
    let started = std::time::Instant::now();
    let err = Client::connect_or_spawn(&paths, &exe)
        .await
        .err()
        .expect("no daemon");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        err.to_string().contains("daemon exited during startup"),
        "{err:#}"
    );
}

#[test]
fn stop_daemon_without_a_daemon_is_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    client::stop_daemon(&paths, Duration::from_secs(1)).expect("nothing to stop");

    let mut bystander = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    std::fs::write(paths.lock_file(), bystander.id().to_string()).expect("lock file");
    let result = client::stop_daemon(&paths, Duration::from_secs(1));
    let survived = bystander.try_wait().expect("try_wait").is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    result.expect("an unlocked lock file names no daemon");
    assert!(
        survived,
        "stop_daemon signalled a pid from an unlocked lock file"
    );
}

/// A `sleep` standing in for a daemon whose lock is held by the test; the lock is
/// released once `sleep` exits. SIGKILLed by its pid on drop if it still runs.
struct LockedSleeper(std::sync::Arc<std::sync::Mutex<std::process::Child>>);

impl LockedSleeper {
    fn exited(&self) -> bool {
        let mut child = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !matches!(child.try_wait(), Ok(None))
    }
}

impl Drop for LockedSleeper {
    fn drop(&mut self) {
        let mut child = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(child.try_wait(), Ok(None)) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn stop_daemon_waits_for_a_pid_written_after_the_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let sleeper = std::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("sleep");
    let pid = sleeper.id();
    let lock = paths.lock_file();
    std::fs::write(&lock, "").expect("lock file");
    let held = std::fs::File::open(&lock).expect("open lock file");
    held.lock().expect("hold the lock");
    let fake = LockedSleeper(std::sync::Arc::new(std::sync::Mutex::new(sleeper)));
    let watched = LockedSleeper(std::sync::Arc::clone(&fake.0));
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&lock, pid.to_string()).expect("write pid");
        while !watched.exited() {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(held);
    });

    let result = client::stop_daemon(&paths, Duration::from_secs(5));
    let stopped = fake.exited();
    drop(fake);
    writer.join().expect("writer");
    result.expect("stop");
    assert!(stopped, "stop_daemon returned without stopping {pid}");
}

/// Starts a `sleep` stand-in and holds the daemon lock for it, with `contents(pid)` in the
/// lock file, until the stand-in exits. Join the returned thread after dropping the
/// stand-in.
fn locked_sleeper(
    paths: &StatePaths,
    contents: impl FnOnce(u32) -> String,
) -> (LockedSleeper, std::thread::JoinHandle<()>) {
    let sleeper = std::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("sleep");
    let pid = sleeper.id();
    let fake = LockedSleeper(std::sync::Arc::new(std::sync::Mutex::new(sleeper)));
    let lock = paths.lock_file();
    std::fs::write(&lock, contents(pid)).expect("lock file");
    let held = std::fs::File::open(&lock).expect("open lock file");
    held.lock().expect("hold the lock");
    let watched = LockedSleeper(std::sync::Arc::clone(&fake.0));
    let holder = std::thread::spawn(move || {
        while !watched.exited() {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(held);
    });
    (fake, holder)
}

#[test]
fn stale_start_time_is_never_signalled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let (fake, holder) = locked_sleeper(&paths, |pid| {
        protocol::lockfile::format(pid, Some("Mon Jan  1 00:00:00 1990"))
    });
    let pid = fake.0.lock().expect("child").id();

    let result = client::stop_daemon(&paths, Duration::from_secs(1));
    let survived = !fake.exited();
    drop(fake);
    holder.join().expect("holder");
    let err = result.expect_err("a stale start time names no daemon");
    assert!(err.to_string().contains(&pid.to_string()), "{err:#}");
    assert!(
        survived,
        "stop_daemon signalled {pid} despite a stale start time"
    );
}

#[test]
fn matching_start_time_is_stopped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let (fake, holder) = locked_sleeper(&paths, |pid| {
        let start = protocol::lockfile::process_start(pid).expect("start of sleep");
        protocol::lockfile::format(pid, Some(&start))
    });

    let result = client::stop_daemon(&paths, Duration::from_secs(5));
    let stopped = fake.exited();
    drop(fake);
    holder.join().expect("holder");
    result.expect("stop");
    assert!(stopped, "stop_daemon returned without stopping the daemon");
}

#[test]
fn pid_only_lock_file_is_still_trusted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let (fake, holder) = locked_sleeper(&paths, |pid| pid.to_string());

    let result = client::stop_daemon(&paths, Duration::from_secs(5));
    let stopped = fake.exited();
    drop(fake);
    holder.join().expect("holder");
    result.expect("stop");
    assert!(stopped, "stop_daemon returned without stopping the daemon");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_state_dir_too_long_for_a_socket_is_refused_before_spawning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let long = dir.path().join("d".repeat(120));
    let paths = StatePaths::new(&long);
    let Err(error) = Client::connect_or_spawn(&paths, std::path::Path::new("/usr/bin/false")).await
    else {
        panic!("connected through an unusable socket path");
    };
    let message = format!("{error:#}");
    assert!(message.contains("too long for a unix socket"), "{message}");
    assert!(message.contains("LAZYAGENTS_STATE_DIR"), "{message}");
    assert!(!long.exists(), "the state dir was created anyway");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relative_state_dir_is_measured_as_the_daemon_will_bind_it() {
    let relative = format!("{}s", "../".repeat(28));
    let paths = StatePaths::new(&relative);
    assert!(paths.socket().as_os_str().len() < 100, "short as written");
    let Err(error) = Client::connect_or_spawn(&paths, std::path::Path::new("/usr/bin/false")).await
    else {
        panic!("connected through an unusable socket path");
    };
    let message = format!("{error:#}");
    assert!(message.contains("too long for a unix socket"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_connection_is_a_typed_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let listener = UnixListener::bind(paths.socket()).expect("bind");
    let fake = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        write_message(
            &mut stream,
            &ServerMessage::Welcome {
                protocol_version: PROTOCOL_VERSION,
                daemon_version: "test".into(),
            },
        )
        .await
        .expect("welcome");
        let _: Option<ClientMessage> = read_message(&mut stream).await.expect("hello");
        let _: Option<ClientMessage> = read_message(&mut stream).await.expect("request");
    });
    let mut client = Client::connect(&paths).await.expect("connect");
    let waiting = client.send(Request::ListAgents).await.expect("send");
    fake.await.expect("fake server");
    let err = waiting.wait().await.expect_err("closed");
    assert!(
        err.downcast_ref::<client::ConnectionClosed>().is_some(),
        "{err}"
    );
    assert_eq!(err.to_string(), "daemon closed the connection");
    let later = client
        .send(Request::ListAgents)
        .await
        .err()
        .expect("sending fails once the connection is closed");
    assert!(
        later.downcast_ref::<client::ConnectionClosed>().is_some(),
        "{later}"
    );
}
