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

    #[error("Attachment validation failed: {0}")]
    AttachmentInvalid(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Error::Database(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DATABASE_ERROR".to_string(),
                e.to_string(),
            ),
            Error::SessionNotFound => (
                StatusCode::NOT_FOUND,
                "SESSION_NOT_FOUND".to_string(),
                "Session not found".to_string(),
            ),
            Error::TurnNotFound => (
                StatusCode::NOT_FOUND,
                "TURN_NOT_FOUND".to_string(),
                "Turn not found".to_string(),
            ),
            Error::RunnerNotFound => (
                StatusCode::NOT_FOUND,
                "RUNNER_NOT_FOUND".to_string(),
                "Runner not found".to_string(),
            ),
            Error::AttachmentInvalid(msg) => (
                StatusCode::BAD_REQUEST,
                "ATTACHMENT_INVALID".to_string(),
                msg.clone(),
            ),
            Error::InvalidRequest(msg) => (
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST".to_string(),
                msg,
            ),
            Error::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR".to_string(),
                msg,
            ),
        };

        let body = Json(ErrorResponse {
            error: ErrorDetail { code, message },
        });

        (status, body).into_response()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

