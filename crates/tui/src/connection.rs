use client::{Client, ConnectionClosed};
use protocol::{AgentId, DiffScope, Failure, GitStat, Request, Response, ServerMessage};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum Incoming {
    Server(ServerMessage),
    /// The daemon's answer to `request`, or why there is none.
    Reply {
        request: Request,
        result: Result<Response, Failure>,
    },
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
/// with the agent as they arrive; every other reply is passed on in request order, with its
/// request. A plain `Ok` is passed on only for a removal, whose success the UI reports.
pub fn spawn(mut client: Client) -> (UnboundedSender<Request>, UnboundedReceiver<Incoming>) {
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<Request>();
    let (incoming_tx, incoming) = mpsc::unbounded_channel();
    let (ordered_tx, mut ordered) =
        mpsc::unbounded_channel::<(Request, JoinHandle<anyhow::Result<Response>>)>();
    let relay_tx = incoming_tx.clone();
    tokio::spawn(async move {
        while let Some((request, reply)) = ordered.recv().await {
            let result = reply
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("waiting for a reply: {e}")));
            if !pass_on(&relay_tx, request, result) {
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
                    let sent = request.clone();
                    let reply = match client.send(request).await {
                        Ok(reply) => tokio::spawn(reply.wait()),
                        Err(e) => tokio::spawn(async move { Err(e) }),
                    };
                    if ordered_tx.send((sent, reply)).is_err() {
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
fn pass_on(
    incoming: &UnboundedSender<Incoming>,
    request: Request,
    result: anyhow::Result<Response>,
) -> bool {
    match result {
        Ok(Response::Ok) if !matches!(request, Request::RemoveAgent { .. }) => true,
        Ok(response) => incoming
            .send(Incoming::Reply {
                request,
                result: Ok(response),
            })
            .is_ok(),
        Err(e) => {
            let closed = e.is::<ConnectionClosed>();
            let failure = e
                .downcast::<Failure>()
                .unwrap_or_else(|e| Failure::from(format!("{e:#}")));
            let sent = incoming
                .send(Incoming::Reply {
                    request,
                    result: Err(failure),
                })
                .is_ok();
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
    fn only_a_closed_connection_ends_the_relay() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let refusal = anyhow::anyhow!("git: the remote closed the connection");
        assert!(pass_on(&tx, Request::ListAgents, Err(refusal)));
        assert!(
            matches!(rx.try_recv(), Ok(Incoming::Reply { result: Err(e), .. }) if e.to_string().contains("remote")),
        );
        assert!(rx.try_recv().is_err());

        let closed = anyhow::Error::new(client::ConnectionClosed);
        assert!(!pass_on(&tx, Request::ListAgents, Err(closed)));
        assert!(matches!(rx.try_recv(), Ok(Incoming::Reply { .. })));
        assert!(matches!(rx.try_recv(), Ok(Incoming::Closed)));
    }

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
                Incoming::Reply {
                    request: Request::ListAgents,
                    result,
                } => assert_eq!(result, Err("list failed".into())),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(
            seen.iter().filter(|m| m.starts_with("Reply")).count(),
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
                (kill, Err(Failure::from("kill failed"))),
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
            matches!(
                &first,
                Incoming::Reply { request: Request::KillAgent { .. }, result: Err(failure) }
                    if failure.to_string() == "kill failed"
            ),
            "{first:?}"
        );
        let second = next().await;
        assert!(
            matches!(
                &second,
                Incoming::Reply { request: Request::ListAgents, result: Ok(Response::Agents(agents)) }
                    if agents.is_empty()
            ),
            "{second:?}"
        );
        drop(requests);
        fake.await.expect("fake server");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_a_removal_is_reported_when_it_succeeds_without_a_value() {
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
            while let Ok(Some(ClientMessage::Request { id, .. })) = read_message(&mut stream).await
            {
                let reply = ServerMessage::Response {
                    id,
                    result: Ok(Response::Ok),
                };
                write_message(&mut stream, &reply).await.expect("reply");
            }
        });
        let client = Client::connect(&paths).await.expect("connect");
        let (requests, mut incoming) = spawn(client);
        let agent = AgentId::random();
        let removal = Request::RemoveAgent {
            id: agent,
            remove_worktree: true,
            force: false,
            delete_branch: true,
        };
        for request in [
            Request::KillAgent { id: agent },
            removal.clone(),
            Request::Unsubscribe,
        ] {
            requests.send(request).expect("send");
        }
        let reply = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
            .await
            .expect("reply in time")
            .expect("open");
        assert!(
            matches!(&reply, Incoming::Reply { request, result: Ok(Response::Ok) } if *request == removal),
            "{reply:?}"
        );
        let quiet = tokio::time::timeout(Duration::from_millis(300), incoming.recv()).await;
        assert!(quiet.is_err(), "{quiet:?}");
        drop(requests);
        fake.await.expect("fake server");
    }
}
