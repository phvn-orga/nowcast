//! The fake supervisor keeps the drop rule of `Supervisor::resolve`.

use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use nowcast_traits::fake::{FakeRouter, FakeSupervisor};
use nowcast_traits::{AnchorId, Router, Supervisor};
use nowcast_types::{BrickName, FlowId, QualifiedFlowId};

fn world() -> (Arc<FakeRouter>, FakeSupervisor, BrickName, QualifiedFlowId) {
    let router = Arc::new(FakeRouter::new());
    let supervisor = FakeSupervisor::new(router.clone());
    let brick = BrickName::new("probe").unwrap();
    let flow = QualifiedFlowId::new(brick.clone(), FlowId::new("item:{}").unwrap());
    (router, supervisor, brick, flow)
}

#[test]
fn a_resolution_dropped_unpolled_subscribes_nobody() {
    let (router, supervisor, brick, flow) = world();
    drop(supervisor.resolve(AnchorId(1), &brick, "item", b"{}"));
    assert_eq!(router.subscribers(&flow), 0);
}

#[test]
fn a_polled_resolution_subscribes_the_anchor() {
    let (router, supervisor, brick, flow) = world();
    let resolving = pin!(supervisor.resolve(AnchorId(1), &brick, "item", b"{}"));
    let Poll::Ready(Ok(resolved)) = resolving.poll(&mut Context::from_waker(Waker::noop())) else {
        panic!("the fake resolves at once");
    };
    assert_eq!(resolved.flow, flow);
    assert_eq!(router.subscribers(&flow), 1);
}
