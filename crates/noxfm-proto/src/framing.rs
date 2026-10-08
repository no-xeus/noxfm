use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Refuse frames larger than this; a directory listing of ~100k entries fits.
const MAX_FRAME: u32 = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("codec: {0}")]
    Codec(#[from] postcard::Error),
    #[error("frame of {0} bytes exceeds limit")]
    TooLarge(u32),
}

pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let buf = postcard::to_stdvec(msg)?;
    let len = u32::try_from(buf.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Returns `Ok(None)` on clean EOF before a frame starts.
pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(Some(postcard::from_bytes(&buf)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[tokio::test]
    async fn round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = ClientMsg {
            id: 7,
            req: Request::Transfer {
                op: TransferOp::Move,
                sources: vec!["/a/b".into(), "/a/c".into()],
                dest: "/d".into(),
            },
        };
        write_frame(&mut a, &msg).await.unwrap();
        let got: ClientMsg = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!(got, msg);
        drop(a);
        assert!(read_frame::<_, ClientMsg>(&mut b).await.unwrap().is_none());
    }
}
