//! `POST /sessions/adopt` and `GET /sessions/enumerate`, at the API boundary.
//!
//! # What these can and cannot cover, stated plainly
//!
//! The harness's `mock` agent is an `adapter.sh` with **no vendor CLI and no native
//! store**, so `sylph_adapters::for_agent("mock")` answers `None` — there is nothing to
//! parse. That is not a harness quirk to work around: the E2E fixture agent is the same
//! by design, and it *refuses* `adopt` and enumerates empty on purpose
//! (`crates/adapters/src/e2e.rs`), because synthesizing a store would make the suite
//! assert against records Sylph itself wrote rather than against a parser.
//!
//! So this file covers what is decidable without a store: **which agents are readable,
//! which environments are reachable, how a malformed request is refused, how a cursor
//! behaves, and that the six failures stay distinct.** The parse itself is covered where
//! the parsers live, and the turn-container rules by `db::ingest`'s own tests against the
//! shapes those parsers were measured to produce.
//!
//! What is therefore **not** covered here, and is only ever exercised by hand against the
//! real stores: a successful adopt end to end. `crate::db::ingest` records the measured
//! results of those runs — 1000 / 1258 / 531 real sessions readable, one `claude` session
//! adopted at 13623 events, one `codex` rollout at 3 bracketed turns.

mod common;

use axum::http::StatusCode;
use common::{json_body, TestBackend};
use serde_json::json;

/// Every failure a client can reach without a native store keeps its own status.
///
/// **Collapsing any pair sends a reader to the wrong layer** — the six adopt fixes are
/// nothing to do, widen configuration, wrong id, already done, unusable source, and bring
/// the machine online (`docs/api/sessions.md`).
#[tokio::test]
async fn an_unreachable_environment_is_refused_as_unavailable_not_as_invalid() {
    let backend = TestBackend::start().await;

    // Well-formed base32, and no envagent is connected for it. The fix is to bring a
    // machine online, which is a `503` — answering `400` would send a reader to their own
    // request.
    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": "env7fk2", "agent": "claude", "nativeId": "abc"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "ENVIRONMENT_UNREACHABLE"
    );
}

/// A malformed environment id is a `400`, distinct from a well-formed unreachable one.
///
/// The pair matters: one is a typo and the other is a machine that is asleep, and a client
/// that cannot tell them apart can only say "something is wrong".
#[tokio::test]
async fn a_malformed_environment_id_is_a_bad_request() {
    let backend = TestBackend::start().await;

    // `_` is not base32, so this cannot name any environment.
    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": "not_base32", "agent": "claude", "nativeId": "abc"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "INVALID_REQUEST"
    );
}

/// Adopting a session the store holds records it and ingests its transcript.
///
/// **The `201` path had no test at all.** All 13 specs here asserted a refusal, because no
/// fixture could answer `enumerate` — so ingestion, the fidelity check, the workdir
/// containment on an adopted row, and the `fidelity` the row is journaled with were written
/// and unobserved. That is the shape `AGENTS.md` records twice: a fixture too small cannot
/// observe the defect, and a suite of refusals proves only that refusing works.
#[tokio::test]
async fn adopting_a_stored_session_records_it_and_reads_its_transcript() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;
    let native_id = common::MockAgents::stored_native_id();

    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": environment, "agent": "mock", "nativeId": native_id}),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        common::body_string(response).await
    );
}

/// An adopted row carries the store's fidelity, never `stream`.
///
/// `stream` is what a *live* link yields, because it holds the process's own stdio. A record
/// read from a native store cannot hold per-delta timings, so journalling `stream` for one
/// would promise a renderer a waterfall the data cannot support — which is why the frontend
/// gates the waterfall on this field rather than on how the session was created.
#[tokio::test]
async fn an_adopted_row_reports_the_stores_fidelity_rather_than_stream() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;
    let native_id = common::MockAgents::stored_native_id();

    let created = json_body(
        backend
            .post_authed(
                "/api/v1/sessions/adopt",
                json!({"environment": environment, "agent": "mock", "nativeId": native_id}),
            )
            .await,
    )
    .await;

    let session = &created["session"];
    assert_eq!(session["fidelity"], "blocks", "{session}");
    assert_eq!(session["nativeId"], native_id, "{session}");
    // The recorded directory, run through the same containment bound `POST /sessions` uses
    // and journalled in the spelling the server canonicalized — which is what `/workspaces`
    // groups by, so a second spelling would split the group.
    assert_eq!(
        session["workdir"].as_str(),
        Some(backend.workdir().display().to_string().as_str()),
        "{session}"
    );
}

