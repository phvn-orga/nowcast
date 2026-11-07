//! The sources outside the brick: Claude Code's OAuth token, the usage API
//! it reads on `/usage`, and Claude Code's copy of its answer. None of them
//! is documented: a format the brick does not recognize reads as
//! `unreadable`, never as a crash.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use jiff::Timestamp;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, RETRY_AFTER, USER_AGENT};
use serde_json::Value;

use crate::quotas::cached_copy;

/// The usage API's address, `/usage`'s in Claude Code 2.1.319.
pub const API: &str = "https://api.anthropic.com";
/// Replaces [`API`], for tests.
pub const API_VAR: &str = "NOWCAST_CLAUDE_CODE_API";
const USAGE_PATH: &str = "/api/oauth/usage";
/// The beta header Claude Code sends with its OAuth token.
const OAUTH_BETA: &str = "oauth-2025-04-20";
const TIMEOUT: Duration = Duration::from_secs(20);

/// Where Claude Code keeps its files, under the user's home.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.claude/.credentials.json`, which holds the OAuth token.
    pub credentials: PathBuf,
    /// `~/.claude.json`, which holds the copy of the last usage answer.
    pub claude_json: PathBuf,
}

impl Paths {
    pub fn under(home: &Path) -> Self {
        Self {
            credentials: home.join(".claude/.credentials.json"),
            claude_json: home.join(".claude.json"),
        }
    }
}

/// The OAuth token, as Claude Code left it.
#[derive(Debug, PartialEq, Eq)]
pub enum Token {
    Valid(String),
    Expired,
    Missing,
    Unreadable,
}

/// Reads the token. The brick never refreshes it: that would write Claude
/// Code's file, which Claude Code rewrites itself whenever it refreshes.
pub fn read_token(path: &Path, now: Timestamp) -> Token {
    let text = match std::fs::read(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Token::Missing,
        Err(_) => return Token::Unreadable,
    };
    let Ok(json) = serde_json::from_slice::<Value>(&text) else {
        return Token::Unreadable;
    };
    let oauth = &json["claudeAiOauth"];
    let (Some(token), Some(expires)) = (oauth["accessToken"].as_str(), oauth["expiresAt"].as_i64())
    else {
        return Token::Unreadable;
    };
    if expires <= now.as_millisecond() {
        Token::Expired
    } else {
        Token::Valid(token.to_owned())
    }
}

/// What a request to the usage API gave.
#[derive(Debug)]
pub enum Fetched {
    Answer(Value),
    /// `401`: the token was refused, as an expired one is.
    Unauthorized,
    /// `429`, with its `Retry-After` in seconds when given.
    Limited(Option<Duration>),
    /// Any other status, or a body that is not JSON.
    Unreadable(String),
    /// The API could not be reached.
    Failed(String),
}

pub fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(TIMEOUT).build()
}

pub async fn fetch(client: &reqwest::Client, api: &str, token: &str) -> Fetched {
    let response = client
        .get(format!("{api}{USAGE_PATH}"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header(
            USER_AGENT,
            concat!("nowcast-brick-claude-code/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(e) => return Fetched::Failed(e.to_string()),
    };
    match response.status() {
        StatusCode::UNAUTHORIZED => Fetched::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => {
            let retry = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            Fetched::Limited(retry)
        }
        StatusCode::OK => match response.bytes().await {
            Ok(body) => match serde_json::from_slice(&body) {
                Ok(answer) => Fetched::Answer(answer),
                Err(e) => Fetched::Unreadable(format!("the answer is not JSON: {e}")),
            },
            Err(e) => Fetched::Failed(e.to_string()),
        },
        status => Fetched::Unreadable(format!("the API answered {status}")),
    }
}

/// Claude Code's copy of the last usage answer, when the file changed since
/// `seen` (its modification time, updated). Read whole only then: the file
/// holds all of Claude Code's settings.
pub fn read_copy(path: &Path, seen: &mut Option<SystemTime>) -> Option<(Value, Timestamp)> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    if *seen == Some(modified) {
        return None;
    }
    *seen = Some(modified);
    let json: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    cached_copy(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_millisecond(2_000_000_000_000).unwrap()
    }

    #[test]
    fn the_token_is_read_with_its_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        assert_eq!(read_token(&path, now()), Token::Missing);
        let write = |expires: i64| {
            let json = serde_json::json!({ "claudeAiOauth": { "accessToken": "t", "expiresAt": expires } });
            std::fs::write(&path, json.to_string()).unwrap();
        };
        write(2_000_000_000_001);
        assert_eq!(read_token(&path, now()), Token::Valid("t".into()));
        write(2_000_000_000_000);
        assert_eq!(read_token(&path, now()), Token::Expired);
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(read_token(&path, now()), Token::Unreadable);
    }

    #[test]
    fn the_copy_is_read_again_only_once_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let mut seen = None;
        assert_eq!(read_copy(&path, &mut seen), None);
        let json = serde_json::json!({
            "cachedUsageUtilization": { "fetchedAtMs": 1_000, "utilization": { "five_hour": { "utilization": 1 } } }
        });
        std::fs::write(&path, json.to_string()).unwrap();
        let (answer, at) = read_copy(&path, &mut seen).unwrap();
        assert_eq!(answer["five_hour"]["utilization"], 1);
        assert_eq!(at, Timestamp::from_millisecond(1_000).unwrap());
        assert_eq!(read_copy(&path, &mut seen), None);
    }
}
