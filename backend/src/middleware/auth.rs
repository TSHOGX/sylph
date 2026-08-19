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
use serde::Serialize;

use crate::error::Error;
use crate::AppState;

const COOKIE_NAME: &str = "sylph_session";

/// List of endpoints exempt from cookie validation.
///
/// `/api/v1/health` is public by design (monitoring, uptime checks).
/// `/api/v1/auth/token` bootstraps the cookie.
const EXEMPT_PATHS: &[&str] = &["/api/v1/health", "/api/v1/auth/token"];

/// Validate that the request comes from a loopback client and carries a valid
/// session cookie (unless it's an exempt path).
pub async fn auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, Error> {
    let path = request.uri().path();

    // Host header validation: reject non-loopback to prevent DNS rebinding.
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        if !is_loopback_host(host) {
            return Err(Error::Forbidden(
                "Non-loopback Host header rejected".to_string(),
            ));
        }
    }

    // Exempt paths skip cookie validation.
    if EXEMPT_PATHS.contains(&path) {
        return Ok(next.run(request).await);
    }

    // Origin validation for mutations: browser-initiated requests that would
    // change state must come from a loopback origin. Non-browser clients (curl,
    // CLI tools) never send Origin, and that's fine — the absence itself is
    // proof it's not a malicious page embedding our API.
    if is_mutation(request.method().as_str()) {
        if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
            if !is_loopback_origin(origin) {
                return Err(Error::Forbidden(format!(
                    "Non-loopback Origin rejected: {}",
                    origin
                )));
            }
        }
    }

    // Cookie validation: extract and compare against the process token.
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let token = extract_cookie(cookie_header, COOKIE_NAME);
    if !constant_time_eq(token.unwrap_or(""), &state.config.cookie_secret) {
        return Err(Error::Unauthorized);
    }

    Ok(next.run(request).await)
}

/// GET /api/v1/auth/token — bootstrap endpoint that sets the cookie.
///
/// Unauthenticated by design: any loopback client can obtain the token, which
/// is the correct policy for a process-local service. The real threat model is
/// browser-based attacks from other origins, not local processes that can
/// already read the database and spawn runners.
pub async fn bootstrap_token(State(state): State<AppState>) -> impl IntoResponse {
    let cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        COOKIE_NAME,
        state.config.cookie_secret,
        86400 * 365 // 1 year
    );

    (
        StatusCode::OK,
        [(header::SET_COOKIE, cookie)],
        Json(BootstrapResponse {
            ok: true,
            message: "Session cookie set".to_string(),
        }),
    )
}

#[derive(Serialize)]
struct BootstrapResponse {
    ok: bool,
    message: String,
}

/// Whether an HTTP method is a mutation.
fn is_mutation(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

/// Whether a Host header value is loopback.
///
/// Accepts `127.0.0.1`, `localhost`, `[::1]`, with optional port.
fn is_loopback_host(host: &str) -> bool {
    let host_without_port = host.split(':').next().unwrap_or(host);
    matches!(
        host_without_port,
        "127.0.0.1" | "localhost" | "[::1]" | "::1"
    )
}

/// Whether an Origin header value is loopback.
///
/// Accepts `http://127.0.0.1`, `http://localhost`, `http://[::1]`, with
/// optional port. HTTPS is not expected (no TLS on loopback), but is allowed.
fn is_loopback_origin(origin: &str) -> bool {
    origin.starts_with("http://127.0.0.1")
        || origin.starts_with("https://127.0.0.1")
        || origin.starts_with("http://localhost")
        || origin.starts_with("https://localhost")
        || origin.starts_with("http://[::1]")
        || origin.starts_with("https://[::1]")
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
        assert_eq!(
            extract_cookie("session=abc123", "session"),
            Some("abc123")
        );
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
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn loopback_origin_validation() {
        assert!(is_loopback_origin("http://127.0.0.1:3000"));
        assert!(is_loopback_origin("http://localhost:3000"));
        assert!(is_loopback_origin("http://[::1]:3000"));
        assert!(is_loopback_origin("https://127.0.0.1:3000"));
        assert!(!is_loopback_origin("http://192.168.1.1:3000"));
        assert!(!is_loopback_origin("http://example.com"));
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
