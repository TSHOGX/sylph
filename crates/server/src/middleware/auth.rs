//! Authentication middleware: cookie validation and origin checks.
//!
//! The security model (docs/architecture.md, "Security Model"):
//! - Loopback-only: default bind is 127.0.0.1
//! - Process-local cookie: HttpOnly, SameSite=Strict
//! - Host header validation to reject DNS rebinding
//! - Origin validation for mutations
//!
//! A random token is generated at startup and set in a cookie on the first
//! unauthenticated request to `/api/v1/auth/token`. All subsequent requests
//! must present that cookie. The token rotates on every backend restart.

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::AppState;

const COOKIE_NAME: &str = "sylph_session";

#[derive(Clone, Debug)]
pub struct SubjectId(pub String);

/// List of endpoints exempt from cookie validation.
///
/// `/api/v1/health` is public by design (monitoring, uptime checks).
/// `/api/v1/auth/token` bootstraps the cookie.
const EXEMPT_PATHS: &[&str] = &[
    "/api/v1/health",
    "/api/v1/auth/token",
    "/api/v1/auth/register",
    "/api/v1/auth/login",
    // The envagent has no browser cookie. The pairing code is short-lived
    // and single-use, and this endpoint remains loopback-only.
    "/api/v1/environments/pair",
];

/// Validate that the request comes from a loopback client and carries a valid
/// session cookie (unless it's an exempt path).
pub async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let path = request.uri().path();

    // Host header validation: reject non-loopback to prevent DNS rebinding.
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        if !is_loopback_host(host) {
            return Err(Error::Forbidden(
                "Non-loopback Host header rejected".to_string(),
            ));
        }
    }

    // Origin validation applies to every mutation, including authentication and
    // pairing endpoints that are exempt from cookie validation.  Exemption is
    // only about who may call the route without a session; it must not create a
    // CSRF exception for a browser request.
    if is_mutation(request.method().as_str()) {
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
            if !is_loopback_origin(origin) {
                return Err(Error::Forbidden(format!(
                    "Non-loopback Origin rejected: {origin}"
                )));
            }
        }
    }

    // Pairing has no browser cookie, but a cross-origin page must not be able
    // to consume a one-shot pairing code.  The global mutation check above
    // covers pairing and invite issuance as well as auth mutations.
    let local_invite_issue = path.starts_with("/api/v1/environments/")
        && path.ends_with("/invites")
        && request.method() == axum::http::Method::POST;
    if EXEMPT_PATHS.contains(&path) || local_invite_issue {
        return Ok(next.run(request).await);
    }

    // Cookie validation: extract and compare against the process token.
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let token = extract_cookie(cookie_header, COOKIE_NAME);
    let presented = token.unwrap_or("");
    if constant_time_eq(presented, &state.config.cookie_secret) {
        let subject_id = state
            .store
            .subject_id_for_token_hash(&hash_token(presented))
            .await?
            .unwrap_or_else(|| "bootstrap".to_string());
        request.extensions_mut().insert(SubjectId(subject_id));
        enforce_session_capability(&state, request.uri().path(), request.method(), presented)
            .await?;
        return Ok(next.run(request).await);
    }
    let subject_id = state
        .store
        .subject_id_for_token_hash(&hash_token(presented))
        .await?
        .ok_or(Error::Unauthorized)?;
    request.extensions_mut().insert(SubjectId(subject_id));

    enforce_session_capability(&state, request.uri().path(), request.method(), presented).await?;

    Ok(next.run(request).await)
}

async fn enforce_session_capability(
    state: &AppState,
    path: &str,
    method: &axum::http::Method,
    presented: &str,
) -> Result<()> {
    let Some(rest) = path.strip_prefix("/api/v1/sessions/") else {
        return Ok(());
    };
    let mut segments = rest.split('/').filter(|value| !value.is_empty());
    let Some(first) = segments.next() else {
        return Ok(());
    };
    let encoded = if first == "draft" {
        segments.next().unwrap_or("")
    } else {
        first
    };
    if encoded.starts_with("draft-") || encoded.starts_with("remote-draft-") {
        let subject = state
            .store
            .subject_id_for_token_hash(&hash_token(presented))
            .await?
            .unwrap_or_else(|| "bootstrap".to_string());
        let capability = if *method == axum::http::Method::GET {
            "observe"
        } else {
            "control"
        };
        state
            .env_bridge
            .authorize_remote_draft(encoded, &subject, capability)
            .await
            .map_err(|error| {
                let message = format!("{error:#}");
                if message.contains("not found") {
                    Error::SessionNotFound
                } else {
                    Error::Forbidden(message)
                }
            })?;
        return Ok(());
    }
    let Ok(session) = sylph_protocol::SessionId::decode(encoded) else {
        return Ok(());
    };
    let suffix = rest.strip_prefix(encoded).unwrap_or("");
    let capability = if *method == axum::http::Method::GET || suffix == "/reread" {
        "observe"
    } else {
        "control"
    };
    let subject = state
        .store
        .subject_id_for_token_hash(&hash_token(presented))
        .await?
        .unwrap_or_else(|| "bootstrap".to_string());
    if !state
        .store
        .has_capability(&subject, session.environment.as_str(), capability)
        .await?
    {
        return Err(Error::Forbidden(format!(
            "{capability} capability required"
        )));
    }
    Ok(())
}

