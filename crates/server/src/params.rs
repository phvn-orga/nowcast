//! Subscription params and flow inputs, checked against their JSON Schema.

use nowcast_types::ApiErrorCode;
use serde_json::Value;

use crate::error::HttpError;

/// How many schema violations the `400` message lists.
const MAX_REPORTED: usize = 5;

/// `400` `invalid_params` when `params` do not match `schema`.
///
/// The schema comes from a declaration discovery already validated; one that
/// still does not compile is a fault of the service, answered with `500`.
/// External references are never fetched, whatever features Cargo enables on
/// `jsonschema` across the workspace: a request never waits on the network or
/// the disk.
pub(crate) fn validate(schema: &Value, params: &Value, kind: &str) -> Result<(), HttpError> {
    check(
        schema,
        params,
        ApiErrorCode::InvalidParams,
        &format!("the params schema of kind {kind:?}"),
        &format!("params do not match the schema of kind {kind:?}"),
    )
}

/// `400` `invalid_input` when `input` does not match `schema`, the schema of
/// the accepted type `ty`. The schema is handled as in [`validate`].
pub(crate) fn validate_input(schema: &Value, input: &Value, ty: &str) -> Result<(), HttpError> {
    check(
        schema,
        input,
        ApiErrorCode::InvalidInput,
        &format!("the schema of type {ty:?}"),
        &format!("the input does not match the schema of type {ty:?}"),
    )
}

/// `code` with `mismatch` and the first violations when `value` does not
/// match `schema`, named `what` in the `500` of a schema that does not compile.
fn check(
    schema: &Value,
    value: &Value,
    code: ApiErrorCode,
    what: &str,
    mismatch: &str,
) -> Result<(), HttpError> {
    let validator = jsonschema::options().offline().build(schema).map_err(|e| {
        tracing::warn!("{what} does not compile: {e}");
        HttpError::internal(format!("{what} is invalid"))
    })?;
    let mut errors = validator.iter_errors(value).peekable();
    if errors.peek().is_none() {
        return Ok(());
    }
    let reported: Vec<String> = errors
        .take(MAX_REPORTED)
        .map(|e| match e.instance_path().as_str() {
            "" => e.to_string(),
            path => format!("{path}: {e}"),
        })
        .collect();
    Err(HttpError::api(
        code,
        format!("{mismatch}: {}", reported.join("; ")),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "required": ["remote", "branch"],
            "properties": {
                "remote": { "type": "string" },
                "branch": { "type": "string" }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn matching_params_pass() {
        let params = json!({ "remote": "git@github.com:org/repo.git", "branch": "main" });
        assert_eq!(validate(&schema(), &params, "pr"), Ok(()));
    }

    #[test]
    fn mismatching_params_answer_400_with_where_and_why() {
        let error =
            validate(&schema(), &json!({ "remote": 1, "branch": "main" }), "pr").unwrap_err();
        assert_eq!((error.status.as_u16(), error.code), (400, "invalid_params"));
        assert!(error.message.contains("/remote"), "{}", error.message);
        for params in [
            json!(null),
            json!({}),
            json!({ "remote": "r", "branch": "b", "x": 1 }),
        ] {
            let error = validate(&schema(), &params, "pr").unwrap_err();
            assert_eq!(error.code, "invalid_params", "{params}");
        }
    }

    #[test]
    fn the_message_lists_a_bounded_number_of_violations() {
        let schema = json!({ "type": "array", "items": { "type": "string" } });
        let error = validate(&schema, &json!([1, 2, 3, 4, 5, 6, 7]), "k").unwrap_err();
        assert_eq!(error.message.matches("; ").count(), MAX_REPORTED - 1);
    }

    #[test]
    fn a_schema_that_does_not_compile_is_an_internal_error() {
        let error = validate(&json!({ "type": 12 }), &json!({}), "k").unwrap_err();
        assert_eq!((error.status.as_u16(), error.code), (500, "internal_error"));
    }

    #[test]
    fn remote_references_are_not_fetched() {
        let schema = json!({ "$ref": "https://example.com/schema.json" });
        let error = validate(&schema, &json!({}), "k").unwrap_err();
        assert_eq!(error.code, "internal_error");
    }
}
