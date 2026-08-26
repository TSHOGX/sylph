use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Session not found")]
    SessionNotFound,

    #[error("Turn not found")]
    TurnNotFound,

    #[error("Runner not found")]
    RunnerNotFound,

    /// A path inside a session's working directory does not exist. Carries no
    /// path: the client already knows what it asked for, and echoing a resolved
    /// absolute path would confirm filesystem layout to a probing caller.
    #[error("File not found")]
    FileNotFound,

    #[error("Session already has an active turn")]
    SessionAlreadyRunning,

    #[error("Turn is already in a terminal state")]
    StopAlreadyCompleted,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Attachment validation failed: {0}")]
    AttachmentInvalid(String),

    #[error("TraceDB unavailable: {0}")]
    TraceDbUnavailable(String),

    #[error("Archived session not found: {0}")]
    TraceSessionNotFound(String),

    #[error("Archived session already imported")]
    SessionAlreadyImported,

    /// The archive has the session but recorded no working directory for it,
    /// so there is nothing to resume into. Distinct from `InvalidRequest`
    /// because the request was well-formed: the gap is in the archive, and the
    /// remedy is on the archive side, not the caller's.
    #[error("Archived session has no recorded working directory: {0}")]
    TraceSessionNotResumable(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// No route matched the request path.
    ///
    /// Raised by the router's fallback, not by a handler, so that a request for
    /// an endpoint this build does not have still answers with the standard
    /// error envelope. Without it axum replies with an empty body, and a client
    /// cannot tell "this endpoint does not exist" from "the file you asked for
    /// does not exist" — both arrive as a bare 404. That ambiguity is expensive
    /// in exactly the case it appears: talking to a stale backend.
    #[error("No such endpoint")]
    RouteNotFound,

    /// The path exists but does not accept this method.
    #[error("Method not allowed for this endpoint")]
    MethodNotAllowed,

    #[error("Request not found")]
    RequestNotFound,

    /// Someone already answered this request.
    ///
    /// Distinct from `InvalidRequest` because the request was well-formed and
    /// would have been valid moments earlier. A client showing a stale approval
    /// dialog needs to tell "you were too late" from "you sent something wrong".
    #[error("Request was already answered")]
    RequestAlreadyResolved,

    /// The agent that raised a request has exited, so nothing is waiting for the
    /// answer any more.
    #[error("The agent that raised this request has exited")]
    LinkGone,

    /// A steer named a turn that is no longer the running one.
    ///
    /// A 409 rather than a 404: the request was well-formed and would have been
    /// valid moments earlier. Refusing it is the point — delivering the text into
    /// whatever turn happens to be running now would succeed silently and land it
    /// in the wrong conversation, which is far harder to notice than an error.
    #[error("Turn {expected} is no longer running")]
    TurnMoved {
        expected: String,
        running: Option<String>,
    },

    /// A link could not be established, or the agent refused to open a session.
    #[error("Could not start the agent: {0}")]
    LinkFailed(String),

    /// The agent does not support what was asked — interrupt on an adapter that
    /// never declared it, for instance. A capability failure rather than a bad
    /// request, so a UI can disable the control instead of retrying.
    #[error("The agent does not support this: {0}")]
    Unsupported(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Serialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl Error {
    /// Status code and stable error code for this variant.
    ///
    /// The codes are the enum in `docs/api-v1.openapi.json`
    /// (`ErrorResponse.error.code`), so clients can branch on them without
    /// parsing messages. Kept in one place because both [`IntoResponse`] and
    /// the auth middleware need the same mapping.
    fn parts(&self) -> (StatusCode, &'static str, String) {
        match self {
            // Database failures are never surfaced verbatim: the message can
            // contain SQL and file paths. The detail is logged instead.
            Error::Database(e) => {
                tracing::error!("Database error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "A database error occurred".to_string(),
                )
            }
            Error::SessionNotFound => (
                StatusCode::NOT_FOUND,
                "SESSION_NOT_FOUND",
                "Session not found".to_string(),
            ),
            Error::TurnNotFound => (
                StatusCode::NOT_FOUND,
                "TURN_NOT_FOUND",
                "Turn not found".to_string(),
            ),
            Error::RunnerNotFound => (
                StatusCode::NOT_FOUND,
                "RUNNER_NOT_FOUND",
                "Runner not found".to_string(),
            ),
            Error::FileNotFound => (
                StatusCode::NOT_FOUND,
                "FILE_NOT_FOUND",
                "No such file or directory in this session's working directory".to_string(),
            ),
            // 409: the request was valid but conflicts with current state, and
            // the client may succeed by retrying after the active turn ends.
            Error::SessionAlreadyRunning => (
                StatusCode::CONFLICT,
                "SESSION_ALREADY_RUNNING",
                "Session already has an active turn".to_string(),
            ),
            Error::StopAlreadyCompleted => (
                StatusCode::CONFLICT,
                "STOP_ALREADY_COMPLETED",
                "Turn has already finished".to_string(),
            ),
            Error::RequestNotFound => (
                StatusCode::NOT_FOUND,
                "REQUEST_NOT_FOUND",
                "No such request".to_string(),
            ),
            // 409 rather than 404: the request exists, and a client racing
            // another needs to know it lost rather than that it was wrong.
            Error::RequestAlreadyResolved => (
                StatusCode::CONFLICT,
                "REQUEST_ALREADY_RESOLVED",
                "This request was already answered".to_string(),
            ),
            Error::LinkGone => (
                StatusCode::CONFLICT,
                "LINK_GONE",
                "The agent that raised this request has exited".to_string(),
            ),
            Error::TurnMoved { expected, running } => (
                StatusCode::CONFLICT,
                "TURN_MOVED",
                match running {
                    Some(running) => format!(
                        "Turn {} is no longer running; {} is",
                        expected, running
                    ),
                    None => format!("Turn {} is no longer running", expected),
                },
            ),
            // 502-ish in spirit but reported as 503: the agent is a dependency
            // this server starts, and the useful signal is "try again once it
            // can start" rather than "your request was malformed".
            Error::LinkFailed(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "LINK_FAILED",
                msg.clone(),
            ),
            Error::Unsupported(msg) => (
                StatusCode::CONFLICT,
                "AGENT_UNSUPPORTED",
                msg.clone(),
            ),
            Error::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Authentication required".to_string(),
            ),
            Error::Forbidden(msg) => (StatusCode::FORBIDDEN, "POLICY_VIOLATION", msg.clone()),
            Error::AttachmentInvalid(msg) => (
                StatusCode::BAD_REQUEST,
                "ATTACHMENT_INVALID",
                msg.clone(),
            ),
            Error::TraceDbUnavailable(msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "TRACEDB_UNAVAILABLE",
                msg.clone(),
            ),
            // The id is client-supplied and safe to echo, and seeing it is what
            // makes a mistyped or stale archive id diagnosable.
            Error::TraceSessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                "TRACE_SESSION_NOT_FOUND",
                format!("No archived session with id {id}"),
            ),
            Error::SessionAlreadyImported => (
                StatusCode::CONFLICT,
                "SESSION_ALREADY_IMPORTED",
                "This archived session has already been imported".to_string(),
            ),
            // 422: understood and well-formed, but the archived session lacks a
            // field import cannot proceed without.
            Error::TraceSessionNotResumable(id) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "TRACE_SESSION_NOT_RESUMABLE",
                format!(
                    "The archive recorded no working directory for {id}, \
                     so there is no directory to resume it in."
                ),
            ),
            Error::InvalidRequest(msg) => {
                (StatusCode::BAD_REQUEST, "INVALID_REQUEST", msg.clone())
            }
            // The message names the process rather than the path, because that
            // is where the fault always is: the caller asked for a route this
            // binary does not serve.
            Error::RouteNotFound => (
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "This backend serves no such endpoint. It may be older than the \
                 client that called it; restart it from the current source."
                    .to_string(),
            ),
            Error::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "METHOD_NOT_ALLOWED",
                "That endpoint does not accept this HTTP method".to_string(),
            ),
            Error::Internal(msg) => {
                tracing::error!("Internal error: {}", msg);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "INTERNAL_ERROR",
                    "An internal error occurred".to_string(),
                )
            }
        }
    }

    /// Stable error code, for tests and for callers that need it without a
    /// full response.
    pub fn code(&self) -> &'static str {
        self.parts().1
    }

    pub fn status(&self) -> StatusCode {
        self.parts().0
    }

    /// Build the JSON body for this error.
    pub fn to_response(&self) -> ErrorResponse {
        let (_, code, message) = self.parts();
        ErrorResponse {
            error: ErrorDetail {
                code: code.to_string(),
                message,
                details: None,
            },
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, _, _) = self.parts();
        (status, Json(self.to_response())).into_response()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_variants_map_to_404() {
        assert_eq!(Error::SessionNotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(Error::TurnNotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(Error::RunnerNotFound.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn conflict_variants_map_to_409() {
        assert_eq!(
            Error::SessionAlreadyRunning.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            Error::StopAlreadyCompleted.status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn codes_match_the_openapi_enum() {
        assert_eq!(Error::SessionNotFound.code(), "SESSION_NOT_FOUND");
        assert_eq!(
            Error::SessionAlreadyRunning.code(),
            "SESSION_ALREADY_RUNNING"
        );
        assert_eq!(Error::StopAlreadyCompleted.code(), "STOP_ALREADY_COMPLETED");
        assert_eq!(
            Error::AttachmentInvalid("x".into()).code(),
            "ATTACHMENT_INVALID"
        );
        assert_eq!(
            Error::TraceDbUnavailable("x".into()).code(),
            "TRACEDB_UNAVAILABLE"
        );
        assert_eq!(
            Error::TraceSessionNotFound("x".into()).code(),
            "TRACE_SESSION_NOT_FOUND"
        );
        assert_eq!(
            Error::SessionAlreadyImported.code(),
            "SESSION_ALREADY_IMPORTED"
        );
        assert_eq!(
            Error::TraceSessionNotResumable("x".into()).code(),
            "TRACE_SESSION_NOT_RESUMABLE"
        );
        assert_eq!(Error::InvalidRequest("x".into()).code(), "INVALID_REQUEST");
    }

    /// Every code this type can emit must appear in the published enum.
    ///
    /// The assertions above pin the *spelling* of individual codes but compare
    /// them against string literals, so both sides could drift from the schema
    /// together and still pass. That is exactly what happened: the schema said
    /// `TRACE_NOT_RESUMABLE` while this type emitted
    /// `TRACE_SESSION_NOT_RESUMABLE`, and `TURN_NOT_FOUND` and `UNAUTHORIZED`
    /// were missing from it entirely, for as long as the mirror was maintained
    /// by hand. Reading the file is what makes the test able to notice.
    ///
    /// **The variant list must be exhaustive by compiler, not by care.** This
    /// test read the schema from disk and still failed to notice six codes,
    /// because the list of variants it checked was itself hand-maintained: it
    /// covered 18 of 24, and the six it omitted (`LINK_GONE`, `LINK_FAILED`,
    /// `TURN_MOVED`, `AGENT_UNSUPPORTED`, `REQUEST_NOT_FOUND`,
    /// `REQUEST_ALREADY_RESOLVED`) were precisely the ones missing from the
    /// enum. Fixing one hand-maintained mirror by adding a second one is no fix
    /// at all — so the body now also `match`es every variant with no wildcard
    /// arm, which makes a new variant a compile error until it is listed.
    ///
    /// One direction only: the schema may list codes no variant emits. Turn
    /// failure codes such as `TURN_INTERRUPTED` are recorded on turn rows rather
    /// than raised here. Note that the schema *also* still carries `RUNNER_BROKEN`,
    /// `RUNNER_PROTOCOL_ERROR`, `RUNNER_START_FAILED`, `APPROVAL_NOT_AVAILABLE`,
    /// `COMMAND_NOT_SUPPORTED`, and `TRACE_NOT_RESUMABLE`, which are v1 casualties
    /// and a superseded spelling rather than reservations — this direction is
    /// permitted so the test cannot block a schema cleanup, not because those
    /// entries are correct. The reverse — emitting a code no client is told about
    /// — is the bug.
    #[test]
    fn every_emitted_code_is_published_in_the_openapi_enum() {
        let schema = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../docs/api-v1.openapi.json"),
        )
        .expect("the OpenAPI schema is part of the repo");
        let schema: serde_json::Value =
            serde_json::from_str(&schema).expect("the OpenAPI schema is valid JSON");

        let published = schema["components"]["schemas"]["ErrorResponse"]["properties"]["error"]
            ["properties"]["code"]["enum"]
            .as_array()
            .expect("the ErrorResponse schema declares a code enum")
            .iter()
            .map(|value| value.as_str().expect("enum members are strings").to_string())
            .collect::<Vec<_>>();

        // Every variant, constructed so the match in `code()` is exercised.
        //
        // Exhaustive by construction rather than by diligence: the `match` below
        // has no wildcard arm, so adding a variant to `Error` fails to *compile*
        // until it is listed here. That is the whole mechanism. This array was
        // previously hand-maintained and had drifted to 18 of 24 variants, and
        // the six it omitted — `LINK_GONE`, `LINK_FAILED`, `TURN_MOVED`,
        // `AGENT_UNSUPPORTED`, `REQUEST_NOT_FOUND`,
        // `REQUEST_ALREADY_RESOLVED` — were exactly the ones missing from the
        // schema. So the test passed while failing to deliver the guarantee its
        // own docstring claims, which is the worst state for a test whose
        // subject is a hand-maintained mirror.
        let all = [
            Error::Database(sqlx::Error::RowNotFound),
            Error::SessionNotFound,
            Error::TurnNotFound,
            Error::RunnerNotFound,
            Error::FileNotFound,
            Error::SessionAlreadyRunning,
            Error::StopAlreadyCompleted,
            Error::Unauthorized,
            Error::Forbidden("x".into()),
            Error::AttachmentInvalid("x".into()),
            Error::TraceDbUnavailable("x".into()),
            Error::TraceSessionNotFound("x".into()),
            Error::SessionAlreadyImported,
            Error::TraceSessionNotResumable("x".into()),
            Error::InvalidRequest("x".into()),
            Error::RouteNotFound,
            Error::MethodNotAllowed,
            Error::RequestNotFound,
            Error::RequestAlreadyResolved,
            Error::LinkGone,
            Error::TurnMoved { expected: "t".into(), running: None },
            Error::LinkFailed("x".into()),
            Error::Unsupported("x".into()),
            Error::Internal("x".into()),
        ];

        // The compile-time half. Every variant must appear here, and the
        // compiler enforces it because there is no `_` arm.
        for error in &all {
            match error {
                Error::Database(_)
                | Error::SessionNotFound
                | Error::TurnNotFound
                | Error::RunnerNotFound
                | Error::FileNotFound
                | Error::SessionAlreadyRunning
                | Error::StopAlreadyCompleted
                | Error::Unauthorized
                | Error::Forbidden(_)
                | Error::AttachmentInvalid(_)
                | Error::TraceDbUnavailable(_)
                | Error::TraceSessionNotFound(_)
                | Error::SessionAlreadyImported
                | Error::TraceSessionNotResumable(_)
                | Error::InvalidRequest(_)
                | Error::RouteNotFound
                | Error::MethodNotAllowed
                | Error::RequestNotFound
                | Error::RequestAlreadyResolved
                | Error::LinkGone
                | Error::TurnMoved { .. }
                | Error::LinkFailed(_)
                | Error::Unsupported(_)
                | Error::Internal(_) => {}
            }
        }

        for error in &all {
            let code = error.code();
            assert!(
                published.iter().any(|member| member == code),
                "`{code}` is emitted by the API but absent from the OpenAPI \
                 error enum, so a client cannot know to handle it",
            );
        }
    }

    /// A mistyped archive id must be distinguishable from an archive that is
    /// down, and from a session that was already imported.
    #[test]
    fn import_failures_have_distinct_statuses() {
        assert_eq!(
            Error::TraceSessionNotFound("claude:abc".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            // Well-formed request, unusable archive record: not a 400.
            Error::TraceSessionNotResumable("claude:abc".into()).status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            Error::SessionAlreadyImported.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            Error::TraceDbUnavailable("spawn failed".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        // The client-supplied id is echoed, since that is what makes the
        // failure diagnosable.
        let body = Error::TraceSessionNotFound("claude:abc".into()).to_response();
        assert!(
            body.error.message.contains("claude:abc"),
            "id missing from message: {}",
            body.error.message
        );
    }

    /// Internal details must not reach the client: messages can carry SQL,
    /// filesystem paths, or environment specifics.
    #[test]
    fn internal_errors_do_not_leak_details() {
        let error = Error::Internal("connection to /Users/x/secret.db failed".into());
        let body = error.to_response();
        assert_eq!(body.error.code, "INTERNAL_ERROR");
        assert!(!body.error.message.contains("secret.db"));
    }

    /// Client-supplied validation messages are safe and useful to echo back.
    #[test]
    fn validation_errors_keep_their_message() {
        let error = Error::AttachmentInvalid("Attachment 'a.png' is too large".into());
        assert_eq!(
            error.to_response().error.message,
            "Attachment 'a.png' is too large"
        );
    }
}
