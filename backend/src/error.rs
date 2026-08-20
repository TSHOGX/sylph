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
    /// One direction only: the schema may list codes no variant emits (turn
    /// failure codes such as `TURN_INTERRUPTED` are recorded on turn rows rather
    /// than raised here, and a few are reserved for planned endpoints). The
    /// reverse — emitting a code no client is told about — is the bug.
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

        // Every variant, constructed so the match in `code()` is exercised. A
        // new variant added without a schema entry fails here.
        let emitted = [
            Error::Database(sqlx::Error::RowNotFound).code(),
            Error::SessionNotFound.code(),
            Error::TurnNotFound.code(),
            Error::RunnerNotFound.code(),
            Error::FileNotFound.code(),
            Error::SessionAlreadyRunning.code(),
            Error::StopAlreadyCompleted.code(),
            Error::Unauthorized.code(),
            Error::Forbidden("x".into()).code(),
            Error::AttachmentInvalid("x".into()).code(),
            Error::TraceDbUnavailable("x".into()).code(),
            Error::TraceSessionNotFound("x".into()).code(),
            Error::SessionAlreadyImported.code(),
            Error::TraceSessionNotResumable("x".into()).code(),
            Error::InvalidRequest("x".into()).code(),
            Error::RouteNotFound.code(),
            Error::MethodNotAllowed.code(),
            Error::Internal("x".into()).code(),
        ];

        for code in emitted {
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
