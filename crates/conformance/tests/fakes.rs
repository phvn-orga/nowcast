//! The fakes and the reference codec pass the suites that apply to them.

use nowcast_conformance::reference;
use nowcast_conformance::{codec::CodecSubject, router::RouterSubject, sessions::SessionsSubject};
use nowcast_traits::fake::{FakeRouter, FakeSessions};
use nowcast_types::{BrickMessage, ServiceMessage};

struct Fake;

impl RouterSubject for Fake {
    type Router = FakeRouter;
    fn new() -> FakeRouter {
        FakeRouter::new()
    }
}

impl SessionsSubject for Fake {
    type Sessions = FakeSessions;
    fn new() -> FakeSessions {
        FakeSessions::new()
    }
}

struct Reference;

impl CodecSubject for Reference {
    fn encode_service(msg: &ServiceMessage) -> Vec<u8> {
        reference::encode_service(msg)
    }
    fn decode_service(frame: &[u8]) -> Result<ServiceMessage, String> {
        reference::decode_service(frame)
    }
    fn encode_brick(msg: &BrickMessage) -> Vec<u8> {
        reference::encode_brick(msg)
    }
    fn decode_brick(frame: &[u8]) -> Result<BrickMessage, String> {
        reference::decode_brick(frame)
    }
}

nowcast_conformance::router_suite!(Fake);
nowcast_conformance::sessions_suite!(Fake);
nowcast_conformance::codec_suite!(Reference);
