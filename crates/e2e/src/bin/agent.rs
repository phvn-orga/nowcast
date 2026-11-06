//! A stand-in for an agent whose hooks run the `nowcast` tool: the tests
//! declare a client kind matching this executable, so that the service
//! anchors the tool it runs on this process, as on an agent.
//!
//! Each line of standard input is a command, a JSON array of strings: the
//! program then its arguments. The agent runs it as its own child, waits for
//! it, and writes one JSON line on standard output:
//! `{"status": <exit code or null>, "stdout": <text>, "stderr": <text>}`.
//! It exits at the end of its input.

use std::io::{self, BufRead, Write};
use std::process::{Command, ExitCode, Stdio};

use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nowcast-e2e-agent: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let argv: Vec<String> = serde_json::from_str(&line?).map_err(io::Error::other)?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("an empty command"))?;
        let output = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()?;
        let result = json!({
            "status": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        });
        writeln!(out, "{result}")?;
        out.flush()?;
    }
    Ok(())
}
