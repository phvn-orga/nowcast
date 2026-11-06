use nowcast_types::SessionId;

use crate::AnchorId;

/// The session each anchor declared.
///
/// The registry watches each declared anchor through
/// [`crate::Identity::exited`]; when it dies, the registry forgets its session
/// and calls [`crate::Router::remove_anchor`].
pub trait Sessions: Send + Sync + 'static {
    /// Declares or replaces the session of `anchor`.
    fn declare(&self, anchor: AnchorId, session: SessionId);

    /// `None` until the anchor declares a session: any request but the
    /// declaration is then refused with `session_required`.
    fn current(&self, anchor: AnchorId) -> Option<SessionId>;
}