/// GET /api/v1/auth/token — first-run bootstrap endpoint that sets the cookie.
///
/// It is intentionally unauthenticated while the install has no registered
/// Subject, which keeps local development and fixtures usable. Once a real
/// account exists it returns 401 instead of minting a second identity; logout
/// therefore cannot silently restore broad bootstrap access on reload.
pub async fn bootstrap_token(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Preserve a registered Subject across page loads.  The bootstrap route is
    // intentionally unauthenticated, but it must not replace a valid account
    // cookie with the development identity merely because the AuthGate mounts.
    if let Some(presented) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| extract_cookie(value, COOKIE_NAME))
    {
        if let Ok(Some(subject_id)) = state
            .store
            .subject_id_for_token_hash(&hash_token(presented))
            .await
        {
            return (
                StatusCode::OK,
                HeaderMap::new(),
                Json(serde_json::json!({"ok": true, "subjectId": subject_id, "preserved": true})),
            );
        }
    }
    // Once an account has been provisioned, an unauthenticated browser must
    // not silently become the bootstrap Subject again.  This is what makes
    // logout durable for the lifetime of the process: AuthGate may probe this
    // endpoint after a reload, but the probe receives 401 until the user logs
    // in explicitly.
    let registered: Option<i64> = match sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM subjects WHERE id <> 'bootstrap' LIMIT 1",
    )
    .fetch_optional(state.store.read_pool())
    .await
    {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                HeaderMap::new(),
                Json(serde_json::json!({"ok": false, "reason": "authentication_unavailable"})),
            );
        }
    };
    if registered.is_some() {
        return (
            StatusCode::UNAUTHORIZED,
            HeaderMap::new(),
            Json(serde_json::json!({"ok": false, "reason": "authentication_required"})),
        );
    }

    let _ = state
        .store
        .upsert_subject(
            "bootstrap",
            "Bootstrap",
            &hash_token(&state.config.cookie_secret),
            true,
        )
        .await;
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        COOKIE_NAME,
        state.config.cookie_secret,
        86400 * 365 // 1 year
    );

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        cookie
            .parse()
            .expect("session cookie is valid header value"),
    );
    (
        StatusCode::OK,
        response_headers,
        Json(serde_json::json!({"ok": true, "subjectId": "bootstrap", "preserved": false})),
    )
}

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub name: String,
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<impl IntoResponse> {
    if req.name.trim().is_empty() {
        return Err(Error::InvalidRequest("name must not be empty".into()));
    }
    let token = uuid::Uuid::new_v4().to_string();
    let id = uuid::Uuid::new_v4().to_string();
    // The development bootstrap identity is always present, so counting all
    // subjects would make the first real registrant non-admin forever.  Provision
    // the first registered Subject as the server authority root while keeping the
    // process-local bootstrap account separate.
    // Make provisioning atomic with insertion. SQLite serializes the write,
    // so concurrent first registrations cannot both observe an empty table and
    // claim administrator status.
    let mut tx = state
        .store
        .raw_pool()
        .begin()
        .await
        .map_err(Error::Database)?;
    let is_admin: i64 = sqlx::query_scalar(
        "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM subjects WHERE id <> 'bootstrap') THEN 1 ELSE 0 END",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::Database)?;
    sqlx::query("INSERT INTO subjects (id, name, token_hash, is_admin, created_at_ms) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(req.name.trim())
        .bind(hash_token(&token))
        .bind(is_admin)
        .bind(crate::models::now_ms())
        .execute(&mut *tx)
        .await
        .map_err(Error::Database)?;
    tx.commit().await.map_err(Error::Database)?;
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict",
        COOKIE_NAME, token
    );
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, cookie)],
        Json(
            serde_json::json!({"id": id, "name": req.name, "isAdmin": is_admin == 1, "token": token}),
        ),
    ))
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub token: String,
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse> {
    if req.token.trim().is_empty() {
        return Err(Error::InvalidRequest("token must not be empty".into()));
    }
    let subject = state
        .store
        .subject_id_for_token_hash(&hash_token(&req.token))
        .await?
        .ok_or(Error::Unauthorized)?;
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict",
        COOKIE_NAME, req.token
    );
    Ok((
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(serde_json::json!({"id": subject})),
    ))
}

pub async fn logout() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("{}=; Path=/; HttpOnly; Max-Age=0", COOKIE_NAME),
        )],
    )
}

pub(crate) fn hash_token_for_storage(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    format!("{:x}", h.finalize())
}

fn hash_token(token: &str) -> String {
    hash_token_for_storage(token)
}

