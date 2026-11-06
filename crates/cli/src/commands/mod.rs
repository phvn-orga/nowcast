//! The commands. Each one checks its arguments, then declares the session
//! before its first call.

mod get;
mod subscribe;
mod watch;

pub use get::get;
pub use subscribe::subscribe;
pub use watch::watch;

use serde_json::Value;

use crate::error::CliError;

/// The content of a subscription, given as JSON on the command line.
fn parse_params(params: &str) -> Result<Value, CliError> {
    serde_json::from_str(params)
        .map_err(|e| CliError::Usage(format!("PARAMS is not JSON ({e}): {params}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn params_are_json() {
        assert_eq!(parse_params(r#"{"a": 1}"#).unwrap(), json!({ "a": 1 }));
        for bad in ["", "main", "{a: 1}", "{} {}"] {
            assert!(
                matches!(parse_params(bad), Err(CliError::Usage(_))),
                "{bad:?}"
            );
        }
    }
}
