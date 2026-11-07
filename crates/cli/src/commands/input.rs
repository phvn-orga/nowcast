//! `nowcast input <flow>`: sends the JSON read on stdin to the flow's brick.

use std::io::Read;

use serde_json::Value;

use crate::client::Client;
use crate::error::CliError;

pub async fn input(client: &Client, flow: &str, stdin: impl Read) -> Result<(), CliError> {
    super::check_flow(flow)?;
    let detail = read_detail(stdin)?;
    let session = client.declare().await?;
    session.input(flow, detail).await
}

/// The detail, one JSON value read whole from `stdin`.
fn read_detail(mut stdin: impl Read) -> Result<Value, CliError> {
    let mut text = String::new();
    stdin
        .read_to_string(&mut text)
        .map_err(|e| CliError::Usage(format!("cannot read stdin: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| CliError::Usage(format!("stdin is not one JSON value ({e})")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_detail_is_one_json_value() {
        assert_eq!(
            read_detail(&b"{\"n\": 1}\n"[..]).unwrap(),
            json!({ "n": 1 })
        );
        for bad in ["", "{", "{} {}"] {
            assert!(
                matches!(read_detail(bad.as_bytes()), Err(CliError::Usage(_))),
                "{bad:?}"
            );
        }
    }
}
