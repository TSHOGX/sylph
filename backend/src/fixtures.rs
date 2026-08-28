//! Shared test fixtures.

/// A session id for fixtures.
///
/// One helper rather than a literal per test: a `SessionId` cannot be built from a
/// string without going through the protocol's own parsers, which is the point — the
/// type is what stops a bare string reaching a query.
pub(crate) fn test_session_id(native: &str) -> sylph_protocol::SessionId {
    sylph_protocol::SessionId::new(
        sylph_protocol::EnvironmentId::parse("env7fk2").expect("base32"),
        sylph_protocol::AgentName::parse("claude").expect("agent"),
        sylph_protocol::NativeId::new(native),
    )
}
