use std::io;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

pub async fn write_message<W, T>(writer: &mut W, message: &T) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes =
        postcard::to_stdvec(message).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|&len| len as usize <= MAX_MESSAGE_LEN)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "message too large"))?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

pub async fn read_message<R, T>(reader: &mut R) -> io::Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0; len];
    reader.read_exact(&mut buf).await?;
    postcard::from_bytes(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMessage, Request};

    #[tokio::test]
    async fn messages_round_trip_over_a_stream() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let sent = ClientMessage::Request {
            id: 9,
            request: Request::ListAgents,
        };
        write_message(&mut a, &sent).await.expect("write");
        write_message(
            &mut a,
            &ClientMessage::Hello {
                protocol_version: 1,
            },
        )
        .await
        .expect("write");
        let first: Option<ClientMessage> = read_message(&mut b).await.expect("read");
        let second: Option<ClientMessage> = read_message(&mut b).await.expect("read");
        assert_eq!(first, Some(sent));
        assert_eq!(
            second,
            Some(ClientMessage::Hello {
                protocol_version: 1
            })
        );
    }

    #[tokio::test]
    async fn clean_eof_is_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        let read: Option<ClientMessage> = read_message(&mut b).await.expect("read");
        assert_eq!(read, None);
    }

    #[tokio::test]
    async fn oversized_length_is_rejected() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_be_bytes()).await.expect("write");
        let err = read_message::<_, ClientMessage>(&mut b)
            .await
            .expect_err("too large");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn garbage_payload_is_invalid_data() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&3u32.to_be_bytes()).await.expect("write");
        a.write_all(&[0xff, 0xff, 0xff]).await.expect("write");
        let err = read_message::<_, ClientMessage>(&mut b)
            .await
            .expect_err("garbage");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
