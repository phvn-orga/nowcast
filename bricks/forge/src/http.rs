//! Conditional GET requests: each answer's `ETag` is sent back with the next
//! request for the same URL (`If-None-Match`), and a `304` gives back the
//! body kept from the last `200`. GitHub does not count a `304` against its
//! rate limit. The cache is shared by every flow, so two flows reading the
//! same URL pay for it once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, ETAG, HeaderMap, IF_NONE_MATCH, RETRY_AFTER, USER_AGENT};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(20);
/// Entries kept at most; past it, the cache starts again empty. A flow reads
/// a handful of URLs, most of them for its current head only.
const CACHE_MAX: usize = 4096;

/// Why a request gave no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// `401`, or `403` without a rate limit: the token was refused.
    Unauthorized,
    NotFound,
    /// A rate limit, with how long to wait when the forge says.
    Limited(Option<Duration>),
    /// Any other status, or a body that is not JSON.
    Unreadable(String),
    /// The forge could not be reached.
    Unreachable(String),
}

/// Each URL's `ETag` and body. Bodies are shared, not copied, with every
/// reading that gets them.
type Cache = HashMap<String, (String, Arc<Value>)>;

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    cache: Arc<Mutex<Cache>>,
}

impl Http {
    pub fn new() -> reqwest::Result<Self> {
        // The one TLS provider the binary carries; installed once.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            client: reqwest::Client::builder().timeout(TIMEOUT).build()?,
            cache: Arc::default(),
        })
    }

    /// GETs `url` as JSON, with `headers` and the bearer `token`.
    pub async fn get(
        &self,
        url: &str,
        token: &str,
        headers: &[(&str, &str)],
    ) -> Result<Arc<Value>, Failure> {
        // The body is taken with its ETag: a 304 gives back this one, even if
        // the cache was cleared meanwhile.
        let kept = self.cache.lock().unwrap().get(url).cloned();
        let mut request = self
            .client
            .get(url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(
                USER_AGENT,
                concat!("nowcast-brick-forge/", env!("CARGO_PKG_VERSION")),
            );
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some((etag, _)) = &kept {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Failure::Unreachable(e.to_string()))?;
        let status = response.status();
        match status {
            StatusCode::NOT_MODIFIED => match kept {
                Some((_, body)) => Ok(body),
                None => Err(Failure::Unreadable("304 without a kept answer".into())),
            },
            StatusCode::UNAUTHORIZED => Err(Failure::Unauthorized),
            StatusCode::NOT_FOUND => Err(Failure::NotFound),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
                match limit(response.headers()) {
                    Some(wait) => Err(Failure::Limited(wait)),
                    None if status == StatusCode::TOO_MANY_REQUESTS => Err(Failure::Limited(None)),
                    None => Err(Failure::Unauthorized),
                }
            }
            s if s.is_success() => {
                let etag = response
                    .headers()
                    .get(ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let body = response
                    .bytes()
                    .await
                    .map_err(|e| Failure::Unreachable(e.to_string()))?;
                let body: Arc<Value> = serde_json::from_slice(&body)
                    .map(Arc::new)
                    .map_err(|e| Failure::Unreadable(format!("the answer is not JSON: {e}")))?;
                if let Some(etag) = etag {
                    let mut cache = self.cache.lock().unwrap();
                    if cache.len() >= CACHE_MAX {
                        cache.clear();
                    }
                    cache.insert(url.to_owned(), (etag, body.clone()));
                }
                Ok(body)
            }
            s => Err(Failure::Unreadable(format!("the forge answered {s}"))),
        }
    }
}

/// How long a rate-limited answer asks to wait: `Retry-After` in seconds,
/// or the reset of an exhausted limit (`x-ratelimit-remaining: 0` with
/// `x-ratelimit-reset`, in seconds since 1970). `None` when the answer is not
/// a rate limit; `Some(None)` when it is one without a wait.
fn limit(headers: &HeaderMap) -> Option<Option<Duration>> {
    let number = |name| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    if let Some(secs) = number(RETRY_AFTER.as_str()) {
        return Some(Some(Duration::from_secs(secs)));
    }
    if number("x-ratelimit-remaining") == Some(0) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        return Some(
            number("x-ratelimit-reset").map(|reset| Duration::from_secs(reset.saturating_sub(now))),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Server;

    #[tokio::test]
    async fn a_304_gives_back_the_kept_answer() {
        let server = Server::start(|request| {
            if request.contains("if-none-match: \"v1\"") {
                (304, vec![], String::new())
            } else {
                (200, vec![("ETag", "\"v1\"")], "{\"n\":1}".into())
            }
        })
        .await;
        let http = Http::new().unwrap();
        let url = server.url("/a");
        assert_eq!(http.get(&url, "t", &[]).await.unwrap()["n"], 1);
        assert_eq!(http.get(&url, "t", &[]).await.unwrap()["n"], 1);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("authorization: bearer t"));
        assert!(requests[1].contains("if-none-match: \"v1\""));
    }

    #[tokio::test]
    async fn failures_are_told_apart() {
        let server = Server::start(|request| match request.lines().next().unwrap_or_default() {
            l if l.contains("/401") => (401, vec![], String::new()),
            l if l.contains("/404") => (404, vec![], String::new()),
            l if l.contains("/limited") => (403, vec![("Retry-After", "7")], String::new()),
            l if l.contains("/forbidden") => (403, vec![], String::new()),
            _ => (200, vec![], "not json".into()),
        })
        .await;
        let http = Http::new().unwrap();
        let get = |path: &str| {
            let (http, url) = (http.clone(), server.url(path));
            async move { http.get(&url, "t", &[]).await }
        };
        assert_eq!(get("/401").await, Err(Failure::Unauthorized));
        assert_eq!(get("/404").await, Err(Failure::NotFound));
        assert_eq!(
            get("/limited").await,
            Err(Failure::Limited(Some(Duration::from_secs(7))))
        );
        assert_eq!(get("/forbidden").await, Err(Failure::Unauthorized));
        assert!(matches!(get("/other").await, Err(Failure::Unreadable(_))));
        assert!(matches!(
            http.get("http://127.0.0.1:9/", "t", &[]).await,
            Err(Failure::Unreachable(_))
        ));
    }
}
