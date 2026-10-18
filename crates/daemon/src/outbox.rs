//! A client's bounded queue of outgoing messages. Frames beyond a few are dropped, since
//! the next one that goes through can carry the whole screen; any other message that does
//! not fit marks the client for disconnection, so nothing else is ever lost unnoticed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use protocol::{Frame, ServerMessage, write_message};
use tokio::io::AsyncWrite;
use tokio::sync::mpsc::{self, Receiver, Sender, error::TrySendError};

/// Frames a client may have queued or being written.
pub const QUEUED_FRAMES: usize = 4;
/// Other messages a client may have queued.
pub const QUEUED_MESSAGES: usize = 16384;

/// The daemon's end of a client's queue.
#[derive(Clone)]
pub struct Outbox {
    tx: Sender<ServerMessage>,
    frames: Arc<AtomicUsize>,
    overflowed: Arc<AtomicBool>,
}

/// The client's writer's end of the queue.
pub struct Outgoing {
    rx: Receiver<ServerMessage>,
    frames: Arc<AtomicUsize>,
}

pub fn outbox() -> (Outbox, Outgoing) {
    let (tx, rx) = mpsc::channel(QUEUED_FRAMES + QUEUED_MESSAGES);
    let frames = Arc::new(AtomicUsize::new(0));
    let outbox = Outbox {
        tx,
        frames: Arc::clone(&frames),
        overflowed: Arc::new(AtomicBool::new(false)),
    };
    (outbox, Outgoing { rx, frames })
}

impl Outbox {
    /// Queues a message that must arrive; when the queue is full the client is marked as
    /// overflowed instead.
    pub fn send(&self, message: ServerMessage) {
        if let Err(TrySendError::Full(_)) = self.tx.try_send(message) {
            self.overflowed.store(true, Ordering::Relaxed);
        }
    }

    /// Queues the frame unless `QUEUED_FRAMES` are queued already; false when it was
    /// dropped, after which the client needs a full frame.
    pub fn send_frame(&self, frame: Frame) -> bool {
        if self.frames.fetch_add(1, Ordering::AcqRel) >= QUEUED_FRAMES {
            self.frames.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        if self.tx.try_send(ServerMessage::Frame(frame)).is_err() {
            self.frames.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// Whether a message that must arrive did not fit.
    pub fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Relaxed)
    }
}

impl Outgoing {
    #[cfg(test)]
    pub fn try_recv(&mut self) -> Option<ServerMessage> {
        self.rx.try_recv().ok()
    }

    /// Writes the queued messages until the queue closes or a write fails.
    pub async fn write_to<W: AsyncWrite + Unpin>(mut self, write: &mut W) {
        while let Some(message) = self.rx.recv().await {
            let frame = matches!(message, ServerMessage::Frame(_));
            let written = write_message(write, &message).await;
            if frame {
                self.frames.fetch_sub(1, Ordering::AcqRel);
            }
            if written.is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use protocol::{AgentId, Screen, read_message};

    use super::*;

    fn frame() -> Frame {
        Screen::blank(4, 2)
            .diff(None, AgentId::random())
            .expect("a first frame")
    }

    #[test]
    fn frames_beyond_the_limit_are_dropped_until_the_writer_takes_some() {
        let (outbox, mut outgoing) = outbox();
        for _ in 0..QUEUED_FRAMES {
            assert!(outbox.send_frame(frame()));
        }
        assert!(!outbox.send_frame(frame()));
        assert!(!outbox.overflowed());
        let _ = outgoing.rx.try_recv().expect("queued");
        outgoing.frames.fetch_sub(1, Ordering::AcqRel);
        assert!(outbox.send_frame(frame()));
    }

    #[test]
    fn queued_frames_leave_room_for_every_other_message() {
        let (outbox, _outgoing) = outbox();
        for _ in 0..QUEUED_FRAMES {
            assert!(outbox.send_frame(frame()));
        }
        for _ in 0..QUEUED_MESSAGES {
            outbox.send(ServerMessage::AgentBell(AgentId::random()));
        }
        assert!(!outbox.overflowed());
        outbox.send(ServerMessage::AgentBell(AgentId::random()));
        assert!(outbox.overflowed());
    }

    #[tokio::test]
    async fn the_writer_frees_a_frames_place_once_written() {
        let (outbox, outgoing) = outbox();
        let (mut ours, mut theirs) = tokio::io::duplex(1 << 20);
        for _ in 0..QUEUED_FRAMES {
            assert!(outbox.send_frame(frame()));
        }
        let writer = tokio::spawn(async move { outgoing.write_to(&mut ours).await });
        for _ in 0..QUEUED_FRAMES {
            let message: ServerMessage = read_message(&mut theirs)
                .await
                .expect("read")
                .expect("a frame");
            assert!(matches!(message, ServerMessage::Frame(_)));
        }
        while !outbox.send_frame(frame()) {
            tokio::task::yield_now().await;
        }
        drop(outbox);
        writer.await.expect("writer");
    }
}