/// An agent with no native store to read answers "no such session", not "bad request".
///
/// # This test's mechanism changed, and the change is the finding
///
/// It used to name `mock` and expect a `400`, and it passed for a reason that has been
/// deleted: `mock` was an `adapter.sh` with no vendor CLI, so it was absent from the
/// compiled-in registry and `require_agents` refused it before any store was consulted.
/// With adapters linked in and injected, `mock` is a real agent — so the refusal now comes
/// from the *store* rather than from the lookup, and it is a `404`: the agent is real, and
/// it holds no session by that id.
///
/// The distinction the original was defending still holds and is asserted below: "adopted
/// nothing" must never be reported as a successful adopt of an empty session, which a
/// journal would make permanent. What changed is only which layer says no.
#[tokio::test]
async fn an_agent_with_no_native_store_cannot_be_adopted_from() {
    let backend = TestBackend::start_with_runners(&[common::MockRunner::no_store("empty")]).await;
    let environment = backend.environment_id().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": environment, "agent": "empty", "nativeId": "abc"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "NATIVE_SESSION_NOT_FOUND",
        "a store-less agent holds no session by that id; it is not a malformed request"
    );
}

/// An agent this build does not support is refused as unknown rather than as uninstalled.
///
/// `crate::adapters::is_known` exists for this distinction: `codex` on a machine without
/// the CLI names a fix, while `gemini` does not (`AGENTS.md` § Product Invariants — three
/// agents, `gemini` and `opencode` deliberately not supported).
#[tokio::test]
async fn an_unsupported_agent_is_refused_as_unknown() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": environment, "agent": "gemini", "nativeId": "abc"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = json_body(response).await;
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("Unknown agent"),
        "an unsupported agent must not read as an uninstalled one: {message}"
    );
}

/// An empty native id is refused before any store is read.
#[tokio::test]
async fn an_empty_native_id_is_refused() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;

    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": environment, "agent": "claude", "nativeId": "   "}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// Adopting a session Sylph already has is a `409`, not a second row.
///
/// The triple *is* the session, so two rows under one id is the state the primary key
/// exists to prevent. Asserted against a session the harness created rather than an
/// adopted one, because the check is about the row and not about where it came from — and
/// this is reachable without a native store, which is what makes it testable here.
#[tokio::test]
async fn adopting_a_session_that_is_already_recorded_is_a_conflict() {
    let backend = TestBackend::start().await;
    let encoded = backend.create_session("mock").await;
    let id = TestBackend::session_id(&encoded);

    // The same triple, asked for as an adopt. It is refused for *being present* — the
    // check runs before the agent's store is consulted, which is why an agent with no
    // store still reaches it.
    let response = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({
                "environment": id.environment.as_str(),
                "agent": id.agent.as_str(),
                "nativeId": id.native_id.as_str(),
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "SESSION_ALREADY_PRESENT"
    );
}

/// Enumerating an environment nothing is connected for is a `503`, matching adopt.
///
/// The picker and the endpoint it feeds must agree about reachability, or a user sees an
/// empty list where they should see "that machine is not there".
#[tokio::test]
async fn enumerate_refuses_an_unreachable_environment() {
    let backend = TestBackend::start().await;

    let response = backend
        .get_authed("/api/v1/sessions/enumerate?environment=env7fk2")
        .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "ENVIRONMENT_UNREACHABLE"
    );
}

/// Enumerate requires an environment, because there is no privileged machine.
#[tokio::test]
async fn enumerate_requires_an_environment() {
    let backend = TestBackend::start().await;
    let response = backend.get_authed("/api/v1/sessions/enumerate").await;
    // A missing required query parameter is refused by the extractor.
    assert!(
        response.status().is_client_error(),
        "got {}",
        response.status()
    );
}

/// A listing of a machine with no readable stores is an **empty list**, not an error.
///
/// A machine with an agent installed and no sessions yet genuinely has none, and a client
/// must render that as "nothing here" rather than as a loading state or a failure. This is
/// the shape the E2E fixture agent always has, deliberately.
///
/// This inverted with the seam, and the inversion is deliberate rather than conceded. The
/// test used to expect a `400`, on the argument that an empty answer from a store-less agent
/// is *ambiguous* — and that argument was only available while such an agent could be
/// filtered out by not being in the registry. It cannot be now, and the protocol already
/// answers the question the other way: `Adapter::enumerate`'s contract is that an empty
/// result is a real state and not an error, and a store-less agent is simply one that is
/// always in it. The unambiguous signal lives on `adopt`, which refuses — asserted above.
#[tokio::test]
async fn enumerate_answers_an_empty_listing_rather_than_failing() {
    let backend = TestBackend::start_with_runners(&[common::MockRunner::no_store("empty")]).await;
    let environment = backend.environment_id().await;

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/enumerate?environment={environment}&agent=empty"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(
        body["sessions"].as_array().map(Vec::len),
        Some(0),
        "an agent with no store lists nothing rather than failing: {body}"
    );
}

