//! `nowcast get <flow>`: the last value of a flow, in one call. With
//! `--after <id>`, the flow's kept history after that id.

use std::io::Write;

use crate::client::Client;
use crate::error::CliError;
use crate::output;

pub async fn get(
    client: &Client,
    flow: &str,
    after: Option<u64>,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), CliError> {
    super::check_flow(flow)?;
    let session = client.declare().await?;
    if let Some(after) = after {
        let history = session.history(flow, after).await?;
        output::history(out, json, flow, &history)?;
        return Ok(());
    }
    let event = session.last(flow).await?;
    if event.is_none() && !json {
        let _ = writeln!(err, "nowcast: {flow} has no value yet");
    }
    output::last_value(out, json, event.as_ref())?;
    Ok(())
}
