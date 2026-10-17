use client::Client;
use protocol::{AgentId, DiffScope, GitStat, Request, Response, ServerMessage};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

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

/// Git requests go out without waiting, since the daemon may take a while to answer
/// them; their replies come back tagged with the agent. Every other request waits for
/// its reply before the next one is sent, so those replies arrive in request order.
pub fn spawn(mut client: Client) -> (UnboundedSender<Request>, UnboundedReceiver<Incoming>) {
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<Request>();
    let (incoming_tx, incoming) = mpsc::unbounded_channel();
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
                    match client.request(request).await {
                        Ok(Response::Ok) => {}
                        Ok(response) => {
                            if incoming_tx.send(Incoming::Response(response)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let message = format!("{e:#}");
                            let closed = message.contains("closed the connection");
                            let _ = incoming_tx.send(Incoming::Failed(message));
                            if closed {
                                let _ = incoming_tx.send(Incoming::Closed);
                                break;
                            }
                        }
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
}
