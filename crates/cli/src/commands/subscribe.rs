//! `nowcast subscribe <brick> <kind> [params]`: one subscription, printed.

use std::io::Write;

use crate::client::Client;
use crate::error::CliError;
use crate::output;

pub async fn subscribe(
    client: &Client,
    brick: &str,
    kind: &str,
    params: &str,
    json: bool,
    out: &mut impl Write,
) -> Result<(), CliError> {
    let params = super::parse_params(params)?;
    let session = client.declare().await?;
    let subscribed = session.subscribe(brick, kind, &params).await?;
    output::subscribed(out, json, &subscribed.body)?;
    Ok(())
}
