//! Messages over an async byte stream: one frame per message.

use nowcast_types::{BrickMessage, ServiceMessage};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{CodecError, decode_brick, decode_service, encode_brick, encode_service, read_frame};

/// Reads the next service message, `Ok(None)` at a clean end of stream. The
/// brick side reads its standard input with it.
///
/// Same stream rules as [`read_frame`].
pub async fn read_service<R>(reader: &mut R) -> Result<Option<ServiceMessage>, CodecError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    match read_frame(reader).await? {
        Some(frame) => decode_service(&frame).map(Some),
        None => Ok(None),
    }
}

/// Reads the next brick message, `Ok(None)` at a clean end of stream. The
/// service side reads a brick's standard output with it.
///
/// Same stream rules as [`read_frame`].
pub async fn read_brick<R>(reader: &mut R) -> Result<Option<BrickMessage>, CodecError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    match read_frame(reader).await? {
        Some(frame) => decode_brick(&frame).map(Some),
        None => Ok(None),
    }
}

/// Writes a service message as one frame, then flushes. The service side
/// writes a brick's standard input with it.
pub async fn write_service<W>(writer: &mut W, msg: &ServiceMessage) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_frame(writer, &encode_service(msg)?).await
}

/// Writes a brick message as one frame, then flushes. The brick side writes
/// its standard output with it.
pub async fn write_brick<W>(writer: &mut W, msg: &BrickMessage) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    write_frame(writer, &encode_brick(msg)?).await
}

/// Writes the whole frame, then flushes so that the peer sees it now even
/// behind a buffered writer. Not cancel safe: dropping the future mid-write
/// leaves a partial frame on the stream.
async fn write_frame<W>(writer: &mut W, frame: &[u8]) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    writer.write_all(frame).await?;
    writer.flush().await?;
    Ok(())
}
