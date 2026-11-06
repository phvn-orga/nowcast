//! The router passes the router rules of contract section 3.

use nowcast_conformance::router::RouterSubject;
use nowcast_router::FlowRouter;

struct Subject;

impl RouterSubject for Subject {
    type Router = FlowRouter;
    fn new() -> FlowRouter {
        FlowRouter::new()
    }
}

nowcast_conformance::router_suite!(Subject);
