//! The client API (contract section 3), over HTTP/1.1 on the service's Unix
//! socket, with the session's event stream in Server-Sent Events.
//!
//! | Route | Success | Errors |
//! |---|---|---|
//! | `PUT /v1/session` | `204` | |
//! | `POST /v1/subscriptions` | `201` | `400` `invalid_params`, `404` `unknown_brick` / `unknown_kind`, `422` `subscription_refused`, `503` `brick_unavailable` |
//! | `DELETE /v1/subscriptions/{subscription}` | `204` | `404` `unknown_subscription` |
//! | `GET /v1/flows/{flow}` | `200` last event, `204` none yet | `404` `unknown_flow` |
//! | `POST /v1/flows/{flow}/refresh` | `202` | `404` `unknown_flow` |
//! | `POST /v1/flows/{flow}/input` | `202` | `400` `invalid_input`, `404` `unknown_flow`, `405` `input_not_accepted`, `503` `brick_unavailable` |
//! | `GET /v1/stream` | `200` `text/event-stream` | |
//!
//! Every route but the first answers `428` `session_required` until the
//! anchor declares its session. Errors always read
//! `{"error": {"code": "...", "message": "..."}}`. Beyond the contract's
//! codes, the server answers `400` / `413` `invalid_request` for a malformed
//! request, `404` `not_found` and `405` `method_not_allowed` for a wrong route,
//! and `500` `internal_error` when the service itself fails.
//!
//! The server holds no state of its own: sessions, subscriptions, flows and
//! bricks belong to the modules it is given, see [`Server`].

mod error;
mod event;
mod params;
mod route;
mod server;
mod sse;
mod validators;

pub use server::{MAX_BODY, Server};
pub use validators::Validators;
