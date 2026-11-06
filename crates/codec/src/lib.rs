//! The brick protocol codec (contract section 4): frames and messages, for
//! both ends of the pipe.
//!
//! A frame is a little-endian u32 length, then a MessagePack map of that many
//! bytes. Every map carries `v` (the protocol version), `t` (the message type)
//! and, on a request and its reply, `req`. The JSON of `params` and `detail`
//! travels as `bin`; this crate carries those bytes and never parses them.
//!
//! The service reads [`BrickMessage`]s and writes [`ServiceMessage`]s, the
//! message types of `nowcast-types`; a brick does the reverse. Each direction
//! has:
//!
//! - pure functions over whole frames, length prefix included:
//!   [`encode_service`], [`decode_service`], [`encode_brick`], [`decode_brick`];
//! - async functions over any tokio [`AsyncRead`](tokio::io::AsyncRead) or
//!   [`AsyncWrite`](tokio::io::AsyncWrite): [`read_service`], [`write_service`],
//!   [`read_brick`], [`write_brick`], and [`read_frame`] for a raw frame.
//!
//! Decoding is strict on what the contract fixes and lenient on the rest: a
//! wrong length, a body that is not one MessagePack map, a missing field, a
//! field of the wrong MessagePack type (JSON sent as a string rather than
//! `bin`, say), a duplicate key, another `v` or an unknown `t` are errors;
//! unknown keys are ignored. Frame bodies are capped at [`MAX_FRAME_LEN`] on
//! both sides. No input makes the codec panic.
//!
//! A brick answering `hello`, its standard input and output standing for the
//! stream:
//!
//! ```
//! use nowcast_codec::{read_brick, read_service, write_brick, write_service};
//! use nowcast_types::{BrickMessage, BrickName, PROTOCOL_VERSION, ServiceMessage};
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let (mut service, mut brick) = tokio::io::duplex(1024);
//! let hello = ServiceMessage::Hello {
//!     name: BrickName::new("forge").unwrap(),
//!     protocol: PROTOCOL_VERSION,
//! };
//! write_service(&mut service, &hello).await?;
//!
//! // The brick side.
//! assert_eq!(read_service(&mut brick).await?, Some(hello));
//! write_brick(&mut brick, &BrickMessage::Ready).await?;
//!
//! // The service side.
//! assert_eq!(read_brick(&mut service).await?, Some(BrickMessage::Ready));
//! # Ok::<_, nowcast_codec::CodecError>(())
//! # }).unwrap();
//! ```
//!
//! The pure functions work on bytes alone:
//!
//! ```
//! use nowcast_codec::{decode_brick, encode_brick};
//! use nowcast_types::{BrickMessage, RequestId};
//!
//! let publish = BrickMessage::Publish {
//!     req: RequestId(7),
//!     flow: "github.com/org/repo#main".try_into().unwrap(),
//!     data_type: "forge.pr/v1".into(),
//!     detail: br#"{"number":123}"#.to_vec(),
//! };
//! let frame = encode_brick(&publish)?;
//! assert_eq!(decode_brick(&frame)?, publish);
//! # Ok::<_, nowcast_codec::CodecError>(())
//! ```
//!
//! After [`CodecError::Io`], [`CodecError::Truncated`] or
//! [`CodecError::TooLarge`] the stream position is unknown: the reader should
//! stop and treat the peer as broken. Any other read error comes from a frame
//! read whole, so the next frame can still be read. Neither the read nor the
//! write functions are cancel safe: dropping one mid-frame loses or cuts a
//! frame, so read a stream from one task that owns it, and write it from one
//! task at a time.
//!
//! [`BrickMessage`]: nowcast_types::BrickMessage
//! [`ServiceMessage`]: nowcast_types::ServiceMessage

mod error;
mod frame;
mod message;
mod stream;

pub use error::CodecError;
pub use frame::{MAX_FRAME_LEN, PREFIX_LEN, read_frame, unframe};
pub use message::{decode_brick, decode_service, encode_brick, encode_service};
pub use stream::{read_brick, read_service, write_brick, write_service};
