//! The one error shape of the client API:
//! `{"error": {"code": "...", "message": "..."}}`.

use hyper::StatusCode;
use nowcast_types::{ApiError, ApiErrorCode};
use serde_json::json;

/// An error answered to a client.
///
/// Its code is one of [`ApiErrorCode`] for the cases the contract names, or one
/// of the HTTP-level codes below for requests the contract does not cover
/// (unknown route, malformed body, unknown subscription, internal failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    /// The `Allow` header of a `405`.
    pub allow: Option<&'static str>,
}

impl HttpError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            allow: None,
        }
    }

    pub fn api(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self::new(status_of(code), code.as_str(), message)
    }

    pub fn session_required() -> Self {
        Self::api(
            ApiErrorCode::SessionRequired,
            "declare the session with PUT /v1/session before any other request",
        )
    }

    pub fn unknown_flow(flow: &str) -> Self {
        Self::api(
            ApiErrorCode::UnknownFlow,
            format!("not subscribed to flow {flow:?}"),
        )
    }

    /// The request is malformed: unreadable body, missing field, bad escape.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn payload_too_large(limit: usize) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            format!("the body exceeds {limit} bytes"),
        )
    }

    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "no such route")
    }

    pub fn method_not_allowed(allow: &'static str) -> Self {
        Self {
            allow: Some(allow),
            ..Self::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                format!("this route allows {allow}"),
            )
        }
    }

    pub fn unknown_subscription(id: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "unknown_subscription",
            format!("no subscription {id:?} for this client"),
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
    }

    /// The JSON body.
    pub fn body(&self) -> Vec<u8> {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        serde_json::to_vec(&body).expect("a JSON value always serializes")
    }
}

impl From<ApiError> for HttpError {
    fn from(e: ApiError) -> Self {
        Self::api(e.code, e.message)
    }
}

/// The status the contract gives each error code.
pub(crate) fn status_of(code: ApiErrorCode) -> StatusCode {
    match code {
        ApiErrorCode::SessionRequired => StatusCode::PRECONDITION_REQUIRED,
        ApiErrorCode::InvalidParams => StatusCode::BAD_REQUEST,
        ApiErrorCode::UnknownBrick | ApiErrorCode::UnknownKind | ApiErrorCode::UnknownFlow => {
            StatusCode::NOT_FOUND
        }
        ApiErrorCode::BrickUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        ApiErrorCode::SubscriptionRefused => StatusCode::UNPROCESSABLE_ENTITY,
        ApiErrorCode::InputNotAccepted => StatusCode::METHOD_NOT_ALLOWED,
        ApiErrorCode::InvalidInput => StatusCode::BAD_REQUEST,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_code_has_its_status() {
        let expected = [
            ("session_required", 428),
            ("invalid_params", 400),
            ("unknown_brick", 404),
            ("unknown_kind", 404),
            ("unknown_flow", 404),
            ("brick_unavailable", 503),
            ("subscription_refused", 422),
            ("input_not_accepted", 405),
            ("invalid_input", 400),
        ];
        for code in ApiErrorCode::ALL {
            let status = expected
                .iter()
                .find(|(name, _)| *name == code.as_str())
                .map(|(_, status)| *status);
            assert_eq!(Some(status_of(*code).as_u16()), status, "{code}");
        }
    }

    #[test]
    fn the_body_has_the_contract_shape() {
        let error = HttpError::api(ApiErrorCode::UnknownKind, "no kind \"x\"");
        let body: serde_json::Value = serde_json::from_slice(&error.body()).unwrap();
        assert_eq!(
            body,
            json!({ "error": { "code": "unknown_kind", "message": "no kind \"x\"" } })
        );
    }

    #[test]
    fn a_refusal_keeps_the_bricks_code_in_the_message() {
        let refused = nowcast_traits::ResolveError::Refused {
            code: "no_such_repo".into(),
            message: "gone".into(),
        };
        let error = HttpError::from(ApiError::from(refused));
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.code, "subscription_refused");
        assert_eq!(error.message, "no_such_repo: gone");
    }
}
