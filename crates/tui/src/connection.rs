use client::Client;
use protocol::{AgentId, DiffScope, GitStat, Request, Response, ServerMessage};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum Incoming {
    Server(ServerMessage),
    Response(Response),
    Failed(String),
    GitStat {
        id: AgentId,
        result: Result<GitStat, String>,
    },
    Diff {
        id: AgentId,
        scope: DiffScope,
        result: Result<String, String>,
    },
    Closed,
}

/// Every request goes out without waiting for the replies before it, so input and server
/// events keep flowing while the daemon works on a slow one. Git replies come back tagged
/// with the agent as they arrive; every other reply is passed on in request order.
pub fn spawn(mut client: Client) -> (UnboundedSender<Request>, UnboundedReceiver<Incoming>) {
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<Request>();
    let (incoming_tx, incoming) = mpsc::unbounded_channel();
    let (ordered_tx, mut ordered) =
        mpsc::unbounded_channel::<JoinHandle<anyhow::Result<Response>>>();
    let relay_tx = incoming_tx.clone();
    tokio::spawn(async move {
        while let Some(reply) = ordered.recv().await {
            let result = reply
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("waiting for a reply: {e}")));
            if !pass_on(&relay_tx, result) {
                break;
            }
        }
    });
    tokio::spawn(async move {
        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break };
                    if let Request::GitStat { id } | Request::GitDiff { id, .. } = request {
                        let diff = match request {
                            Request::GitDiff { scope, .. } => Some(scope),
                            _ => None,
                        };
                        let sent = client.send(request).await;
                        let tx = incoming_tx.clone();
                        tokio::spawn(async move {
                            let result = match sent {
                                Ok(reply) => reply.wait().await,
                                Err(e) => Err(e),
                            }
                            .map_err(|e| format!("{e:#}"));
                            let _ = tx.send(git_reply(id, diff, result));
                        });
                        continue;
                    }
                    let reply = match client.send(request).await {
                        Ok(reply) => tokio::spawn(reply.wait()),
                        Err(e) => tokio::spawn(async move { Err(e) }),
                    };
                    if ordered_tx.send(reply).is_err() {
                        break;
                    }
                }
                event = client.next_event() => match event {
                    Some(message) => {
                        if incoming_tx.send(Incoming::Server(message)).is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = incoming_tx.send(Incoming::Closed);
                        break;
                    }
                },
            }
        }
    });
    (requests_tx, incoming)
}

/// Passes a reply on to the UI; false once the UI is gone or the daemon closed the connection.
fn pass_on(incoming: &UnboundedSender<Incoming>, result: anyhow::Result<Response>) -> bool {
    match result {
        Ok(Response::Ok) => true,
        Ok(response) => incoming.send(Incoming::Response(response)).is_ok(),
        Err(e) => {
            let message = format!("{e:#}");
            let closed = message.contains("closed the connection");
            let sent = incoming.send(Incoming::Failed(message)).is_ok();
            if closed {
                let _ = incoming.send(Incoming::Closed);
            }
            sent && !closed
        }
    }
}

