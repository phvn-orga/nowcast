use std::fmt;

use nowcast_types::ClientKindName;

/// An anchor process, issued by [`crate::Identity`] and unique for the lifetime
/// of the service, even when the system reuses a pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorId(pub u64);

impl fmt::Display for AnchorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The process a connection is attached to: the nearest ancestor whose
/// executable matches a declared client kind, or the connected process itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub id: AnchorId,
    /// `None` when no ancestor matched and the connected process anchors itself.
    pub kind: Option<ClientKindName>,
}
