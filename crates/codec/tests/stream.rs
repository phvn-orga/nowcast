//! Reading and writing messages over an async byte stream.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use nowcast_codec::{
    CodecError, MAX_FRAME_LEN, encode_brick, read_brick, read_frame, read_service, write_brick,
    write_service,
};
use nowcast_conformance::codec::{brick_messages, service_messages};
use nowcast_types::BrickMessage;
use tokio::io::{AsyncRead, AsyncWriteExt, BufWriter, ReadBuf, duplex};

fn block_on<F: Future>(test: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(test)
}

#[test]
fn messages_cross_a_stream_in_both_directions() {
    block_on(async {
        let (mut service, mut brick) = duplex(64 * 1024);
        for msg in service_messages() {
            write_service(&mut service, &msg).await.unwrap();
        }
        for msg in brick_messages() {
            write_brick(&mut brick, &msg).await.unwrap();
        }
        for msg in service_messages() {
            assert_eq!(read_service(&mut brick).await.unwrap(), Some(msg));
        }
        for msg in brick_messages() {
            assert_eq!(read_brick(&mut service).await.unwrap(), Some(msg));
        }
    });
}

#[test]
fn a_clean_end_of_stream_reads_as_none() {
    block_on(async {
        let (mut service, brick) = duplex(1024);
        write_service(&mut service, &service_messages()[0])
            .await
            .unwrap();
        drop(service);
        let mut brick = brick;
        assert!(read_service(&mut brick).await.unwrap().is_some());
        assert!(read_service(&mut brick).await.unwrap().is_none());
        assert!(read_frame(&mut &[][..]).await.unwrap().is_none());
    });
}

#[test]
fn frames_arriving_one_byte_at_a_time_are_reassembled() {
    block_on(async {
        // A one-byte pipe: every read returns a single byte.
        let (mut brick, mut service) = duplex(1);
        let writer = async {
            for msg in brick_messages() {
                write_brick(&mut brick, &msg).await.unwrap();
            }
            drop(brick);
        };
        let reader = async {
            let mut read = Vec::new();
            while let Some(msg) = read_brick(&mut service).await.unwrap() {
                read.push(msg);
            }
            read
        };
        let ((), read) = tokio::join!(writer, reader);
        assert_eq!(read, brick_messages());
    });
}

#[test]
fn a_stream_ending_inside_the_length_is_truncated() {
    block_on(async {
        let mut input = &[5u8, 0][..];
        assert!(matches!(
            read_frame(&mut input).await,
            Err(CodecError::Truncated {
                expected: 4,
                received: 2
            })
        ));
    });
}

#[test]
fn a_stream_ending_inside_the_body_is_truncated() {
    block_on(async {
        let frame = encode_brick(&brick_messages()[3]).unwrap();
        let mut input = &frame[..frame.len() - 1];
        assert!(matches!(
            read_brick(&mut input).await,
            Err(CodecError::Truncated { expected, received })
                if expected == frame.len() && received == frame.len() - 1
        ));
    });
}

#[test]
fn an_oversized_length_is_refused_before_reading_the_body() {
    block_on(async {
        // Only the length: reading on would report a truncation instead.
        let len = u32::try_from(MAX_FRAME_LEN + 1).unwrap().to_le_bytes();
        let mut input = &len[..];
        assert!(matches!(
            read_frame(&mut input).await,
            Err(CodecError::TooLarge { .. })
        ));
    });
}

#[test]
fn a_frame_at_the_size_limit_is_read() {
    block_on(async {
        let mut frame = u32::try_from(MAX_FRAME_LEN).unwrap().to_le_bytes().to_vec();
        frame.resize(4 + MAX_FRAME_LEN, 0);
        let read = read_frame(&mut &frame[..]).await.unwrap().unwrap();
        assert_eq!(read.len(), frame.len());
    });
}

#[test]
fn a_malformed_frame_is_an_error_not_a_panic() {
    block_on(async {
        // A frame holding the MessagePack integer 1.
        let mut input = &[1u8, 0, 0, 0, 0x01][..];
        assert!(matches!(
            read_brick(&mut input).await,
            Err(CodecError::Malformed(_))
        ));
    });
}

#[test]
fn a_write_reaches_the_peer_without_an_explicit_flush() {
    block_on(async {
        let (brick, mut service) = duplex(1024);
        let mut brick = BufWriter::new(brick);
        write_brick(&mut brick, &BrickMessage::Ready).await.unwrap();
        assert_eq!(
            read_brick(&mut service).await.unwrap(),
            Some(BrickMessage::Ready)
        );
    });
}

#[test]
fn an_oversized_message_writes_nothing() {
    block_on(async {
        let (mut brick, mut service) = duplex(1024);
        let publish = BrickMessage::Publish {
            req: nowcast_types::RequestId(1),
            flow: "f".try_into().unwrap(),
            data_type: "t/v1".into(),
            detail: vec![0; MAX_FRAME_LEN],
        };
        assert!(matches!(
            write_brick(&mut brick, &publish).await,
            Err(CodecError::TooLarge { .. })
        ));
        brick.shutdown().await.unwrap();
        assert!(read_brick(&mut service).await.unwrap().is_none());
    });
}

/// A stream whose every read fails.
struct Broken;

impl AsyncRead for Broken {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "gone")))
    }
}

#[test]
fn a_stream_failure_is_reported_as_io() {
    block_on(async {
        let error = read_service(&mut Broken).await.unwrap_err();
        assert!(
            matches!(&error, CodecError::Io(e) if e.kind() == io::ErrorKind::BrokenPipe),
            "{error}"
        );
        assert!(std::error::Error::source(&error).is_some());
    });
}
