//! The compiled schemas of accepted types, so that an input, sent with every
//! measure of a session, does not compile its schema again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use jsonschema::Validator;
use nowcast_traits::BrickDeclaration;
use nowcast_types::BrickName;
use serde_json::Value;

use crate::error::HttpError;
use crate::params;

/// One compiled schema per brick and type, kept while the brick's declaration
/// is the one it was compiled from: discovery hands out a new declaration
/// when the file changes.
#[derive(Default)]
pub struct Validators(Mutex<HashMap<(BrickName, String), Cached>>);

struct Cached {
    declaration: Weak<BrickDeclaration>,
    validator: Arc<Validator>,
}

impl Validators {
    /// The compiled `schema` of type `ty` in `declaration`.
    pub(crate) fn get(
        &self,
        declaration: &Arc<BrickDeclaration>,
        ty: &str,
        schema: &Value,
    ) -> Result<Arc<Validator>, HttpError> {
        let key = (declaration.name.clone(), ty.to_owned());
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(&key)
            && cached
                .declaration
                .upgrade()
                .is_some_and(|held| Arc::ptr_eq(&held, declaration))
        {
            return Ok(Arc::clone(&cached.validator));
        }
        let validator = Arc::new(params::compile(
            schema,
            &format!("the schema of type {ty:?}"),
        )?);
        cache.insert(
            key,
            Cached {
                declaration: Arc::downgrade(declaration),
                validator: Arc::clone(&validator),
            },
        );
        Ok(validator)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn declaration() -> Arc<BrickDeclaration> {
        Arc::new(BrickDeclaration {
            name: BrickName::new("b").unwrap(),
            version: "0.1.0".into(),
            description: String::new(),
            protocol: 1,
            command: vec!["b".into()],
            dir: PathBuf::from("/"),
            env: Default::default(),
            configure: None,
            subscriptions: Default::default(),
            types: Default::default(),
        })
    }

    #[test]
    fn a_schema_compiles_once_per_declaration() {
        let validators = Validators::default();
        let schema = json!({ "type": "object" });
        let first = declaration();
        let a = validators.get(&first, "t", &schema).unwrap();
        let b = validators.get(&first, "t", &schema).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        // A changed declaration compiles its schema again.
        let second = declaration();
        let c = validators
            .get(&second, "t", &json!({ "type": "array" }))
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        assert!(c.is_valid(&json!([])));
    }
}
