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
