use client::Client;
use protocol::{AgentId, GitStat, Request, Response, ServerMessage};
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
                    if let Request::GitStat { id } | Request::GitDiff { id } = request {
                        let diff = matches!(request, Request::GitDiff { .. });
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

fn git_reply(id: AgentId, diff: bool, result: Result<Response, String>) -> Incoming {
    let unexpected = |other: Response| format!("unexpected reply {other:?}");
    if diff {
        let result = result.and_then(|response| match response {
            Response::Diff(text) => Ok(text),
            other => Err(unexpected(other)),
        });
        Incoming::Diff { id, result }
    } else {
        let result = result.and_then(|response| match response {
            Response::GitStat(stat) => Ok(stat),
            other => Err(unexpected(other)),
        });
        Incoming::GitStat { id, result }
    }
}