/// Whether an HTTP method is a mutation.
fn is_mutation(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

/// Whether a Host header value is loopback.
///
/// Accepts `127.0.0.1`, `localhost`, and `[::1]`, each with an optional port.
/// IPv6 literals are bracketed and contain colons, so the port cannot be split
/// off with a naive `split(':')` — the brackets are matched first.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();

    let hostname = if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6: take what is inside the brackets and ignore any
        // `:port` that follows the closing one.
        match rest.split_once(']') {
            Some((inside, after)) if after.is_empty() || after.starts_with(':') => inside,
            _ => return false,
        }
    } else {
        // A bare IPv6 address (multiple colons) is not valid in a Host header
        // without brackets, so anything with more than one colon is rejected.
        match host.split_once(':') {
            Some((name, port)) if !port.contains(':') => name,
            Some(_) => return false,
            None => host,
        }
    };

    matches!(hostname, "127.0.0.1" | "localhost" | "::1")
}

/// Whether an Origin header value is loopback.
///
/// Accepts `http://127.0.0.1`, `http://localhost`, and `http://[::1]`, each with
/// an optional port. HTTPS is not expected on loopback but is allowed.
///
/// The authority is parsed and compared through [`is_loopback_host`] rather than
/// prefix-matched: `http://localhost.evil.com` shares a prefix with a legitimate
/// origin but is a routable host the attacker controls.
fn is_loopback_origin(origin: &str) -> bool {
    let authority = match origin.split_once("://") {
        Some(("http" | "https", authority)) => authority,
        _ => return false,
    };

    // An Origin carries no path, but a stray one must not smuggle the check
    // past the authority comparison.
    if authority.contains('/') {
        return false;
    }

    is_loopback_host(authority)
}

/// Extract a named cookie from a Cookie header value.
///
/// Returns None if the cookie is absent or malformed. Does not decode
/// URL-encoded values — the token is UUID-like and doesn't need it.
fn extract_cookie<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    for pair in cookie_header.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix(name) {
            if let Some(value) = value.strip_prefix('=') {
                return Some(value.trim());
            }
        }
    }
    None
}

/// Constant-time string comparison to prevent timing attacks on the token.
///
/// A variable-time comparison leaks the position of the first differing byte,
/// and an attacker with network access could brute-force the token byte by
/// byte. Loopback-only binding makes this academic, but the defense is cheap.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= x ^ y;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_correctness() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("ab", "abc"));
    }

    #[test]
    fn extract_cookie_parsing() {
        assert_eq!(extract_cookie("session=abc123", "session"), Some("abc123"));
        assert_eq!(
            extract_cookie("foo=bar; session=xyz; baz=qux", "session"),
            Some("xyz")
        );
        assert_eq!(extract_cookie("session=", "session"), Some(""));
        assert_eq!(extract_cookie("foo=bar", "session"), None);
        assert_eq!(extract_cookie("", "session"), None);
    }

    #[test]
    fn loopback_host_validation() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.1:4317"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("localhost:4317"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("[::1]:4317"));

        assert!(!is_loopback_host("192.168.1.1"));
        assert!(!is_loopback_host("192.168.1.1:4317"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("evil.example.com:4317"));

        // A rebinding attempt that embeds a loopback name in a routable host
        // must not pass: only the hostname component is compared.
        assert!(!is_loopback_host("localhost.evil.com"));
        assert!(!is_loopback_host("127.0.0.1.evil.com"));

        // Malformed or unbracketed IPv6 is rejected rather than guessed at.
        assert!(!is_loopback_host("[::1"));
        assert!(!is_loopback_host("::1"));
        assert!(!is_loopback_host("[2001:db8::1]"));
    }

    #[test]
    fn loopback_origin_validation() {
        assert!(is_loopback_origin("http://127.0.0.1:3000"));
        assert!(is_loopback_origin("http://localhost:3000"));
        assert!(is_loopback_origin("http://localhost"));
        assert!(is_loopback_origin("http://[::1]:3000"));
        assert!(is_loopback_origin("https://127.0.0.1:3000"));

        assert!(!is_loopback_origin("http://192.168.1.1:3000"));
        assert!(!is_loopback_origin("http://example.com"));

        // Prefix matching would accept these: the attacker owns the real host.
        assert!(!is_loopback_origin("http://localhost.evil.com"));
        assert!(!is_loopback_origin("http://127.0.0.1.evil.com"));
        assert!(!is_loopback_origin("http://localhost@evil.com"));

        // Non-HTTP schemes and opaque origins are not loopback pages.
        assert!(!is_loopback_origin("file://localhost"));
        assert!(!is_loopback_origin("null"));
        assert!(!is_loopback_origin(""));
    }

    #[test]
    fn mutation_detection() {
        assert!(is_mutation("POST"));
        assert!(is_mutation("PUT"));
        assert!(is_mutation("PATCH"));
        assert!(is_mutation("DELETE"));
        assert!(!is_mutation("GET"));
        assert!(!is_mutation("HEAD"));
        assert!(!is_mutation("OPTIONS"));
    }
}