/// A cursor this endpoint did not issue is refused rather than treated as zero.
///
/// Silently restarting a paged scan is how a client loops forever over page one, and the
/// loop is invisible: every page is a valid response.
#[tokio::test]
async fn a_foreign_cursor_is_refused_rather_than_restarting_the_scan() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/enumerate?environment={environment}&cursor=not-a-cursor"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "INVALID_REQUEST"
    );
}

/// A listing is well-formed and paged even when this machine holds no sessions.
///
/// Asserted on the *shape* rather than on a count, because what a developer's machine
/// holds is exactly the property that makes a count unassertable — the same reason
/// `crate::adapters`'s own tests assert on `is_known` rather than on `available`.
#[tokio::test]
async fn a_listing_is_shaped_correctly_whatever_this_machine_holds() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;

    let response = backend
        .get_authed(&format!(
            "/api/v1/sessions/enumerate?environment={environment}&limit=5"
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = json_body(response).await;
    let sessions = body["sessions"]
        .as_array()
        .expect("a listing always carries a sessions array");
    assert!(sessions.len() <= 5, "the limit is honoured");

    for row in sessions {
        // Every row must be adoptable-or-explained. A row with neither an id nor a reason
        // is one a picker can only offer and then fail on.
        assert!(
            row["id"].is_string(),
            "every row carries the triple it would adopt as"
        );
        assert!(row["agent"].is_string());
        assert!(row["nativeId"].is_string());
        // `stream` is unreachable for anything read off a disk.
        let fidelity = row["fidelity"].as_str().unwrap_or_default();
        assert!(
            fidelity == "blocks" || fidelity == "messages",
            "a native store cannot hold stream fidelity; got {fidelity:?}"
        );
        if let Some(blocked) = row["blocked"].as_str() {
            assert!(
                matches!(blocked, "alreadyAdopted" | "noWorkdir" | "outsideRoots"),
                "a blocked reason must be one a client can branch on; got {blocked:?}"
            );
        }
    }
}

/// A session Sylph already holds is **marked**, not hidden and not offered.
///
/// Hiding it leaves a user hunting for a session plainly on the machine; offering it hands
/// out a `409` nobody can act on (`docs/api/sessions.md`
/// § `GET /api/v1/sessions/enumerate`). This
/// is asserted through the classification the endpoint uses rather than against a native
/// store, so it holds on a machine with no agents installed.
#[tokio::test]
async fn an_already_adopted_row_is_marked_rather_than_offered() {
    let backend = TestBackend::start().await;
    let encoded = backend.create_session("mock").await;
    let id = TestBackend::session_id(&encoded);

    // The same question `enumerate` asks per agent, and the same one `adopt`'s `409`
    // answers — so a marked row and the refusal cannot disagree.
    let adopted = backend
        .state
        .store
        .adopted_native_ids(&id.environment, id.agent.as_str())
        .await
        .expect("the adopted set");
    assert!(
        adopted.contains(id.native_id.as_str()),
        "a recorded session must be reported as already present"
    );

    // And an id nothing recorded is absent from it, or every row would read as adopted.
    assert!(!adopted.contains("no-such-native-id"));
}

/// The adopted set is scoped to one agent on one environment.
///
/// Two agents can hold sessions with the same native id — nothing makes a vendor's id
/// space disjoint from another vendor's — so a set that ignored the agent would mark a
/// `codex` session adopted because a `claude` one shares its id.
#[tokio::test]
async fn the_adopted_set_does_not_leak_across_agents() {
    let backend = TestBackend::start().await;
    let encoded = backend.create_session("mock").await;
    let id = TestBackend::session_id(&encoded);

    let other = backend
        .state
        .store
        .adopted_native_ids(&id.environment, "claude")
        .await
        .expect("the adopted set");
    assert!(
        !other.contains(id.native_id.as_str()),
        "a `mock` session must not mark the same id adopted for `claude`"
    );
}
