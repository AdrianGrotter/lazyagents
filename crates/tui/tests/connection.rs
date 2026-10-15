use std::path::PathBuf;
use std::time::Duration;

use protocol::{Request, Response, ServerMessage, StatePaths};
use tui::connection::{Incoming, spawn};

async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Incoming>) -> Incoming {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timely")
        .expect("open")
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_events_and_closure_flow_through_the_actor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = StatePaths::new(dir.path());
    let daemon = tokio::spawn(daemon::run(daemon::Config {
        paths: paths.clone(),
        exe: PathBuf::from("/usr/bin/true"),
        idle_exit: None,
    }));
    let client = loop {
        if let Ok(client) = client::Client::connect(&paths).await {
            break client;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (requests, mut incoming) = spawn(client);

    requests.send(Request::ListProjects).expect("send");
    assert!(
        matches!(next(&mut incoming).await, Incoming::Response(Response::Projects(p)) if p.is_empty())
    );

    let project_dir = tempfile::tempdir().expect("project");
    requests
        .send(Request::AddProject {
            path: project_dir.path().into(),
            name: None,
        })
        .expect("send");
    let mut saw_event = false;
    let mut saw_response = false;
    while !(saw_event && saw_response) {
        match next(&mut incoming).await {
            Incoming::Server(ServerMessage::ProjectsChanged(_)) => saw_event = true,
            Incoming::Response(Response::Project(_)) => saw_response = true,
            other => panic!("unexpected {other:?}"),
        }
    }

    requests
        .send(Request::KillAgent {
            id: protocol::AgentId::random(),
        })
        .expect("send");
    assert!(
        matches!(next(&mut incoming).await, Incoming::Failed(e) if e.contains("unknown agent"))
    );

    requests.send(Request::Shutdown).expect("send");
    daemon.await.expect("join").expect("clean");
    loop {
        if matches!(next(&mut incoming).await, Incoming::Closed) {
            break;
        }
    }
}