fn git_reply(id: AgentId, diff: Option<DiffScope>, result: Result<Response, String>) -> Incoming {
    let unexpected = |other: Response| format!("unexpected reply {other:?}");
    if let Some(scope) = diff {
        let result = result.and_then(|response| match response {
            Response::Diff(text) => Ok(text),
            other => Err(unexpected(other)),
        });
        Incoming::Diff { id, scope, result }
    } else {
        let result = result.and_then(|response| match response {
            Response::GitStat(stat) => Ok(stat),
            other => Err(unexpected(other)),
        });
        Incoming::GitStat { id, result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::{ClientMessage, PROTOCOL_VERSION, StatePaths, read_message, write_message};
    use std::time::Duration;
    use tokio::net::UnixListener;

    #[test]
    fn git_replies_of_the_wrong_kind_become_errors() {
        let id = AgentId::random();
        let stat = git_reply(id, None, Ok(Response::Diff("+x".into())));
        assert!(
            matches!(&stat, Incoming::GitStat { id: got, result: Err(e) } if *got == id && e.contains("unexpected")),
            "{stat:?}"
        );
        let diff = git_reply(id, Some(DiffScope::All), Ok(Response::Diff("+x".into())));
        assert!(
            matches!(&diff, Incoming::Diff { result: Ok(text), .. } if text == "+x"),
            "{diff:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn git_errors_arrive_tagged_and_never_as_failed() {
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
            while let Ok(Some(ClientMessage::Request { id, request })) =
                read_message(&mut stream).await
            {
                let error = match request {
                    Request::GitStat { .. } => "stat failed",
                    Request::GitDiff { .. } => "diff failed",
                    _ => "list failed",
                };
                let reply = ServerMessage::Response {
                    id,
                    result: Err(error.into()),
                };
                write_message(&mut stream, &reply).await.expect("reply");
            }
        });
        let client = Client::connect(&paths).await.expect("connect");
        let (requests, mut incoming) = spawn(client);
        let agent = AgentId::random();
        for request in [
            Request::GitStat { id: agent },
            Request::GitDiff {
                id: agent,
                scope: protocol::DiffScope::All,
            },
            Request::ListAgents,
        ] {
            requests.send(request).expect("send");
        }
        let mut seen = Vec::new();
        for _ in 0..3 {
            let message = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .expect("reply in time")
                .expect("open");
            seen.push(format!("{message:?}"));
            match message {
                Incoming::GitStat { id, result } => {
                    assert_eq!((id, result), (agent, Err("stat failed".into())));
                }
                Incoming::Diff { id, result, .. } => {
                    assert_eq!((id, result), (agent, Err("diff failed".into())));
                }
                Incoming::Failed(message) => assert_eq!(message, "list failed"),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(
            seen.iter().filter(|m| m.starts_with("Failed")).count(),
            1,
            "{seen:?}"
        );
        drop(requests);
        fake.await.expect("fake server");
    }

    async fn next_request(stream: &mut tokio::net::UnixStream) -> (u64, Request) {
        match read_message(stream).await {
            Ok(Some(ClientMessage::Request { id, request })) => (id, request),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn events_and_requests_flow_while_a_reply_is_withheld() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::new(dir.path());
        let listener = UnixListener::bind(paths.socket()).expect("bind");
        let agent = AgentId::random();
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
            let (kill, request) = next_request(&mut stream).await;
            assert!(matches!(request, Request::KillAgent { .. }), "{request:?}");
            write_message(&mut stream, &ServerMessage::AgentBell(agent))
                .await
                .expect("push");
            let (list, request) = next_request(&mut stream).await;
            assert!(matches!(request, Request::ListAgents), "{request:?}");
            for (id, result) in [
                (list, Ok(Response::Agents(Vec::new()))),
                (kill, Err("kill failed".to_string())),
            ] {
                write_message(&mut stream, &ServerMessage::Response { id, result })
                    .await
                    .expect("reply");
            }
            while let Ok(Some(_)) = read_message::<_, ClientMessage>(&mut stream).await {}
        });
        let client = Client::connect(&paths).await.expect("connect");
        let (requests, mut incoming) = spawn(client);
        let mut next = async || {
            tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .expect("message in time")
                .expect("open")
        };

        requests
            .send(Request::KillAgent { id: agent })
            .expect("send");
        let pushed = next().await;
        assert!(
            matches!(pushed, Incoming::Server(ServerMessage::AgentBell(id)) if id == agent),
            "{pushed:?}"
        );
        requests.send(Request::ListAgents).expect("send");
        let first = next().await;
        assert!(
            matches!(&first, Incoming::Failed(message) if message == "kill failed"),
            "{first:?}"
        );
        let second = next().await;
        assert!(
            matches!(&second, Incoming::Response(Response::Agents(agents)) if agents.is_empty()),
            "{second:?}"
        );
        drop(requests);
        fake.await.expect("fake server");
    }
}
