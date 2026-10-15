use client::Client;
use protocol::{Request, Response, ServerMessage};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

#[derive(Debug)]
pub enum Incoming {
    Server(ServerMessage),
    Response(Response),
    Failed(String),
    Closed,
}

pub fn spawn(mut client: Client) -> (UnboundedSender<Request>, UnboundedReceiver<Incoming>) {
    let (requests_tx, mut requests) = mpsc::unbounded_channel::<Request>();
    let (incoming_tx, incoming) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break };
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
