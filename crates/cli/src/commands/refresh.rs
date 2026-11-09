//! `nowcast refresh <flow>` and `nowcast refresh --brick <name>`: ask a brick
//! to read its source again now, for one flow or for all of them.

use nowcast_types::BrickName;

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
) -> Result<(), CliError> {
    BrickName::new(brick)
        .map_err(|e| CliError::Usage(format!("invalid brick name {brick:?}: {e}")))?;
    let session = client.declare().await?;
    session.refresh_brick(brick, kind).await
}
