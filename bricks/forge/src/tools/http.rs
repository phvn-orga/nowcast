//! JSON over HTTP, answers asked for compressed in gzip (GitHub's weigh 150
//! to 600 KB, about ten times less in gzip). GET requests are conditional:
//! each answer's `ETag` is sent back with the next request for the same URL
//! (`If-None-Match`), and a `304` gives back the body kept from the last
//! `200`. GitHub does not count a `304` against its rate limit. The cache is shared by every branch
//! followed, so two readings of the same URL pay for it once. POST requests,
//! for an API that only answers them (GraphQL), are not cached. A forge
//! without `ETag` is read in full every time; the answer's headers say what
//! is left of its limit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{
    AUTHORIZATION, CONTENT_TYPE, ETAG, HeaderMap, HeaderValue, IF_NONE_MATCH, RETRY_AFTER,
    USER_AGENT,
};
use reqwest::{RequestBuilder, Response, StatusCode};
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
    /// `404`, or `410`: gone, such as a deleted issue.
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

    /// GETs `url` as JSON, with `headers` and the `authorization` header
    /// (`Bearer <token>`, for one).
    pub async fn get(
        &self,
        url: &str,
        authorization: &str,
        headers: &[(&str, &str)],
    ) -> Result<Arc<Value>, Failure> {
        Ok(self.get_answer(url, authorization, headers).await?.0)
    }

    /// [`Http::get`], with the answer's headers, for a forge that says on a
    /// successful answer how much of its limit is left.
    pub async fn get_answer(
        &self,
        url: &str,
        authorization: &str,
        headers: &[(&str, &str)],
    ) -> Result<(Arc<Value>, HeaderMap), Failure> {
        // The body is taken with its ETag: a 304 gives back this one, even if
        // the cache was cleared meanwhile.
        let kept = self.cache.lock().unwrap().get(url).cloned();
        let mut request = self.request(self.client.get(url), authorization, headers);
        if let Some((etag, _)) = &kept {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = send(request).await?;
        let answer_headers = response.headers().clone();
        if response.status() == StatusCode::NOT_MODIFIED {
            return match kept {
                Some((_, body)) => Ok((body, answer_headers)),
                None => Err(Failure::Unreadable("304 without a kept answer".into())),
            };
        }
        let etag = answer_headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = Arc::new(json(response).await?);
        if let Some(etag) = etag {
            let mut cache = self.cache.lock().unwrap();
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
            cache.insert(url.to_owned(), (etag, body.clone()));
        }
        Ok((body, answer_headers))
    }

    /// POSTs `body` as JSON to `url`, with `headers` and the `authorization`
    /// header, and reads the answer as JSON. Nothing is cached.
    pub async fn post(
        &self,
        url: &str,
        authorization: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<Value, Failure> {
        Ok(self.post_answer(url, authorization, headers, body).await?.0)
    }

    /// [`Http::post`], with the answer's headers.
    pub async fn post_answer(
        &self,
        url: &str,
        authorization: &str,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Result<(Value, HeaderMap), Failure> {
        let request = self
            .request(self.client.post(url), authorization, headers)
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_string());
        let response = send(request).await?;
        let answer_headers = response.headers().clone();
        Ok((json(response).await?, answer_headers))
    }

    /// `request` with the `authorization` header, the user agent and
    /// `headers`. The authorization is marked sensitive, and left out when
    /// it cannot be a header: the forge then refuses the request.
    fn request(
        &self,
        request: RequestBuilder,
        authorization: &str,
        headers: &[(&str, &str)],
    ) -> RequestBuilder {
        let mut request = request.header(
            USER_AGENT,
            concat!("nowcast-brick-forge/", env!("CARGO_PKG_VERSION")),
        );
        if let Ok(mut value) = HeaderValue::from_str(authorization) {
            value.set_sensitive(true);
            request = request.header(AUTHORIZATION, value);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request
    }
}

async fn send(request: RequestBuilder) -> Result<Response, Failure> {
    request
        .send()
        .await
        .map_err(|e| Failure::Unreachable(e.to_string()))
}

/// The body of a successful answer, as JSON, `null` for a `204`; any other
/// status is a failure.
async fn json(response: Response) -> Result<Value, Failure> {
    let status = response.status();
    match status {
        // Azure DevOps answers a request it cannot authenticate with a sign-in
        // page, as a 203.
        StatusCode::UNAUTHORIZED | StatusCode::NON_AUTHORITATIVE_INFORMATION => {
            return Err(Failure::Unauthorized);
        }
        StatusCode::NO_CONTENT => return Ok(Value::Null),
        StatusCode::NOT_FOUND | StatusCode::GONE => return Err(Failure::NotFound),
        StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS => {
            return Err(match limit(response.headers()) {
                Some(wait) => Failure::Limited(wait),
                None if status == StatusCode::TOO_MANY_REQUESTS => Failure::Limited(None),
                None => Failure::Unauthorized,
            });
        }
        s if !s.is_success() => {
            return Err(Failure::Unreadable(format!("the forge answered {s}")));
        }
        _ => {}
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| Failure::Unreachable(e.to_string()))?;
    serde_json::from_slice(&body)
        .map_err(|e| Failure::Unreadable(format!("the answer is not JSON: {e}")))
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

/// Percent-encodes what a path segment or a query value cannot hold as is.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
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
        assert_eq!(http.get(&url, "Bearer t", &[]).await.unwrap()["n"], 1);
        assert_eq!(http.get(&url, "Bearer t", &[]).await.unwrap()["n"], 1);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].contains("authorization: bearer t"));
        assert!(requests[1].contains("if-none-match: \"v1\""));
    }

    /// A POST sends its JSON body and caches nothing; its failures are those
    /// of a GET.
    #[tokio::test]
    async fn a_post_sends_json_and_reads_json() {
        let server = Server::start(|request| {
            if request.contains("/limited") {
                (429, vec![], String::new())
            } else {
                (200, vec![("ETag", "\"v1\"")], "{\"ok\":true}".into())
            }
        })
        .await;
        let http = Http::new().unwrap();
        let body = serde_json::json!({ "query": "q" });
        let url = server.url("/graphql");
        for _ in 0..2 {
            let answer = http.post(&url, "Basic dA==", &[], &body).await.unwrap();
            assert_eq!(answer["ok"], true);
        }
        let requests = server.requests();
        assert!(requests[0].starts_with("post /graphql"));
        assert!(requests[0].contains("authorization: basic da=="));
        assert!(requests[0].contains("content-type: application/json"));
        assert!(requests[0].ends_with("{\"query\":\"q\"}"));
        assert!(!requests[1].contains("if-none-match"));
        assert_eq!(
            http.post(&server.url("/limited"), "t", &[], &body).await,
            Err(Failure::Limited(None))
        );
    }

    /// `{"n":1}`, compressed by `gzip -n`.
    const GZIPPED: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 171, 86, 202, 83, 178, 50, 172, 5, 0, 126, 59, 75, 212, 7,
        0, 0, 0,
    ];

    /// Answers are asked for in gzip, and a gzip body is decoded.
    #[tokio::test]
    async fn answers_are_asked_for_compressed() {
        let server = Server::start_raw(|request| {
            if request.contains("accept-encoding: gzip") {
                (200, vec![("Content-Encoding", "gzip")], GZIPPED.to_vec())
            } else {
                (200, vec![], b"{\"n\":0}".to_vec())
            }
        })
        .await;
        let http = Http::new().unwrap();
        assert_eq!(http.get(&server.url("/a"), "t", &[]).await.unwrap()["n"], 1);
    }

    #[tokio::test]
    async fn failures_are_told_apart() {
        let server = Server::start(|request| match request.lines().next().unwrap_or_default() {
            l if l.contains("/401") => (401, vec![], String::new()),
            l if l.contains("/404") => (404, vec![], String::new()),
            l if l.contains("/410") => (410, vec![], String::new()),
            l if l.contains("/203") => (203, vec![], "<html>sign in</html>".into()),
            l if l.contains("/204") => (204, vec![], String::new()),
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
        assert_eq!(get("/410").await, Err(Failure::NotFound));
        assert_eq!(
            get("/203").await,
            Err(Failure::Unauthorized),
            "a sign-in page"
        );
        assert_eq!(get("/204").await.unwrap().as_ref(), &Value::Null);
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
