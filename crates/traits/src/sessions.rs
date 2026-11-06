use nowcast_types::SessionId;

use crate::AnchorId;

/// The session each anchor declared.
///
/// The registry watches each declared anchor through
/// [`crate::Identity::exited`]; when it dies, the registry forgets its session
/// and calls [`crate::Router::remove_anchor`], in that order. So once an
/// anchor has been removed from the router, [`Sessions::current`] answers
/// `None` for it: a caller that subscribes an anchor can check `current`
/// afterwards and undo a subscription made after the anchor's removal.
pub trait Sessions: Send + Sync + 'static {
    /// Declares or replaces the session of `anchor`.
    fn declare(&self, anchor: AnchorId, session: SessionId);

    /// `None` until the anchor declares a session: any request but the
    /// declaration is then refused with `session_required`.
    fn current(&self, anchor: AnchorId) -> Option<SessionId>;
}
