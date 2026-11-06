//! `nowcast refresh <flow>`: asks the brick to read its source again now.

use crate::client::Client;
use crate::error::CliError;

pub async fn refresh(client: &Client, flow: &str) -> Result<(), CliError> {
    super::check_flow(flow)?;
    let session = client.declare().await?;
    session.refresh(flow).await
}
