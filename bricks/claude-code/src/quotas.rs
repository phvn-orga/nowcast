//! The account's quota windows, merged from every source by freshness.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde_json::{Map, Value, json};

/// Where a reading comes from, as `claude-code.quotas/v1` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The usage API, read by the brick or copied by Claude Code.
    UsageApi,
    /// A session's last response.
    Session,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Self::UsageApi => "usage-api",
            Self::Session => "session",
        }
    }
}

/// One reading of one window.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub percent: f64,
    pub resets_at: Option<String>,
    pub as_of: Timestamp,
    pub source: Source,
}

/// The state of the usage API as a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiStatus {
    /// Not read yet.
    Pending,
    Ok,
    TokenExpired,
    NoCredentials,
    RateLimited,
    Unreadable,
}

impl ApiStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::TokenExpired => "token_expired",
            Self::NoCredentials => "no_credentials",
            Self::RateLimited => "rate_limited",
            Self::Unreadable => "unreadable",
        }
    }
}

/// Every window known, and the usage API's state.
#[derive(Debug, Clone, PartialEq)]
pub struct Quotas {
    windows: BTreeMap<String, Reading>,
    status: ApiStatus,
    /// The last successful reading of the usage API.
    api_as_of: Option<Timestamp>,
}

impl Default for Quotas {
    fn default() -> Self {
        Self {
            windows: BTreeMap::new(),
            status: ApiStatus::Pending,
            api_as_of: None,
        }
    }
}

impl Quotas {
    /// Keeps `reading` for `name` unless the window holds a fresher one.
    /// Returns whether what a consumer sees changed: the percentage, the
    /// reset or the source. A newer instant alone is kept without counting as
    /// a change, so that every session turn does not publish again.
    pub fn offer(&mut self, name: &str, reading: Reading) -> bool {
        match self.windows.get_mut(name) {
            Some(held) if held.as_of > reading.as_of => false,
            Some(held) => {
                let changed = held.percent != reading.percent
                    || held.resets_at != reading.resets_at
                    || held.source != reading.source;
                *held = reading;
                changed
            }
            None => {
                self.windows.insert(name.to_owned(), reading);
                true
            }
        }
    }

    /// Records the usage API's state. Returns whether it changed.
    pub fn set_status(&mut self, status: ApiStatus, now: Timestamp) -> bool {
        if status == ApiStatus::Ok {
            self.api_as_of = Some(now);
        }
        let changed = self.status != status;
        self.status = status;
        changed
    }

    pub fn status(&self) -> ApiStatus {
        self.status
    }

    /// The detail of `claude-code.quotas/v1`.
    pub fn detail(&self) -> Value {
        let windows: Vec<Value> = self
            .windows
            .iter()
            .map(|(name, r)| {
                json!({
                    "name": name,
                    "percent": r.percent,
                    "resets_at": r.resets_at,
                    "as_of": r.as_of.to_string(),
                    "source": r.source.as_str(),
                })
            })
            .collect();
        json!({
            "windows": windows,
            "usage_api": {
                "status": self.status.as_str(),
                "as_of": self.api_as_of.map(|t| t.to_string()),
            },
        })
    }
}

/// The windows of a usage API answer: every top-level object with a numeric
/// `utilization`, under its key (`five_hour`, `seven_day`, `seven_day_opus`).
/// `None` when the answer is not an object.
pub fn windows_of(answer: &Value) -> Option<Vec<(String, f64, Option<String>)>> {
    let object = answer.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(name, window)| {
                let window = window.as_object()?;
                let percent = window.get("utilization")?.as_f64()?;
                Some((name.clone(), percent, string(window, "resets_at")))
            })
            .collect(),
    )
}

/// Claude Code's copy of a usage API answer, in its `~/.claude.json`: the
/// answer and when it was fetched.
pub fn cached_copy(claude_json: &Value) -> Option<(Value, Timestamp)> {
    let cached = claude_json.get("cachedUsageUtilization")?;
    let fetched = cached.get("fetchedAtMs")?.as_i64()?;
    let at = Timestamp::from_millisecond(fetched).ok()?;
    Some((cached.get("utilization")?.clone(), at))
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: i64) -> Timestamp {
        Timestamp::from_second(s).unwrap()
    }

    fn reading(percent: f64, s: i64, source: Source) -> Reading {
        Reading {
            percent,
            resets_at: Some("2026-11-07T18:00:00Z".into()),
            as_of: at(s),
            source,
        }
    }

    #[test]
    fn the_freshest_reading_wins_whatever_its_source() {
        let mut quotas = Quotas::default();
        assert!(quotas.offer("five_hour", reading(10.0, 100, Source::UsageApi)));
        assert!(!quotas.offer("five_hour", reading(50.0, 90, Source::Session)));
        assert!(quotas.offer("five_hour", reading(12.0, 110, Source::Session)));
        // Only the instant moved: kept, but nothing to publish.
        assert!(!quotas.offer("five_hour", reading(12.0, 120, Source::Session)));
        let detail = quotas.detail();
        assert_eq!(detail["windows"][0]["percent"], 12.0);
        assert_eq!(detail["windows"][0]["source"], "session");
        assert_eq!(detail["windows"][0]["as_of"], at(120).to_string());
    }

    #[test]
    fn the_status_dates_the_last_successful_reading() {
        let mut quotas = Quotas::default();
        assert_eq!(
            quotas.detail()["usage_api"],
            json!({ "status": "pending", "as_of": null })
        );
        assert!(quotas.set_status(ApiStatus::Ok, at(5)));
        assert!(!quotas.set_status(ApiStatus::Ok, at(6)));
        assert!(quotas.set_status(ApiStatus::TokenExpired, at(7)));
        assert_eq!(
            quotas.detail()["usage_api"],
            json!({ "status": "token_expired", "as_of": at(6).to_string() })
        );
    }

    #[test]
    fn an_answer_gives_every_window_with_a_utilization() {
        let answer = json!({
            "five_hour": { "utilization": 13, "resets_at": "2026-11-06T11:59:59+00:00" },
            "seven_day_opus": { "utilization": 2.5, "resets_at": null },
            "seven_day_sonnet": null,
            "limits": [{ "kind": "session", "percent": 13 }],
            "extra_usage": { "is_enabled": false },
        });
        let mut windows = windows_of(&answer).unwrap();
        windows.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            windows,
            vec![
                (
                    "five_hour".into(),
                    13.0,
                    Some("2026-11-06T11:59:59+00:00".into())
                ),
                ("seven_day_opus".into(), 2.5, None),
            ]
        );
        assert_eq!(windows_of(&json!([1])), None);
    }

    #[test]
    fn the_cached_copy_is_dated() {
        let claude = json!({
            "cachedUsageUtilization": {
                "fetchedAtMs": 1_793_954_132_073_i64,
                "utilization": { "five_hour": { "utilization": 13 } },
            }
        });
        let (answer, fetched) = cached_copy(&claude).unwrap();
        assert_eq!(
            fetched,
            Timestamp::from_millisecond(1_793_954_132_073).unwrap()
        );
        assert_eq!(answer["five_hour"]["utilization"], 13);
        assert_eq!(cached_copy(&json!({})), None);
    }
}
