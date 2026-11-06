//! The frame: a little-endian u32 length, then that many bytes of body.

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::CodecError;

/// Size of the length prefix, in bytes.
pub const PREFIX_LEN: usize = 4;

/// The largest frame body accepted or produced, in bytes (16 MiB).
///
/// The contract sets no limit. This one is far above any realistic message,
/// and bounds the memory a misbehaving peer can make the reader hold.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Checks a body length against [`MAX_FRAME_LEN`] and returns its prefix.
pub(crate) fn prefix(len: usize) -> Result<[u8; PREFIX_LEN], CodecError> {
    let too_large = CodecError::TooLarge {
        len,
        max: MAX_FRAME_LEN,
    };
    if len > MAX_FRAME_LEN {
        return Err(too_large);
    }
    let len = u32::try_from(len).map_err(|_| too_large)?;
    Ok(len.to_le_bytes())
}

/// Reads the announced body length of a prefix, refusing one above
/// [`MAX_FRAME_LEN`].
fn body_len(prefix: [u8; PREFIX_LEN]) -> Result<usize, CodecError> {
    let len = u32::from_le_bytes(prefix) as usize;
    if len > MAX_FRAME_LEN {
        return Err(CodecError::TooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }
    Ok(len)
}

/// Splits a whole frame into its body, checking that the length prefix counts
/// exactly the bytes that follow.
pub fn unframe(frame: &[u8]) -> Result<&[u8], CodecError> {
    let Some((prefix, body)) = frame.split_first_chunk::<PREFIX_LEN>() else {
        return Err(CodecError::Truncated {
            expected: PREFIX_LEN,
            received: frame.len(),
        });
    };
    let declared = body_len(*prefix)?;
    if declared != body.len() {
        return Err(CodecError::LengthMismatch {
            declared,
            actual: body.len(),
        });
    }
    Ok(body)
}

/// Reads one whole frame, length prefix included, from a stream.
///
/// Returns `Ok(None)` when the stream ends cleanly before a new frame, and
/// [`CodecError::Truncated`] when it ends inside one. A length above
/// [`MAX_FRAME_LEN`] is refused before its body is read. The buffer grows
/// with the bytes actually received, so a bogus length allocates nothing.
///
/// After an error the stream position is unknown: stop reading it. Not cancel
/// safe: dropping the future mid-frame loses the bytes already read, so read
/// a stream from one task that owns it.
///
/// The message readers built on it add decoding errors, which come after the
/// whole frame was read and so leave the stream at the next frame.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, CodecError>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut prefix = [0u8; PREFIX_LEN];
    let mut filled = 0;
    while filled < PREFIX_LEN {
        match reader.read(&mut prefix[filled..]).await? {
            0 if filled == 0 => return Ok(None),
            0 => {
                return Err(CodecError::Truncated {
                    expected: PREFIX_LEN,
                    received: filled,
                });
            }
            n => filled += n,
        }
    }
    let len = body_len(prefix)?;
    let mut frame = prefix.to_vec();
    (&mut *reader)
        .take(len as u64)
        .read_to_end(&mut frame)
        .await?;
    let received = frame.len() - PREFIX_LEN;
    if received != len {
        return Err(CodecError::Truncated {
            expected: PREFIX_LEN + len,
            received: frame.len(),
        });
    }
    Ok(Some(frame))
}
