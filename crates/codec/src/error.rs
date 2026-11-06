use std::{fmt, io};

/// Why a frame could not be read, written or decoded.
#[derive(Debug)]
pub enum CodecError {
    /// The underlying stream failed.
    Io(io::Error),
    /// The input ended inside a frame: within its 4-byte length, or before the
    /// number of bytes the length announces.
    Truncated { expected: usize, received: usize },
    /// A frame given whole holds a different number of bytes than its length
    /// announces.
    LengthMismatch { declared: usize, actual: usize },
    /// The frame body exceeds [`MAX_FRAME_LEN`](crate::MAX_FRAME_LEN) bytes.
    TooLarge { len: usize, max: usize },
    /// The `v` key names a protocol version this codec does not speak.
    UnsupportedVersion(u64),
    /// The `t` key names no message of this direction.
    UnknownType(String),
    /// The body is not one MessagePack map holding the message's fields with
    /// their contract types.
    Malformed(String),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "brick stream: {e}"),
            Self::Truncated { expected, received } => write!(
                f,
                "truncated frame: expected {expected} bytes, the input ended after {received}"
            ),
            Self::LengthMismatch { declared, actual } => {
                write!(f, "the frame length says {declared} bytes, {actual} follow")
            }
            Self::TooLarge { len, max } => {
                write!(
                    f,
                    "a frame body of {len} bytes exceeds the {max}-byte limit"
                )
            }
            Self::UnsupportedVersion(v) => write!(
                f,
                "protocol version {v} is not supported, expected {}",
                nowcast_types::PROTOCOL_VERSION
            ),
            Self::UnknownType(t) => write!(f, "unknown message type {t:?}"),
            Self::Malformed(reason) => write!(f, "malformed message: {reason}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for CodecError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
