//! `nowcast get <flow>`: the last value of a flow, in one call.

use std::io::Write;

use nowcast_types::QualifiedFlowId;

use crate::client::Client;
use crate::error::CliError;
use crate::output;

pub async fn get(
    client: &Client,
    flow: &str,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), CliError> {
    QualifiedFlowId::parse(flow)
        .map_err(|e| CliError::Usage(format!("invalid flow id {flow:?}: {e}")))?;
    let session = client.declare().await?;
    let event = session.last(flow).await?;
    if event.is_none() && !json {
        let _ = writeln!(err, "nowcast: {flow} has no value yet");
    }
    output::last_value(out, json, event.as_ref())?;
    Ok(())
}
