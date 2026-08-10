use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Control frames are length-prefixed JSON: a 4-byte big-endian length followed
/// by exactly that many bytes of UTF-8 JSON. The cap bounds a single message so a
/// peer can't advertise a huge length and force an unbounded allocation off the
/// wire; it is small on purpose because these frames only ever carry handshake
/// structs, never relayed traffic.
pub const MAX_FRAME_BYTES: usize = 16 * 1_024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the frame size limit")]
    Oversize(usize),
    #[error("received an empty frame")]
    Empty,
    #[error("frame transport failed")]
    Io(#[from] std::io::Error),
    #[error("encode frame")]
    Encode(#[source] serde_json::Error),
    #[error("decode frame")]
    Decode(#[source] serde_json::Error),
}

pub async fn write_frame<T, S>(stream: &mut S, value: &T) -> Result<(), FrameError>
where
    T: Serialize,
    S: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(value).map_err(FrameError::Encode)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::Oversize(payload.len()));
    }
    stream.write_u32(payload.len() as u32).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read_frame<T, S>(stream: &mut S) -> Result<T, FrameError>
where
    T: DeserializeOwned,
    S: AsyncRead + Unpin,
{
    let length = stream.read_u32().await? as usize;
    if length == 0 {
        return Err(FrameError::Empty);
    }
    // Reject on the advertised length alone, before allocating the buffer.
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::Oversize(length));
    }
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(FrameError::Decode)
}
