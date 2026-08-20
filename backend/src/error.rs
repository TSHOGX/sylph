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
