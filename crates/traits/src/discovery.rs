use std::collections::BTreeMap;
use std::sync::Arc;

use nowcast_types::{BrickName, ClientKindName};
use tokio::sync::broadcast;

/// A brick declaration file, validated and with its paths resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct BrickDeclaration {
    pub name: BrickName,
    pub version: String,
    pub description: String,
    /// The major version of the brick protocol the brick speaks.
    pub protocol: u32,
    /// The command to launch. A relative program path is already resolved from
    /// the real directory of the declaration file, link resolved.
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// By subscription kind.
    pub subscriptions: BTreeMap<String, SubscriptionKind>,
    /// By published type, for example `forge.pr/v1`.
    pub types: BTreeMap<String, TypeDeclaration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionKind {
    pub description: String,
    /// The JSON Schema the subscription params must match, loaded.
    pub schema: serde_json::Value,
    /// The type this kind's flows publish, declared in
    /// [`BrickDeclaration::types`].
    pub publishes: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDeclaration {
    /// The JSON Schema of the type's detail, loaded.
    pub schema: serde_json::Value,
}

/// A client kind declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientKind {
    pub name: ClientKindName,
    pub description: String,
    /// Glob patterns on the path of the anchor's executable. `*` also matches `/`.
    pub exe: Vec<String>,
}

/// A change in what discovery holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryChange {
    BrickAdded(BrickName),
    /// The brick is gone: its running instance stops and its flows end.
    BrickRemoved(BrickName),
    /// The declaration changed in place.
    BrickChanged(BrickName),
}

/// The loaded declarations of bricks and client kinds.
///
/// Discovery reads, validates and follows the declaration directories. Invalid
/// files and clashing names are left out (and logged) before anything reaches
/// this trait.
pub trait Discovery: Send + Sync + 'static {
    fn brick(&self, name: &BrickName) -> Option<Arc<BrickDeclaration>>;

    fn bricks(&self) -> Vec<Arc<BrickDeclaration>>;

    /// Embedded declarations first, replaced by user ones of the same name.
    fn client_kinds(&self) -> Vec<ClientKind>;

    /// Receives every change made after the call.
    fn changes(&self) -> broadcast::Receiver<DiscoveryChange>;
}
