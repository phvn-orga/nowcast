//! `nowcast refresh <flow>` and `nowcast refresh --brick <name>`: ask a brick
//! to read its source again now, for one flow, for all of them, or for what
//! a scope touches.

use nowcast_types::BrickName;
use serde_json::Value;

use crate::client::Client;
use crate::error::CliError;

pub async fn refresh(client: &Client, flow: &str) -> Result<(), CliError> {
    super::check_flow(flow)?;
    let session = client.declare().await?;
    session.refresh(flow).await
}

pub async fn refresh_brick(
    client: &Client,
    brick: &str,
    kind: Option<&str>,
    scope: Option<&str>,
) -> Result<(), CliError> {
    BrickName::new(brick)
        .map_err(|e| CliError::Usage(format!("invalid brick name {brick:?}: {e}")))?;
    let scope = scope
        .map(|scope| {
            serde_json::from_str::<Value>(scope)
                .map_err(|e| CliError::Usage(format!("the scope is not JSON: {e}")))
        })
        .transpose()?;
    let session = client.declare().await?;
    session.refresh_brick(brick, kind, scope).await
}
