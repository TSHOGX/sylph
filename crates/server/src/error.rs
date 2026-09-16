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

    /// A path inside a session's working directory does not exist. Carries no
    /// path: the client already knows what it asked for, and echoing a resolved
    /// absolute path would confirm filesystem layout to a probing caller.
    #[error("File not found")]
    FileNotFound,

    #[error("Session already has an active turn")]
    SessionAlreadyRunning,

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Forbidden: {0}")]
    Forbidden(String),

    #[error("Attachment validation failed: {0}")]
    AttachmentInvalid(String),

    /// The agent CLI could not be started, or started and reported no session id.
    ///
    /// **Reporting no id is a failure rather than a degraded success**: without one the
    /// session can never be resumed, so every link death — the idle reaper, a restart,
    /// an explicit detach — would make it permanently unlinkable
    /// (`docs/api/sessions.md` § `POST /api/v1/sessions`).
    #[error("Could not start the agent: {0:#}")]
    AgentStartFailed(anyhow::Error),

    /// Two rows for one conversation, refused by the primary key.
    ///
    /// A `409` rather than an upsert: the triple *is* the session, so a second row
    /// under one id is the state the key exists to prevent.
    #[error("A session already exists with that identity")]
    SessionAlreadyPresent,

    /// A surface whose contract is settled and whose implementation is not.
    ///
    /// Deliberately its own variant rather than an `Internal`: an operation that
    /// *cannot yet* be performed and one that failed unexpectedly call for opposite
    /// responses from a client, and flattening them would have a UI offer a retry for
    /// something no retry completes.
    #[error("Not implemented: {0}")]
    NotImplemented(String),

    /// That environment's store holds no session under the given native id.
    ///
    /// A `404` about the *vendor's* store rather than about Sylph's, which is why it is
    /// not [`Self::SessionNotFound`]: the fix is a different id, and a client that
    /// cannot tell the two apart would look for the session in the wrong store.
    #[error("No such session in that agent's native store: {0}")]
    NativeSessionNotFound(String),

    /// The store holds the session and recorded no working directory for it.
    ///
    /// `422` rather than `400`: the request is well-formed and the id is real, so
    /// nothing the caller can change makes this succeed — the *source* is unusable.
    /// Answering `400` would send a reader to their own request, and answering `404`
    /// would claim a session that demonstrably exists is absent.
    ///
    /// **Guessing a directory is not available as a fallback.** A workdir is the
    /// `current_dir` of a spawned agent, so inventing one decides where arbitrary code
    /// runs (`docs/api/sessions.md` § `POST /api/v1/sessions/adopt`).
    #[error("That native session recorded no working directory, so it cannot be resumed")]
    NativeSessionNotResumable,

    /// No envagent is connected for the named environment.
    ///
    /// `503` and not `400`: the environment id is well-formed and may be correct, and
    /// the fix is to bring a machine online rather than to edit the request. Distinct
    /// from [`Self::LinkFailed`], which is an agent that would not start on a machine
    /// that *is* reachable.
    #[error("No environment is connected for '{0}'")]
    EnvironmentUnreachable(String),

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

    /// A link could not be established, or the agent refused to open a session.
    #[error("Could not start the agent: {0}")]
    LinkFailed(String),

    #[error("Agent handshake failed: {0}")]
    HandshakeFailed(String),

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
            // 502-ish in spirit but reported as 503: the agent is a dependency
            // this server starts, and the useful signal is "try again once it
            // can start" rather than "your request was malformed".
            Error::LinkFailed(msg) => (StatusCode::SERVICE_UNAVAILABLE, "LINK_FAILED", msg.clone()),
            Error::HandshakeFailed(msg) => {
                (StatusCode::BAD_GATEWAY, "HANDSHAKE_FAILED", msg.clone())
            }
            Error::Unsupported(msg) => (StatusCode::CONFLICT, "AGENT_UNSUPPORTED", msg.clone()),
            Error::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Authentication required".to_string(),
            ),
            Error::Forbidden(msg) => (StatusCode::FORBIDDEN, "POLICY_VIOLATION", msg.clone()),
            Error::AttachmentInvalid(msg) => {
                (StatusCode::BAD_REQUEST, "ATTACHMENT_INVALID", msg.clone())
            }
            // 502 rather than 500: the failure is in a process this server started,
            // not in the server, and the distinction is what tells an operator whether
            // to look at their agent install or at Sylph.
            Error::AgentStartFailed(error) => (
                StatusCode::BAD_GATEWAY,
                "AGENT_START_FAILED",
                format!("{error:#}"),
            ),
            Error::SessionAlreadyPresent => (
                StatusCode::CONFLICT,
                "SESSION_ALREADY_PRESENT",
                "A session with that identity is already recorded".to_string(),
            ),
            Error::NotImplemented(msg) => {
                (StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED", msg.clone())
            }
            // The id is echoed because it is the client's own input and the whole fix:
            // a wrong native id is indistinguishable from a store that has been
            // pruned, and the message is where a user checks what they pasted.
            Error::NativeSessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                "NATIVE_SESSION_NOT_FOUND",
                format!("That agent's native store on this environment holds no session '{id}'"),
            ),
            Error::NativeSessionNotResumable => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "NATIVE_SESSION_NOT_RESUMABLE",
                "That native session recorded no working directory, so there is nowhere \
                 to run it. Adopting it would mean choosing a directory for an agent to \
                 write in, which only the recorded one can answer."
                    .to_string(),
            ),
            Error::EnvironmentUnreachable(id) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "ENVIRONMENT_UNREACHABLE",
                format!(
                    "No environment is connected for '{id}'. Sessions can only be read \
                     from a machine whose envagent is reachable."
                ),
            ),
            Error::InvalidRequest(msg) => (StatusCode::BAD_REQUEST, "INVALID_REQUEST", msg.clone()),
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
    }

    #[test]
    fn conflict_variants_map_to_409() {
        assert_eq!(Error::SessionAlreadyRunning.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn codes_match_the_openapi_enum() {
        assert_eq!(Error::SessionNotFound.code(), "SESSION_NOT_FOUND");
        assert_eq!(
            Error::SessionAlreadyRunning.code(),
            "SESSION_ALREADY_RUNNING"
        );
        assert_eq!(
            Error::AttachmentInvalid("x".into()).code(),
            "ATTACHMENT_INVALID"
        );
        assert_eq!(
            Error::AgentStartFailed(anyhow::anyhow!("x")).code(),
            "AGENT_START_FAILED"
        );
        assert_eq!(
            Error::SessionAlreadyPresent.code(),
            "SESSION_ALREADY_PRESENT"
        );
        assert_eq!(Error::NotImplemented("x".into()).code(), "NOT_IMPLEMENTED");
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
    /// One direction only: the schema may list codes no variant emits, because
    /// turn failure codes such as `TURN_INTERRUPTED` are recorded on turn rows
    /// rather than raised here, and this test must not stand in the way of a
    /// schema cleanup. That slack is not a licence to keep entries no producer
    /// exists for. The schema previously carried `RUNNER_NOT_FOUND`,
    /// `RUNNER_BROKEN`, `RUNNER_PROTOCOL_ERROR`, `RUNNER_START_FAILED`,
    /// `APPROVAL_NOT_AVAILABLE`, `COMMAND_NOT_SUPPORTED`, and
    /// `TRACE_NOT_RESUMABLE` — v1 casualties and one superseded spelling of
    /// `TRACE_SESSION_NOT_RESUMABLE` — and they are now deleted, since a code no
    /// server can emit only teaches a client to handle a case that cannot occur.
    /// The reverse — emitting a code no client is told about — is the bug this
    /// test exists to catch.
    #[test]
    fn every_emitted_code_is_published_in_the_openapi_enum() {
        let schema = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api-v1.openapi.json"),
        )
        .expect("the OpenAPI schema is part of the repo");
        let schema: serde_json::Value =
            serde_json::from_str(&schema).expect("the OpenAPI schema is valid JSON");

        let published = schema["components"]["schemas"]["ErrorResponse"]["properties"]["error"]
            ["properties"]["code"]["enum"]
            .as_array()
            .expect("the ErrorResponse schema declares a code enum")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("enum members are strings")
                    .to_string()
            })
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
            Error::FileNotFound,
            Error::SessionAlreadyRunning,
            Error::Unauthorized,
            Error::Forbidden("x".into()),
            Error::AttachmentInvalid("x".into()),
            Error::AgentStartFailed(anyhow::anyhow!("x")),
            Error::SessionAlreadyPresent,
            Error::NotImplemented("x".into()),
            Error::NativeSessionNotFound("x".into()),
            Error::NativeSessionNotResumable,
            Error::EnvironmentUnreachable("x".into()),
            Error::InvalidRequest("x".into()),
            Error::RouteNotFound,
            Error::MethodNotAllowed,
            Error::RequestNotFound,
            Error::RequestAlreadyResolved,
            Error::LinkGone,
            Error::LinkFailed("x".into()),
            Error::HandshakeFailed("x".into()),
            Error::Unsupported("x".into()),
            Error::Internal("x".into()),
        ];

        // The compile-time half. Every variant must appear here, and the
        // compiler enforces it because there is no `_` arm.
        for error in &all {
            match error {
                Error::Database(_)
                | Error::SessionNotFound
                | Error::FileNotFound
                | Error::SessionAlreadyRunning
                | Error::Unauthorized
                | Error::Forbidden(_)
                | Error::AttachmentInvalid(_)
                | Error::AgentStartFailed(_)
                | Error::SessionAlreadyPresent
                | Error::NotImplemented(_)
                | Error::NativeSessionNotFound(_)
                | Error::NativeSessionNotResumable
                | Error::EnvironmentUnreachable(_)
                | Error::InvalidRequest(_)
                | Error::RouteNotFound
                | Error::MethodNotAllowed
                | Error::RequestNotFound
                | Error::RequestAlreadyResolved
                | Error::LinkGone
                | Error::LinkFailed(_)
                | Error::HandshakeFailed(_)
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

    /// Starting a session fails in ways that need distinct fixes, so they must not
    /// share a status.
    ///
    /// This replaces `import_failures_have_distinct_statuses`, which pinned the four
    /// archive-shaped variants. The shape of that test is what survives: a caller has to
    /// be able to tell "the agent would not start" from "that session is already
    /// recorded" from "this is not built yet", because the three have nothing in common
    /// to do about them.
    #[test]
    fn starting_a_session_fails_with_distinct_statuses() {
        // 502 rather than 500: the failure is in a process this server started, which is
        // what tells an operator to look at their agent install rather than at Sylph.
        assert_eq!(
            Error::AgentStartFailed(anyhow::anyhow!("claude exited 1")).status(),
            StatusCode::BAD_GATEWAY
        );
        // The primary key refused a second row for one conversation.
        assert_eq!(Error::SessionAlreadyPresent.status(), StatusCode::CONFLICT);
        // Distinct from 500 on purpose: no retry completes an unimplemented operation,
        // so a client must not offer one.
        assert_eq!(
            Error::NotImplemented("adopt".into()).status(),
            StatusCode::NOT_IMPLEMENTED
        );
    }

    /// Adopting fails six ways, and **collapsing any pair sends a reader to the wrong
    /// layer** (`docs/api/sessions.md` § `POST /api/v1/sessions/adopt`).
    ///
    /// The six fixes are: nothing to do, widen configuration, wrong id, already done,
    /// unusable source, bring the machine online. Asserted as a set of distinct
    /// `(status, code)` pairs rather than one assertion each, because the property is
    /// that no two *share* an answer — which per-variant assertions cannot express and
    /// which is the thing that would actually regress.
    #[test]
    fn the_six_adopt_failures_are_distinguishable_from_each_other() {
        let failures = [
            // 400: the recorded workdir is gone, or the agent is not one this build runs.
            Error::InvalidRequest("no such agent".into()),
            // 403: well-formed, and refused by policy. The fix is `config.toml`.
            Error::Forbidden("outside the roots".into()),
            // 404: the vendor's store has no such session.
            Error::NativeSessionNotFound("9bdacb6e".into()),
            // 409: already adopted. Two rows for one conversation is what the key refuses.
            Error::SessionAlreadyPresent,
            // 422: the source is unusable — real session, no recorded directory.
            Error::NativeSessionNotResumable,
            // 503: the machine is not there. Nothing about the request is wrong.
            Error::EnvironmentUnreachable("env7fk2".into()),
        ];

        let mut seen = std::collections::HashSet::new();
        for failure in &failures {
            let (status, code, _) = failure.parts();
            assert!(
                seen.insert((status, code)),
                "`{code}` ({status}) is shared with another adopt failure, so a client \
                 cannot tell which of two fixes applies"
            );
        }

        // And the statuses are the ones the spec names, since a client branches on them.
        assert_eq!(
            Error::NativeSessionNotFound("x".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            Error::NativeSessionNotResumable.status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            Error::EnvironmentUnreachable("x".into()).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        // A native-store miss must not be reported as a Sylph-store miss: same status,
        // different code, and the code is what says which store to look in.
        assert_eq!(Error::SessionNotFound.status(), StatusCode::NOT_FOUND);
        assert_ne!(
            Error::SessionNotFound.code(),
            Error::NativeSessionNotFound("x".into()).code()
        );
    }

    /// A malformed session id is a 400, and a well-formed one naming nothing is a 404.
    ///
    /// The pair that keeps a client with a corrupted id from being told its session was
    /// deleted — `api::sessions::decode_id` is what draws the line, and it reports the
    /// decoder's own reason so the message names which part of the triple is wrong.
    #[test]
    fn a_malformed_session_id_is_refused_rather_than_reported_missing() {
        let malformed = crate::api::sessions::decode_id("not-a-triple")
            .expect_err("a bare string is not an encoded triple");
        assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
        assert_eq!(malformed.code(), "INVALID_REQUEST");

        assert_eq!(Error::SessionNotFound.status(), StatusCode::NOT_FOUND);

        // The decoder's reason is echoed, since that is what makes the failure
        // diagnosable — a client sees which component was wrong, not just "invalid".
        let body = malformed.to_response();
        assert!(
            body.error.message.contains("not-a-triple"),
            "the offending id is missing from the message: {}",
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
