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
    let slow = client.send(Request::GitDiff { id }).await.expect("send");
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
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
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
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert!(
        err.to_string().contains("daemon exited during startup"),
        "{err:#}"
    );
}
