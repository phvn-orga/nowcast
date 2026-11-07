//! Maps a method and a raw path to a route of the client API.
//!
//! Path segments stay percent-encoded here: a flow id may hold `/`, so the path
//! is split first and each segment decoded afterwards, by [`decode`].

use hyper::Method;
use nowcast_types::EventId;

use crate::error::HttpError;

/// A route of the client API, its path parameters still percent-encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route<'a> {
    /// `PUT /v1/session`
    Session,
    /// `POST /v1/subscriptions`
    Subscribe,
    /// `DELETE /v1/subscriptions/{subscription}`
    Unsubscribe(&'a str),
    /// `GET /v1/flows/{flow}`
    Flow(&'a str),
    /// `POST /v1/flows/{flow}/refresh`
    Refresh(&'a str),
    /// `POST /v1/flows/{flow}/input`
    Input(&'a str),
    /// `GET /v1/stream`
    Stream,
}

impl<'a> Route<'a> {
    /// The route for `method` on `path`; `404` `not_found` when no route has
    /// that path, `405` `method_not_allowed` when one has it with another
    /// method.
    pub fn parse(method: &Method, path: &'a str) -> Result<Self, HttpError> {
        let segments: Vec<&str> = path
            .strip_prefix("/v1/")
            .ok_or_else(HttpError::not_found)?
            .split('/')
            .collect();
        let (route, allowed) = match segments.as_slice() {
            ["session"] => (Self::Session, Method::PUT),
            ["subscriptions"] => (Self::Subscribe, Method::POST),
            ["subscriptions", id] => (Self::Unsubscribe(id), Method::DELETE),
            ["flows", flow] => (Self::Flow(flow), Method::GET),
            ["flows", flow, "refresh"] => (Self::Refresh(flow), Method::POST),
            ["flows", flow, "input"] => (Self::Input(flow), Method::POST),
            ["stream"] => (Self::Stream, Method::GET),
            _ => return Err(HttpError::not_found()),
        };
        if *method == allowed {
            return Ok(route);
        }
        let allow = match allowed {
            Method::PUT => "PUT",
            Method::POST => "POST",
            Method::DELETE => "DELETE",
            _ => "GET",
        };
        Err(HttpError::method_not_allowed(allow))
    }
}

/// Decodes a percent-encoded path segment. `+` stays a `+`, as in any path.
/// Fails on a `%` not followed by two hex digits, or bytes that are not UTF-8.
pub(crate) fn decode(segment: &str) -> Result<String, HttpError> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let byte = bytes
                .get(i + 1..i + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                // `from_str_radix` takes a leading sign: "%+1" is no escape.
                .filter(|_| bytes[i + 1].is_ascii_hexdigit())
                .ok_or_else(|| {
                    HttpError::invalid_request(format!(
                        "malformed percent-encoding in path segment {segment:?}"
                    ))
                })?;
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| {
        HttpError::invalid_request(format!("path segment {segment:?} does not decode to UTF-8"))
    })
}

/// The `after` parameter of a query string, the start of a history read.
/// Other parameters are ignored; `400` `invalid_request` when `after` is not
/// an event id.
pub(crate) fn after(query: Option<&str>) -> Result<Option<EventId>, HttpError> {
    let Some(value) =
        query
            .unwrap_or_default()
            .split('&')
            .find_map(|pair| match pair.split_once('=') {
                Some(("after", value)) => Some(value),
                None if pair == "after" => Some(""),
                _ => None,
            })
    else {
        return Ok(None);
    };
    // `u64::from_str` alone would take a leading `+`, which no id has.
    value
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| value.parse().ok())
        .flatten()
        .map(|id| Some(EventId(id)))
        .ok_or_else(|| HttpError::invalid_request(format!("after={value:?} is not an event id")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn after_is_an_event_id_or_absent() {
        let parsed = |query| after(query).map_err(|e| (e.status.as_u16(), e.code));
        assert_eq!(parsed(None), Ok(None));
        assert_eq!(parsed(Some("")), Ok(None));
        assert_eq!(parsed(Some("x=1")), Ok(None));
        assert_eq!(parsed(Some("after=0")), Ok(Some(EventId(0))));
        assert_eq!(parsed(Some("x=1&after=42")), Ok(Some(EventId(42))));
        assert_eq!(parsed(Some("after=")), Err((400, "invalid_request")));
        assert_eq!(parsed(Some("after")), Err((400, "invalid_request")));
        assert_eq!(parsed(Some("after=-1")), Err((400, "invalid_request")));
        assert_eq!(parsed(Some("after=4x")), Err((400, "invalid_request")));
        assert_eq!(parsed(Some("after=+5")), Err((400, "invalid_request")));
    }

    fn parse(method: Method, path: &str) -> Result<Route<'_>, (u16, &'static str)> {
        Route::parse(&method, path).map_err(|e| (e.status.as_u16(), e.code))
    }

    #[test]
    fn every_route_parses() {
        assert_eq!(parse(Method::PUT, "/v1/session"), Ok(Route::Session));
        assert_eq!(
            parse(Method::POST, "/v1/subscriptions"),
            Ok(Route::Subscribe)
        );
        assert_eq!(
            parse(Method::DELETE, "/v1/subscriptions/s-1"),
            Ok(Route::Unsubscribe("s-1"))
        );
        assert_eq!(
            parse(Method::GET, "/v1/flows/forge%2Fa"),
            Ok(Route::Flow("forge%2Fa"))
        );
        assert_eq!(
            parse(Method::POST, "/v1/flows/forge%2Fa/refresh"),
            Ok(Route::Refresh("forge%2Fa"))
        );
        assert_eq!(parse(Method::GET, "/v1/stream"), Ok(Route::Stream));
    }

    #[test]
    fn unknown_paths_are_not_found() {
        for path in [
            "/",
            "/v1",
            "/v1/",
            "/v2/session",
            "/v1/session/",
            "/v1/nope",
            "/v1/subscriptions/a/b",
            // An unencoded flow id is split into several segments.
            "/v1/flows/forge/a",
            "/v1/flows/forge%2Fa/refresh/now",
        ] {
            assert_eq!(parse(Method::GET, path), Err((404, "not_found")), "{path}");
        }
    }

    #[test]
    fn a_wrong_method_is_not_allowed() {
        let error = Route::parse(&Method::GET, "/v1/session").unwrap_err();
        assert_eq!(
            (error.status.as_u16(), error.code),
            (405, "method_not_allowed")
        );
        assert_eq!(error.allow, Some("PUT"));
        assert_eq!(
            parse(Method::POST, "/v1/stream"),
            Err((405, "method_not_allowed"))
        );
        assert_eq!(
            parse(Method::GET, "/v1/flows/a%2Fb/refresh"),
            Err((405, "method_not_allowed"))
        );
    }

    #[test]
    fn decoding_restores_reserved_characters() {
        assert_eq!(
            decode("forge%2Fgithub.com%2Fo%2Fr%23main").unwrap(),
            "forge/github.com/o/r#main"
        );
        assert_eq!(decode("a%20b+c").unwrap(), "a b+c");
        assert_eq!(decode("%e2%82%ac").unwrap(), "\u{20ac}");
        assert_eq!(decode("plain").unwrap(), "plain");
        assert_eq!(decode("").unwrap(), "");
        assert_eq!(decode("%25").unwrap(), "%");
    }

    #[test]
    fn malformed_escapes_are_refused() {
        for segment in ["%", "%2", "a%zz", "%+1", "%-1", "%FF", "%C3%28"] {
            let error = decode(segment).unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.code),
                (400, "invalid_request"),
                "{segment}"
            );
        }
    }
}
