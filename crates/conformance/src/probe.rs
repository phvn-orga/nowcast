//! The probe: the brick the API and supervisor suites subscribe to.
//!
//! It has one subscription kind, `item`, whose params are `{"id": <string>}`
//! and nothing else, and publishes `probe.item/v1`. A harness that runs a real
//! brick (the test brick) declares it with these schemas.

use std::collections::BTreeMap;

use nowcast_traits::{BrickDeclaration, SubscriptionKind, TypeDeclaration};
use nowcast_types::{BrickName, PROTOCOL_VERSION};
use serde_json::json;

pub const NAME: &str = "probe";
pub const KIND: &str = "item";
pub const TYPE: &str = "probe.item/v1";

pub fn name() -> BrickName {
    BrickName::new(NAME).unwrap()
}

/// The JSON Schema of the `item` params.
pub fn params_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["id"],
        "properties": { "id": { "type": "string" } },
        "additionalProperties": false
    })
}

/// The JSON Schema of the `probe.item/v1` detail.
pub fn type_schema() -> serde_json::Value {
    json!({ "type": "object" })
}

/// The probe as discovery would load it, launched by `command`.
pub fn declaration(command: Vec<String>) -> BrickDeclaration {
    BrickDeclaration {
        name: name(),
        version: "0.1.0".into(),
        description: "Publishes on demand, for conformance tests.".into(),
        protocol: PROTOCOL_VERSION,
        command,
        env: BTreeMap::new(),
        subscriptions: BTreeMap::from([(
            KIND.into(),
            SubscriptionKind {
                description: "One item, named by its id.".into(),
                schema: params_schema(),
                publishes: TYPE.into(),
            },
        )]),
        types: BTreeMap::from([(
            TYPE.into(),
            TypeDeclaration {
                schema: type_schema(),
            },
        )]),
    }
}

/// The params of the `item` subscription for `id`, as JSON bytes.
pub fn params(id: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "id": id })).unwrap()
}
