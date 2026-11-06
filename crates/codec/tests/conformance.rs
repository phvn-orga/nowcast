//! The frame rules of contract section 4, from the conformance crate.

use nowcast_conformance::codec::CodecSubject;
use nowcast_types::{BrickMessage, ServiceMessage};

struct Codec;

impl CodecSubject for Codec {
    fn encode_service(msg: &ServiceMessage) -> Vec<u8> {
        nowcast_codec::encode_service(msg).expect("a suite message fits a frame")
    }

    fn decode_service(frame: &[u8]) -> Result<ServiceMessage, String> {
        nowcast_codec::decode_service(frame).map_err(|e| e.to_string())
    }

    fn encode_brick(msg: &BrickMessage) -> Vec<u8> {
        nowcast_codec::encode_brick(msg).expect("a suite message fits a frame")
    }

    fn decode_brick(frame: &[u8]) -> Result<BrickMessage, String> {
        nowcast_codec::decode_brick(frame).map_err(|e| e.to_string())
    }
}

nowcast_conformance::codec_suite!(Codec);
