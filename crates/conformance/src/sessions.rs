//! Contract section 3, the session: declared first, declared again on every
//! session change. Checked on the [`Sessions`] trait; `session_required` over
//! HTTP is in [`crate::api`].

use nowcast_traits::{AnchorId, Sessions};
use nowcast_types::SessionId;

/// Builds a fresh registry, with no session declared.
pub trait SessionsSubject {
    type Sessions: Sessions;
    fn new() -> Self::Sessions;
}

const A: AnchorId = AnchorId(1);
const B: AnchorId = AnchorId(2);

/// No session until the anchor declares one.
pub fn no_session_before_declaring<S: SessionsSubject>() {
    let sessions = S::new();
    assert_eq!(sessions.current(A), None);
}

/// A declared session is the anchor's, and only its.
pub fn declaring_sets_the_anchors_session<S: SessionsSubject>() {
    let sessions = S::new();
    sessions.declare(A, SessionId::new("one"));
    assert_eq!(sessions.current(A), Some(SessionId::new("one")));
    assert_eq!(sessions.current(B), None);
}

/// Declaring again on a session change (`/clear`, resume) replaces the session.
pub fn declaring_again_replaces_the_session<S: SessionsSubject>() {
    let sessions = S::new();
    sessions.declare(A, SessionId::new("one"));
    sessions.declare(A, SessionId::new("two"));
    assert_eq!(sessions.current(A), Some(SessionId::new("two")));
}

/// A declaration that must not replace sets a missing session and keeps an
/// existing one: a tool run inside an agent keeps the agent's session.
pub fn declaring_if_absent_keeps_the_session<S: SessionsSubject>() {
    let sessions = S::new();
    sessions.declare_if_absent(A, SessionId::new("one"));
    assert_eq!(sessions.current(A), Some(SessionId::new("one")));
    sessions.declare_if_absent(A, SessionId::new("two"));
    assert_eq!(sessions.current(A), Some(SessionId::new("one")));
    sessions.declare(A, SessionId::new("three"));
    assert_eq!(sessions.current(A), Some(SessionId::new("three")));
}

/// `GET /v1/status` lists every anchor's current session, by anchor.
pub fn the_declared_sessions_are_listed<S: SessionsSubject>() {
    let sessions = S::new();
    assert_eq!(sessions.declared(), vec![]);
    sessions.declare(B, SessionId::new("two"));
    sessions.declare(A, SessionId::new("one"));
    sessions.declare(A, SessionId::new("three"));
    assert_eq!(
        sessions.declared(),
        vec![(A, SessionId::new("three")), (B, SessionId::new("two"))]
    );
}

/// Runs the session rules of contract section 3.
#[macro_export]
macro_rules! sessions_suite {
    ($subject:ty) => {
        mod sessions_suite {
            #[allow(unused_imports)]
            use super::*;
            $crate::__tests!(
                sync,
                sessions,
                $subject,
                [
                    no_session_before_declaring,
                    declaring_sets_the_anchors_session,
                    declaring_again_replaces_the_session,
                    declaring_if_absent_keeps_the_session,
                    the_declared_sessions_are_listed,
                ]
            );
        }
    };
}
