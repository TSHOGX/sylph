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

// ------------------------------------------------------------------ re-reading
//
// `POST /sessions/{id}/reread` — one pass over a store Sylph already holds a row for.
//
// The three properties below are the ones that make the operation worth having, and each
// fails for a different reason: the pass has to *find* what the vendor appended, it must not
// re-add what a previous pass already journaled, and asking twice has to be two passes
// rather than one cached answer. A single test covering all three would go red without
// saying which promise broke.

/// Percent-encode one path segment. A native id is the vendor's value and may contain `/`.
fn segment(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Adopt the stored session, wait for its events to land, and return its encoded id.
///
/// **The wait is load-bearing, not defensive.** `POST /sessions/adopt` answers `201` when
/// the envagent has *accepted* the command; the parsed events are spooled to the server
/// afterwards. A re-read issued before they arrive finds an empty journal, suppresses
/// nothing, and re-journals the whole adopted prefix — so a dedup test would fail against
/// correct product code, which is the most expensive kind of red. A test that instead
/// happens to read the transcript first passes for a reason it does not state, and that is
/// a flake waiting for a slower machine.
async fn adopt_stored(backend: &TestBackend) -> String {
    let environment = backend.environment_id().await;
    let native_id = common::MockAgents::stored_native_id();
    let created = backend
        .post_authed(
            "/api/v1/sessions/adopt",
            json!({"environment": environment, "agent": "mock", "nativeId": native_id}),
        )
        .await;
    assert_eq!(
        created.status(),
        StatusCode::CREATED,
        "{}",
        common::body_string(created).await
    );
    let encoded = format!("{environment}:mock:{native_id}");
    // Two waits, because adoption finishes in two stages and a re-read needs both.
    //
    // First the pass itself has to arrive: `POST /sessions/adopt` answers `201` when the
    // envagent *accepted* the command, and the parsed events are spooled afterwards.
    // Polling the transcript is the available signal, since it is served over the same API
    // a client uses.
    for _ in 0..100 {
        if transcript(backend, &encoded)
            .await
            .iter()
            .any(|text| text == "read from the store")
        {
            // Then the ephemeral rows have to be flushed. `JournalWriter` holds them in a
            // ~50 ms window (`crate::db::writer`) and a `turn`/`started` derives
            // `Ephemeral`, so `events` is still short one row at this point — while the
            // transcript, assembled from stable rows only, already looks complete. A
            // re-read in that gap builds its suppression set without the ephemeral row,
            // judges it new, and re-journals one copy per click.
            //
            // A sleep rather than a poll, and deliberately: no reader-facing route exposes
            // an unflushed ephemeral row, which is the property that makes them ephemeral.
            // Waiting on the transcript again would re-observe the signal already given.
            // The bound is the writer's own window with room to spare, and it is stated
            // here rather than tuned by trial so a change to that window has one place to
            // look.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            return encoded;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the adopted session's events never reached the journal");
}

/// Every event a session holds, across both tiers, oldest first.
async fn transcript(backend: &TestBackend, encoded: &str) -> Vec<String> {
    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{}/turns", segment(encoded)))
            .await,
    )
    .await;
    body["turns"]
        .as_array()
        .expect("a turn list")
        .iter()
        .flat_map(|turn| {
            turn["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
        })
        .filter_map(|event| event["text"].as_str().map(str::to_string))
        .filter(|text| !text.is_empty())
        .collect()
}

/// A re-read picks up what the vendor wrote after the adopt pass.
///
/// **This is the defect the endpoint exists for.** "Refresh transcript" invalidated a
/// client-side query cache and nothing else, so a session adopted from a terminal could
/// never gain a line the vendor appended afterwards — no amount of clicking reached the
/// store. The assertion is on the *transcript*, not on the response counts, because a count
/// can be right while nothing was journaled.
#[tokio::test]
async fn a_reread_ingests_what_the_vendor_appended_after_the_adopt_pass() {
    let backend = TestBackend::start().await;
    let encoded = adopt_stored(&backend).await;

    let before = transcript(&backend, &encoded).await;
    assert!(
        !before.iter().any(|text| text == "written after adoption"),
        "the store has not grown yet: {before:?}"
    );

    // The vendor writes more into its own transcript.
    backend.agents.grow_stored_session("written after adoption");

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{}/reread", segment(&encoded)),
            json!({}),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        common::body_string(response).await
    );

    let after = transcript(&backend, &encoded).await;
    assert!(
        after.iter().any(|text| text == "written after adoption"),
        "the re-read did not ingest the appended event: {after:?}"
    );
}

/// Re-reading twice does not journal the same event twice.
///
/// `Adapter::adopt` has no cursor and restarts at the head of the store, so the second pass
/// re-emits every event the first one saw. Nothing in the raw tier can suppress that for
/// most agents — only `claude` emits native records at all, and `codex` and `pi` carry no
/// per-event vendor id — so suppression is content-based and this is what pins it.
///
/// Asserted as an exact count of one text rather than a total, so a transcript that grows
/// for some unrelated reason cannot mask a duplicate of the line under test.
#[tokio::test]
async fn re_reading_twice_does_not_duplicate_an_event_already_held() {
    let backend = TestBackend::start().await;
    let encoded = adopt_stored(&backend).await;
    backend.agents.grow_stored_session("appended once");

    for _ in 0..2 {
        let response = backend
            .post_authed(
                &format!("/api/v1/sessions/{}/reread", segment(&encoded)),
                json!({}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let texts = transcript(&backend, &encoded).await;
    let appended = texts.iter().filter(|text| *text == "appended once").count();
    assert_eq!(appended, 1, "the event was journaled more than once: {texts:?}");
    let original = texts
        .iter()
        .filter(|text| *text == "read from the store")
        .count();
    // Two, not one: the fixture's own transcript carries that text on an assistant event
    // *and* on the vendor's turn close. What matters is that neither was re-added.
    assert_eq!(
        original, 2,
        "the adopted prefix was re-journaled by a later pass: {texts:?}"
    );
}

/// Asking twice runs twice, and the second answer describes the second store.
///
/// A re-read reads a file and writes no vendor state, so it looks replay-safe — and a
/// request id hashed from its payload would make it so, since the payload is identical on
/// every call. The second click would then address the first pass's durable result and
/// report counts from before the bytes it was clicked to collect existed: a refresh that
/// returns a cached answer, which is the original defect one layer down.
///
/// The window is opened deliberately: the store grows *between* the two calls, so a replayed
/// second answer is observably wrong rather than merely suspicious.
#[tokio::test]
async fn re_reading_after_the_store_grows_again_reports_the_second_pass_not_the_first() {
    let backend = TestBackend::start().await;
    let encoded = adopt_stored(&backend).await;
    let path = format!("/api/v1/sessions/{}/reread", segment(&encoded));

    // A first pass with nothing new. It must succeed and say it added nothing, which is the
    // "nothing happened" report — not an error, and not silence.
    let first = json_body(backend.post_authed(&path, json!({})).await).await;
    assert_eq!(first["reread"]["ingested"], 0, "{first}");
    assert!(
        first["reread"]["alreadyHeld"].as_u64().unwrap_or(0) > 0,
        "a pass over an unchanged store recognizes what it already holds: {first}"
    );

    // Now the vendor appends, and the same request is made again.
    backend.agents.grow_stored_session("arrived between passes");
    let second = json_body(backend.post_authed(&path, json!({})).await).await;

    assert_eq!(
        second["reread"]["ingested"], 1,
        "the second call replayed the first call's result instead of running: {second}"
    );
}

/// A session Sylph does not hold cannot be re-read, and says so as itself.
///
/// Distinct from the store having dropped a session Sylph *does* hold: one is a wrong id and
/// the other means Sylph's copy is now the only one. Collapsing them sends a reader to the
/// wrong layer.
#[tokio::test]
async fn re_reading_a_session_sylph_does_not_hold_is_a_session_not_found() {
    let backend = TestBackend::start().await;
    let environment = backend.environment_id().await;

    let response = backend
        .post_authed(
            &format!(
                "/api/v1/sessions/{}/reread",
                segment(&format!("{environment}:mock:no-such-session"))
            ),
            json!({}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "SESSION_NOT_FOUND"
    );
}

/// A row whose native session the vendor has dropped reports the *store's* absence.
///
/// The fix differs from every other `404` here: nothing the caller can change brings it
/// back, and Sylph's transcript is now the only copy of that conversation.
#[tokio::test]
async fn re_reading_a_row_whose_native_session_is_gone_reports_the_store_not_the_row() {
    let backend = TestBackend::start().await;
    // A row created by Sylph, so it exists — and its native id is not the one the mock's
    // store enumerates, so the store cannot answer for it.
    let encoded = backend.create_session("mock").await;

    let response = backend
        .post_authed(
            &format!("/api/v1/sessions/{}/reread", segment(&encoded)),
            json!({}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "NATIVE_SESSION_NOT_FOUND"
    );
}

/// A re-read leaves `fidelity` exactly as the adopt pass journaled it.
///
/// `fidelity` is a property of the record, so a second pass over the same store cannot raise
/// or lower it. The failure this pins is a downgrade: a renderer reads the field before
/// deciding whether it may draw a waterfall, so a partial pass that rewrote `blocks` to
/// `messages` would silently stop drawing durations the data still supports.
#[tokio::test]
async fn a_reread_does_not_change_the_sessions_fidelity() {
    let backend = TestBackend::start().await;
    let encoded = adopt_stored(&backend).await;
    let path = format!("/api/v1/sessions/{}", segment(&encoded));

    let before = json_body(backend.get_authed(&path).await).await;
    assert_eq!(before["session"]["fidelity"], "blocks", "{before}");

    backend.agents.grow_stored_session("more from the vendor");
    let response = backend
        .post_authed(&format!("{path}/reread"), json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let after = json_body(backend.get_authed(&path).await).await;
    assert_eq!(after["session"]["fidelity"], "blocks", "{after}");
}

/// A turn the vendor appended after adoption survives a re-read.
///
/// The counterpart to the duplicate-suppression tests, and it guards the opposite mistake.
/// A `turn`/`started` is `Ephemeral`, so consolidation never folds it into a block and
/// `delete_turn_events` removes the journal row it lived in — once its turn is folded the
/// event is in neither tier and cannot be recognized by content. Suppression therefore keys
/// on whether a container is already open, and a rule that reads "an opener during a
/// re-read is a duplicate" would silently swallow every genuinely new turn: the transcript
/// would stop growing while the endpoint kept reporting success.
///
/// Asserted on the appended turn's **start stamp**, which is the field the opener carries and
/// the only one that changes. Suppressing an opener does not lose the turn — any event with no
/// open container opens one — so a turn count cannot see this: the container simply opens at
/// the *reply's* timestamp instead of the vendor's `started`. That silently shortens the turn,
/// and a duration read from it is then a measurement of the wrong interval, which is exactly
/// the class `docs/README.md` says fidelity exists to prevent.
#[tokio::test]
async fn a_turn_appended_after_adoption_is_journaled_as_its_own_turn() {
    let backend = TestBackend::start().await;
    let encoded = adopt_stored(&backend).await;

    let turns_before = turn_count(&backend, &encoded).await;

    // A whole new turn, bracketed the way a vendor writes one.
    backend.agents.grow_stored_session("turn:a second turn arrived");

    let response = json_body(
        backend
            .post_authed(
                &format!("/api/v1/sessions/{}/reread", segment(&encoded)),
                json!({}),
            )
            .await,
    )
    .await;
    assert!(
        response["reread"]["ingested"].as_u64().unwrap_or(0) > 0,
        "a new turn is new work: {response}"
    );

    assert_eq!(
        turn_count(&backend, &encoded).await,
        turns_before + 1,
        "the appended turn was folded into the existing one instead of opening its own"
    );
    let texts = transcript(&backend, &encoded).await;
    assert_eq!(
        texts
            .iter()
            .filter(|text| *text == "a second turn arrived")
            .count(),
        // Twice: the fixture carries the text on the assistant event and on the turn close,
        // the same shape the adopted prefix has.
        2,
        "the appended turn's content is missing: {texts:?}"
    );

    // The fixture brackets an appended turn at `AT_MS + 10`, with its reply one millisecond
    // later. A kept opener starts the turn at the bracket; a swallowed one starts it at the
    // reply, one millisecond late and attributed to the wrong event.
    let newest = newest_turn(&backend, &encoded).await;
    assert_eq!(
        newest["startedAtMs"].as_i64(),
        Some(1_700_000_000_010),
        "the appended turn's opener was suppressed, so its start is the reply's stamp: {newest}"
    );
}

/// The most recently started turn, as the API reports it.
async fn newest_turn(backend: &TestBackend, encoded: &str) -> serde_json::Value {
    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{}/turns", segment(encoded)))
            .await,
    )
    .await;
    body["turns"]
        .as_array()
        .and_then(|turns| turns.last())
        .map(|turn| turn["turn"].clone())
        .expect("a session with turns")
}

/// How many turns this session holds.
async fn turn_count(backend: &TestBackend, encoded: &str) -> usize {
    let body = json_body(
        backend
            .get_authed(&format!("/api/v1/sessions/{}/turns", segment(encoded)))
            .await,
    )
    .await;
    body["turns"].as_array().map_or(0, |turns| turns.len())
}
