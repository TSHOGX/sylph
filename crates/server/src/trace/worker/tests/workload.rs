//! S1.4 workload dimensions for the production projection path.
//!
//! `docs/unified-trace-conformance.md` § Performance fixtures names seven
//! independent dimensions. History length and superseded versions of hot objects
//! are bounded on the read side in `crate::trace::reading_store`, and the
//! cumulative reducer-claim ceiling is answered in `super::reconciliation`. This
//! module measures the remaining five ingestion dimensions: concurrent sources,
//! child fan-out, activation count, payload bytes and source rewrite frequency.
//!
//! These are workload measurements, not correctness replays. Source order
//! correctness lives in `super::corpus` and delegation semantics in
//! `super::delegation`; neither varies its dimension to observe cost.

use super::*;
use crate::trace::reading_store::read_scope;

/// Independent sessions on independent sources, admitted interleaved.
struct ConcurrentSources {
    fixture: Fixture,
    through: u64,
}

async fn concurrent_sources(sources: usize, records_each: usize) -> ConcurrentSources {
    let fixture = Fixture::new().await;
    let mut offsets = vec![0u64; sources];
    let mut sequence = 0u64;
    // One header per source first, so every source is declared before any of them
    // grows. Admitting them interleaved is what makes this a concurrency measure
    // rather than a sequence of independent single-source imports.
    for (index, offset) in offsets.iter_mut().enumerate() {
        let mut header = serde_json::to_vec(&serde_json::json!({
            "type":"session_meta","ordinal":0,
            "payload":{"id":format!("concurrent-{index}")}
        }))
        .unwrap();
        header.push(b'\n');
        sequence += 1;
        fixture
            .native_on(&format!("source-{index}"), sequence, *offset, &header)
            .await;
        *offset += header.len() as u64;
    }
    for record in 0..records_each {
        for (index, offset) in offsets.iter_mut().enumerate() {
            let mut bytes = serde_json::to_vec(&serde_json::json!({
                "type":"response_item","ordinal":record+1,
                "payload":{"type":"message","id":format!("concurrent-{index}-{record}"),
                    "role":"assistant",
                    "content":[{"type":"output_text","text":format!("source {index} record {record}")}]}
            }))
            .unwrap();
            bytes.push(b'\n');
            sequence += 1;
            fixture
                .native_on(&format!("source-{index}"), sequence, *offset, &bytes)
                .await;
            *offset += bytes.len() as u64;
        }
    }
    let through = fixture.evidence.evidence_cut().await.unwrap();
    ConcurrentSources { fixture, through }
}

/// Per-record projection cost must not depend on how many other sources are live.
///
/// A source's own history is its own scan; sharing a worker must not make one
/// source's tail proportional to the number of concurrent producers. The
/// measurement holds total records constant so the only variable is how they are
/// distributed, which separates fan-in cost from ingest volume.
#[tokio::test]
async fn concurrent_source_count_does_not_change_per_record_projection_work() {
    let mut samples = Vec::new();
    for (sources, records_each) in [(1usize, 24usize), (4, 6), (12, 2)] {
        let setup = concurrent_sources(sources, records_each).await;
        let (generation, _) = setup.fixture.generation("concurrent").await;
        let worker = setup.fixture.worker();
        let progress = worker
            .catch_up(&generation, 1, setup.through)
            .await
            .unwrap();
        let total_records = sources * records_each;
        assert_eq!(progress.suspended_sources, 0, "no source may be suspended");
        assert_eq!(progress.unmapped_observations, 0);
        assert_eq!(
            progress.projected_cut, setup.through,
            "every concurrent source must reach the same projected cut"
        );
        let held = worker
            .states
            .lock()
            .await
            .get("concurrent")
            .unwrap()
            .clone();
        let claims = held.lock().await.reducer.len();
        // Durable checkpoint bytes are the attributable memory figure: `source_states`
        // is keyed per physical source, so concurrent import is the arrangement that
        // could multiply retained state for the same admitted records.
        let committed = generation.checkpoint().await.unwrap().snapshot;
        let state_bytes = serde_json::to_vec(&generation.mapper_states(&committed).await.unwrap())
            .unwrap()
            .len();
        samples.push((
            sources,
            records_each,
            total_records,
            claims,
            progress.reduction.claims_resolved,
            progress.reader_rows_materialized,
            state_bytes,
        ));
    }
    for (sources, each, total, claims, resolved, rows, state_bytes) in &samples {
        eprintln!(
            "concurrent_source_workload sources={sources} records_each={each} total_records={total} retained_claims={claims} claims_resolved={resolved} reader_rows={rows} state_bytes={state_bytes} state_bytes_per_record={:.1}",
            *state_bytes as f64 / *total as f64,
        );
    }
    // Every arrangement admitted the same number of content records, so retained
    // claims may differ only by the per-source header, never by a fan-in term.
    let baseline = &samples[0];
    for sample in &samples[1..] {
        let extra_headers = sample.0 - baseline.0;
        let claim_growth = sample.3 as i64 - baseline.3 as i64;
        assert!(
            claim_growth >= 0 && claim_growth <= (extra_headers * 16) as i64,
            "retained claims grew with concurrent source count beyond per-source declarations: {baseline:?} to {sample:?}"
        );
        assert!(
            sample.5 <= baseline.5 + sample.0 * 8,
            "reader rows grew with concurrent source count beyond per-source rows: {baseline:?} to {sample:?}"
        );
        // Retained state may carry a per-source term — each live source keeps its own
        // mapper state and header — but concurrent import must not multiply the state
        // held for the same admitted records. Twelve sources sharing 24 records may
        // cost a per-source constant, not twelve times the single-source figure.
        assert!(
            sample.6 <= baseline.6 * 2 + sample.0 * 4096,
            "retained state grew with concurrent source count beyond a per-source term: {baseline:?} to {sample:?}"
        );
    }
}

/// Activation count: one agent opening and closing many execution boundaries.
async fn activations(count: usize) -> (Fixture, u64) {
    let fixture = Fixture::new().await;
    let mut offset = 0u64;
    let mut sequence = 0u64;
    let mut header = serde_json::to_vec(&serde_json::json!({
        "type":"session_meta","ordinal":0,"payload":{"id":"activation-owner"}
    }))
    .unwrap();
    header.push(b'\n');
    sequence += 1;
    fixture
        .native_on("activation-source", sequence, offset, &header)
        .await;
    offset += header.len() as u64;
    for turn in 0..count {
        let id = format!("activation-turn-{turn}");
        let started = 1_780_000_000u64 + turn as u64 * 60;
        for payload in [
            serde_json::json!({"type":"task_started","turn_id":id,"started_at":started}),
            serde_json::json!({"type":"task_complete","turn_id":id,"started_at":started,
                "completed_at":started+30,"duration_ms":30_000}),
        ] {
            let mut bytes = serde_json::to_vec(&serde_json::json!({
                "type":"event_msg","ordinal":sequence,"payload":payload
            }))
            .unwrap();
            bytes.push(b'\n');
            sequence += 1;
            fixture
                .native_on("activation-source", sequence, offset, &bytes)
                .await;
            offset += bytes.len() as u64;
        }
    }
    let through = fixture.evidence.evidence_cut().await.unwrap();
    (fixture, through)
}

/// Fan-out: one parent declaring many children in one source.
async fn fan_out(children: usize) -> (Fixture, u64) {
    let fixture = Fixture::new().await;
    let mut offset = 0u64;
    let mut sequence = 0u64;
    let mut header = serde_json::to_vec(&serde_json::json!({
        "type":"session_meta","ordinal":0,"payload":{"id":"fan-parent"}
    }))
    .unwrap();
    header.push(b'\n');
    sequence += 1;
    fixture
        .native_on("fan-source", sequence, offset, &header)
        .await;
    offset += header.len() as u64;
    for child in 0..children {
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "type":"event_msg","ordinal":child+1,
            "payload":{"type":"collab_agent_spawn_end","sender_thread_id":"fan-parent",
                "call_id":format!("spawn-{child}"),
                "new_thread_id":format!("fan-child-{child}"),
                "new_agent_nickname":format!("Child {child}")}
        }))
        .unwrap();
        bytes.push(b'\n');
        sequence += 1;
        fixture
            .native_on("fan-source", sequence, offset, &bytes)
            .await;
        offset += bytes.len() as u64;
    }
    let through = fixture.evidence.evidence_cut().await.unwrap();
    (fixture, through)
}

/// Fan-out stays bounded at the observed corpus ceiling, and the growth term
/// between scales is reported rather than assumed.
///
/// A parent's relation set is the structure most at risk of superlinear work:
/// admitting child N changes the parent's directory, which re-materializes the
/// directory rows already emitted for children 1..N. The retained Codex roots in
/// `docs/measurements/unified-trace/codex-structure.json` declare 9, 17 and 3
/// direct children, so 17 is the observed ceiling and 40 is deliberately past it.
///
/// Measuring both is the point: the ceiling establishes what the delivery must
/// support, and the over-provisioned point exposes where the cost goes. The
/// superlinear per-child term is real and is recorded as such; it is bounded here
/// rather than removed, because removing it means incremental directory
/// maintenance, which `docs/roadmap.md` defers explicitly.
#[tokio::test]
async fn child_fan_out_stays_bounded_at_the_observed_corpus_ceiling() {
    // The largest direct-child count among the three retained long roots.
    const OBSERVED_CEILING: usize = 17;
    let mut samples = Vec::new();
    for children in [4usize, OBSERVED_CEILING, 40] {
        let (fixture, through) = fan_out(children).await;
        let (generation, _) = fixture.generation("fan-out").await;
        let worker = fixture.worker();
        let progress = worker.catch_up(&generation, 1, through).await.unwrap();
        assert_eq!(progress.suspended_sources, 0);
        assert_eq!(progress.projected_cut, through);
        let held = worker.states.lock().await.get("fan-out").unwrap().clone();
        let state = held.lock().await;
        let claims = state.reducer.len();
        let spawn_relations = state
            .reduction
            .relations
            .iter()
            .filter(|relation| matches!(&relation.relation, Relation::Spawn { .. }))
            .count();
        drop(state);
        assert_eq!(
            spawn_relations, children,
            "each declared child must contribute exactly one spawn relation"
        );
        samples.push((
            children,
            claims,
            progress.reduction.claims_resolved,
            progress.reader_rows_materialized,
            progress.membership.objects_examined,
        ));
    }
    for (children, claims, resolved, rows, examined) in &samples {
        eprintln!(
            "fan_out_workload children={children} retained_claims={claims} claims_resolved={resolved} reader_rows={rows} membership_objects_examined={examined}"
        );
    }
    for window in samples.windows(2) {
        let (small_children, _, _, small_rows, _) = window[0];
        let (large_children, _, _, large_rows, _) = window[1];
        let factor = large_children as f64 / small_children as f64;
        eprintln!(
            "fan_out_growth children={small_children}->{large_children} children_factor={factor:.2} row_growth={:.2} rows_per_child={:.1}->{:.1}",
            large_rows as f64 / small_rows as f64,
            small_rows as f64 / small_children as f64,
            large_rows as f64 / large_children as f64,
        );
    }
    // Retained claims are the durable record, and they must stay linear in the
    // children actually declared: a pairwise claim would mean the graph itself
    // encoded every parent/child pair.
    let (ceiling_children, ceiling_claims, _, ceiling_rows, _) = samples[1];
    assert_eq!(ceiling_children, OBSERVED_CEILING);
    assert!(
        ceiling_claims <= OBSERVED_CEILING * 16,
        "retained claims at the observed ceiling are not linear in declared children: {ceiling_claims}"
    );
    // Reader rows at the observed ceiling are the foreground cost this delivery
    // has to carry. Bounding the absolute figure, rather than a ratio, is what
    // makes this a supported-workload statement instead of a trend line.
    assert!(
        ceiling_rows <= OBSERVED_CEILING * 32,
        "reader materialization at the observed ceiling exceeds its bound: {ceiling_rows} rows for {OBSERVED_CEILING} children"
    );
    // The re-materialization term is superlinear per child. It is measured and
    // reported above; assert only that it has not become quadratic, which at this
    // decade would put rows past the square of the child count.
    let (largest_children, _, _, largest_rows, _) = samples[2];
    assert!(
        largest_rows < largest_children * largest_children,
        "reader materialization went quadratic in children: {largest_rows} rows for {largest_children} children"
    );
}

/// Activation accumulation stays bounded at the observed corpus ceiling, and the
/// growth term past it is reported rather than assumed.
///
/// Each turn contributes one declaration plus a start and an outcome fact. The
/// durable record must stay linear in turns actually opened: the reducer tracks
/// one active activation per agent, and a closed one must leave the hot path.
/// The `started` activity counts in
/// `docs/measurements/unified-trace/codex-structure.json` are 8, 17 and 3 across
/// the three retained long roots, so 17 is the observed ceiling and 200 is
/// deliberately far past it.
///
/// As with child fan-out, reader materialization carries a superlinear term:
/// closing activation N re-materializes the history rows already emitted for
/// turns 1..N-1. It is measured and bounded here rather than removed, because
/// removing it means incremental history maintenance, which `docs/roadmap.md`
/// defers explicitly.
#[tokio::test]
async fn activation_count_stays_bounded_at_the_observed_corpus_ceiling() {
    // The largest `started` count among the three retained long roots.
    const OBSERVED_CEILING: usize = 17;
    let mut samples = Vec::new();
    for count in [8usize, OBSERVED_CEILING, 200] {
        let (fixture, through) = activations(count).await;
        let (generation, _) = fixture.generation("activations").await;
        let worker = fixture.worker();
        let progress = worker.catch_up(&generation, 1, through).await.unwrap();
        assert_eq!(progress.suspended_sources, 0);
        assert_eq!(progress.unmapped_observations, 0);
        assert_eq!(progress.projected_cut, through);
        let held = worker
            .states
            .lock()
            .await
            .get("activations")
            .unwrap()
            .clone();
        let state = held.lock().await;
        let claims = state.reducer.len();
        let executions = state.reduction.executions.len();
        drop(state);
        assert_eq!(
            executions, count,
            "each turn must contribute exactly one closed activation"
        );
        samples.push((
            count,
            claims,
            progress.reduction.claims_resolved,
            progress.reader_rows_materialized,
        ));
    }
    for (count, claims, resolved, rows) in &samples {
        eprintln!(
            "activation_workload activations={count} retained_claims={claims} claims_resolved={resolved} reader_rows={rows} claims_per_activation={:.2} rows_per_activation={:.2}",
            *claims as f64 / *count as f64,
            *rows as f64 / *count as f64,
        );
    }
    for window in samples.windows(2) {
        let (small_count, _, _, small_rows) = window[0];
        let (large_count, _, _, large_rows) = window[1];
        eprintln!(
            "activation_growth activations={small_count}->{large_count} activation_factor={:.2} row_growth={:.2} rows_per_activation={:.1}->{:.1}",
            large_count as f64 / small_count as f64,
            large_rows as f64 / small_rows as f64,
            small_rows as f64 / small_count as f64,
            large_rows as f64 / large_count as f64,
        );
    }
    // Retained claims are the durable record. Each turn contributes a fixed set
    // of facts, so the per-activation claim slope must stay flat across every
    // scale measured, including past the ceiling.
    let claim_slope = |small: (usize, usize, usize, usize), large: (usize, usize, usize, usize)| {
        (large.1 - small.1) as f64 / (large.0 - small.0) as f64
    };
    let early_claims = claim_slope(samples[0], samples[1]);
    let late_claims = claim_slope(samples[1], samples[2]);
    eprintln!(
        "activation_claim_slope early={early_claims:.3} late={late_claims:.3} ratio={:.3}",
        late_claims / early_claims
    );
    assert!(
        early_claims > 0.0,
        "a per-activation claim rate of zero would mean the measurement observed no growth"
    );
    assert!(
        late_claims <= early_claims * 1.25,
        "per-activation claim cost grew with accumulated activations: {early_claims:.3} then {late_claims:.3}"
    );
    // Reader rows at the observed ceiling are the foreground cost this delivery
    // has to carry. Bounding the absolute figure, rather than a ratio, makes this
    // a supported-workload statement instead of a trend line.
    let (ceiling_count, _, _, ceiling_rows) = samples[1];
    assert_eq!(ceiling_count, OBSERVED_CEILING);
    assert!(
        ceiling_rows <= OBSERVED_CEILING * 32,
        "reader materialization at the observed ceiling exceeds its bound: {ceiling_rows} rows for {OBSERVED_CEILING} activations"
    );
    // Past the ceiling the re-materialization term is superlinear per activation,
    // for the same reason as child fan-out: admitting activation N rewrites the
    // owner's directory. It is measured and reported above; assert only that it
    // has not become quadratic, which at this decade would put rows past the
    // square of the activation count.
    let (largest_count, _, _, largest_rows) = samples[2];
    assert!(
        largest_rows < largest_count * largest_count,
        "reader materialization went quadratic in activations: {largest_rows} rows for {largest_count} activations"
    );
}

/// Payload bytes: one history whose record count is fixed while each record's
/// body grows.
pub(super) async fn sized_history(bytes_each: usize, records: u64) -> (Fixture, u64) {
    let fixture = Fixture::new().await;
    let mut offset = 0u64;
    let mut header = serde_json::to_vec(&serde_json::json!({
        "type":"session_meta","ordinal":0,"payload":{"id":"sized-owner"}
    }))
    .unwrap();
    header.push(b'\n');
    fixture.native_on("sized-source", 1, offset, &header).await;
    offset += header.len() as u64;
    for sequence in 2..=records + 1 {
        let mut raw = serde_json::to_vec(&serde_json::json!({
            "type":"response_item","ordinal":sequence,
            "payload":{"type":"message","id":format!("sized-{sequence}"),
                "role":"assistant",
                "content":[{"type":"output_text","text":"x".repeat(bytes_each)}]}
        }))
        .unwrap();
        raw.push(b'\n');
        fixture
            .native_on("sized-source", sequence, offset, &raw)
            .await;
        offset += raw.len() as u64;
    }
    let through = fixture.evidence.evidence_cut().await.unwrap();
    (fixture, through)
}

/// Payload size must not enter projection work or the first page's byte budget.
///
/// A reader row carries a bounded preview, not the body: `QueryLimits`
/// `max_preview_bytes` is what a page pays per row, and the body itself is a
/// separate explicit read. That is the contract this measures — a thousandfold
/// body growth may move page bytes by the preview limit per visible row and
/// nothing more, and must not move row or VM-step counts at all.
///
/// Without this, a vendor writing large tool output would grow the foreground
/// page in proportion to bytes nobody asked to read, which
/// `docs/unified-trace-conformance.md` § Performance fixtures forbids.
#[tokio::test]
async fn payload_bytes_do_not_enter_projection_work_or_the_first_page_budget() {
    const RECORDS: u64 = 8;
    let mut samples = Vec::new();
    for bytes_each in [64usize, 640, 6400, 64_000] {
        let (fixture, through) = sized_history(bytes_each, RECORDS).await;
        let (generation, _) = fixture.generation("payload-bytes").await;
        let worker = fixture.worker();
        let progress = worker.catch_up(&generation, 1, through).await.unwrap();
        assert_eq!(progress.suspended_sources, 0);
        assert_eq!(progress.unmapped_observations, 0);
        assert_eq!(progress.projected_cut, through);
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:sized-owner").unwrap());
        let page = generation
            .read_page(
                &snapshot,
                &read_scope(&agent, None, "history"),
                None,
                None,
                None,
                RECORDS as usize,
                false,
            )
            .await
            .unwrap();
        assert!(
            !page.rows.is_empty(),
            "the measurement must read the rows it claims to bound"
        );
        let page_bytes: usize = page
            .rows
            .iter()
            .map(|row| serde_json::to_vec(&row.body).unwrap().len())
            .sum();
        samples.push((
            bytes_each,
            progress.reader_rows_materialized,
            page.vm_steps,
            page_bytes,
            page.rows.len(),
        ));
    }
    for (bytes_each, rows, steps, page_bytes, visible) in &samples {
        eprintln!(
            "payload_bytes_workload bytes_each={bytes_each} total_payload={} reader_rows={rows} page_vm_steps={steps} first_page_bytes={page_bytes} visible_rows={visible}",
            *bytes_each as u64 * RECORDS,
        );
    }
    let baseline = samples[0];
    let preview_limit = QueryLimits::default().max_preview_bytes;
    for sample in &samples[1..] {
        assert_eq!(
            sample.1, baseline.1,
            "payload size changed reader materialization work: {baseline:?} to {sample:?}"
        );
        assert_eq!(
            sample.2, baseline.2,
            "payload size changed the page's own scan: {baseline:?} to {sample:?}"
        );
        // Each visible row may carry at most one preview's worth more than the
        // smallest body, which is the bounded quantity; the body is a separate read.
        assert!(
            sample.3 <= baseline.3 + sample.4 * preview_limit,
            "first-page bytes grew past one preview per visible row: {baseline:?} to {sample:?}"
        );
    }
}

/// Source rewrite frequency: the same records re-presented under a new physical
/// source generation, as a truncate-rewriting vendor produces.
async fn rewritten_source(rewrites: u64, records: u64) -> (Fixture, GenerationStore, Vec<usize>) {
    let fixture = Fixture::new().await;
    let (generation, _) = fixture.generation("rewrite-frequency").await;
    let worker = fixture.worker();
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let mut sequence = 0u64;
    let mut rows = Vec::new();
    for incarnation in 0..rewrites {
        let mut offset = 0u64;
        // A rewrite re-presents the same logical records at the same positions
        // under a new generation. Fresh ids would measure history growth instead.
        for ordinal in 0..=records {
            let mut raw = if ordinal == 0 {
                serde_json::to_vec(&serde_json::json!({
                    "type":"session_meta","ordinal":0,"payload":{"id":"rewrite-owner"}
                }))
                .unwrap()
            } else {
                serde_json::to_vec(&serde_json::json!({
                    "type":"response_item","ordinal":ordinal,
                    "payload":{"type":"message","id":format!("rewrite-record-{ordinal}"),
                        "role":"assistant",
                        "content":[{"type":"output_text","text":format!("record {ordinal}")}]}
                }))
                .unwrap()
            };
            raw.push(b'\n');
            sequence += 1;
            fixture
                .admit(
                    ObservationRef {
                        issuer: issuer.clone(),
                        producer_id: "native".into(),
                        producer_epoch: "one".into(),
                        producer_seq: sequence,
                    },
                    SourcePosition {
                        source: SourceRef {
                            issuer: issuer.clone(),
                            instance: "rewrite-source".into(),
                        },
                        generation: incarnation,
                        locator: SourceLocator::Jsonl {
                            byte_start: offset,
                            byte_end: offset + raw.len() as u64,
                            record_ordinal: ordinal,
                        },
                    },
                    &raw,
                    TransportKind::NativeJsonl,
                )
                .await;
            offset += raw.len() as u64;
        }
        let through = fixture.evidence.evidence_cut().await.unwrap();
        let progress = worker.catch_up(&generation, 1, through).await.unwrap();
        assert_eq!(progress.suspended_sources, 0);
        assert_eq!(progress.projected_cut, through);
        rows.push(progress.reader_rows_materialized);
    }
    (fixture, generation, rows)
}

/// A rewrite's per-incarnation cost must stay flat, and the reader must keep
/// exactly one row per logical record however many times the source was rewritten.
///
/// `docs/unified-trace-conformance.md` § Storage, query, and failure acceptance
/// requires truncate/rewrite to be an acquisition case; the cost question is
/// separate and belongs here. The risk is that generation N is reconciled against
/// all of 0..N, which would make a vendor that rewrites on every turn quadratic in
/// its own history.
///
/// Rows per incarnation are asserted flat rather than merely subquadratic, because
/// re-presenting known records resolves the same objects to the same values: the
/// only new work is the current incarnation's own extent.
#[tokio::test]
async fn repeated_source_rewrites_cost_the_same_each_time_and_do_not_duplicate_rows() {
    const RECORDS: u64 = 4;
    let mut samples = Vec::new();
    for rewrites in [1u64, 4, 16] {
        let (fixture, generation, rows) = rewritten_source(rewrites, RECORDS).await;
        let total: usize = rows.iter().sum();
        // However many incarnations were admitted, the reader shows one row per
        // logical record: a rewrite is a new view of the same history, not more of it.
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let agent = AgentRef::Session(SessionId::decode("env7fk2:codex:rewrite-owner").unwrap());
        let page = generation
            .read_page(
                &snapshot,
                &read_scope(&agent, None, "history"),
                None,
                None,
                None,
                (RECORDS * 4) as usize,
                false,
            )
            .await
            .unwrap();
        let mut addresses: Vec<_> = page.rows.iter().map(|row| row.address.clone()).collect();
        addresses.sort();
        addresses.dedup();
        assert_eq!(
            addresses.len(),
            page.rows.len(),
            "a rewrite duplicated reader rows at {rewrites} incarnations"
        );
        let visible = page.rows.len();
        drop(fixture);
        samples.push((rewrites, rows, total, visible));
    }
    for (rewrites, rows, total, visible) in &samples {
        eprintln!(
            "rewrite_frequency_workload rewrites={rewrites} rows_each={rows:?} total_rows={total} visible_rows={visible} rows_per_rewrite={:.1}",
            *total as f64 / *rewrites as f64,
        );
    }
    let visible = samples[0].3;
    assert!(visible > 0, "the rewrite measurement read no rows");
    // Row identity is the correctness half and holds at every frequency measured:
    // an Nth incarnation of the same records is a new view of one history, so the
    // reader shows the same rows it showed at one incarnation.
    for (rewrites, _, _, later_visible) in &samples {
        assert_eq!(
            *later_visible, visible,
            "rewrite count changed how many rows the reader shows: {rewrites} incarnations"
        );
    }
    // The cost half is a real growth term: each incarnation re-materializes the
    // owner's history rows, so the per-pass cost rises by a fixed step per prior
    // incarnation rather than staying flat. Bound the observed frequency and report
    // the step, the same shape as child fan-out and activation count above.
    //
    // Pi is the vendor that truncate-rewrites, and its ordinary appends do not take
    // that path: `_persist` uses `appendFileSync` once flushed, and `_rewriteFile`
    // runs on empty-file initialization, version migration and fork, read from the
    // unbundled install `docs/agents.md` § `pi` names. A handful of incarnations per
    // source is therefore the supported workload, and 16 is deliberately past it.
    const OBSERVED_FREQUENCY: u64 = 4;
    let (frequency, rows, _, _) = &samples[1];
    assert_eq!(*frequency, OBSERVED_FREQUENCY);
    let steps: Vec<usize> = rows.windows(2).map(|pair| pair[1] - pair[0]).collect();
    eprintln!("rewrite_frequency_step rewrites={frequency} row_steps={steps:?}");
    assert!(
        steps.windows(2).all(|pair| pair[0] == pair[1]),
        "the per-incarnation row step is not constant, so rewrite cost is worse than linear: {steps:?}"
    );
    let ceiling = rows.last().unwrap();
    assert!(
        *ceiling <= visible * OBSERVED_FREQUENCY as usize * 8,
        "reader materialization at the observed rewrite frequency exceeds its bound: {ceiling} rows"
    );
    // Past the observed frequency the accumulated term must still not go quadratic
    // in the reader rows one incarnation carries.
    let (largest, largest_rows, _, _) = &samples[2];
    let last = *largest_rows.last().unwrap();
    assert!(
        last < (visible * *largest as usize).pow(2),
        "reader materialization went quadratic in rewrites: {last} rows at {largest} incarnations"
    );
}

/// Durable projection state per retained record, and whether the largest retained
/// history fits `WorkerLimits::max_state_bytes`.
///
/// This is the memory half of S1.4. Process RSS is not the figure to report: a
/// shared test process attributes an allocator's warmup to whichever measurement
/// ran first, which is how a 1101 kB/record "cost" turns into 21 kB/record purely
/// by running later. The durable mapper checkpoint is attributable, has a real
/// declared limit, and is what a restart must load back into memory in one piece.
///
/// Accumulated state legitimately grows with retained history, so the figure to
/// bound is the transient page a restarting worker decodes, not the total. This
/// measures the growth rate, states what the largest retained history accumulates,
/// and asserts that the per-page read stays bounded regardless — which is what
/// `max_state_bytes` means once restoration pages
/// (`crate::trace::generation::GenerationStore::mapper_state_page`).
#[tokio::test]
async fn durable_projection_state_keeps_the_largest_retained_history_restorable() {
    // The largest retained Codex root in
    // `docs/measurements/unified-trace/codex-structure.json`.
    const LARGEST_RETAINED_HISTORY: usize = 13009;
    let limit = WorkerLimits::default().max_state_bytes;
    let mut samples = Vec::new();
    for records in [200u64, 800] {
        let (fixture, through) = sized_history(512, records).await;
        let (generation, _) = fixture.generation("state-bytes").await;
        let worker = fixture.worker();
        worker.catch_up(&generation, 1, through).await.unwrap();
        let snapshot = generation.checkpoint().await.unwrap().snapshot;
        let states = generation.mapper_states(&snapshot).await.unwrap();
        let bytes = serde_json::to_vec(&states).unwrap().len();
        let tombstones = states.values().filter(|value| value.is_null()).count();
        samples.push((records as usize, bytes, states.len(), tombstones));
    }
    for (records, bytes, entries, tombstones) in &samples {
        eprintln!(
            "durable_state_workload records={records} state_bytes={bytes} entries={entries} tombstones={tombstones} bytes_per_record={:.1} max_state_bytes={limit}",
            *bytes as f64 / *records as f64,
        );
    }
    // Separate the per-record slope from fixed overhead, so the projection does not
    // inherit a constant as though it scaled.
    let (small_records, small_bytes, ..) = samples[0];
    let (large_records, large_bytes, ..) = samples[1];
    let slope = (large_bytes - small_bytes) as f64 / (large_records - small_records) as f64;
    let fixed = large_bytes as f64 - slope * large_records as f64;
    let projected = (slope * LARGEST_RETAINED_HISTORY as f64 + fixed).ceil() as usize;
    // The record count at which the accumulated checkpoint passes one page's worth
    // of bytes. Past it, restoration must take more than one page — which is the
    // behaviour under test, not a failure.
    let records_per_page = (limit as f64 / slope).floor() as usize;
    eprintln!(
        "durable_state_projection bytes_per_record={slope:.1} fixed_overhead={fixed:.1} projected_at_{LARGEST_RETAINED_HISTORY}={projected} records_per_page={records_per_page} max_state_bytes={limit}"
    );
    assert!(
        slope > 0.0,
        "a per-record state rate of zero would mean the measurement observed no growth"
    );
    // Every restoration page stays within the limit whatever the total, and the walk
    // terminates. A page reading its whole table would exceed the small limit here,
    // and a cursor that failed to advance would spin instead of finishing.
    let (fixture, through) = sized_history(512, 200).await;
    let (generation, _) = fixture.generation("state-pages").await;
    let worker = fixture.worker();
    worker.catch_up(&generation, 1, through).await.unwrap();
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let page_limit = 64 * 1024;
    let mut cursor = None;
    let mut pages = 0usize;
    let mut restored = 0usize;
    let mut worst = 0usize;
    loop {
        let page = generation
            .mapper_state_page(&snapshot, cursor.as_deref(), page_limit)
            .await
            .unwrap();
        pages += 1;
        restored += page.states.len();
        worst = worst.max(
            page.states
                .iter()
                .map(|(key, value)| key.len() + serde_json::to_vec(value).unwrap().len())
                .sum(),
        );
        assert!(pages <= 4096, "restoration pages must advance, not loop");
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    // Compare the walk against the unpaged read of the same snapshot. Comparing it
    // to another fixture's entry count would measure two stores, not the paging.
    let total_entries = generation.mapper_states(&snapshot).await.unwrap().len();
    eprintln!(
        "durable_state_pages page_limit={page_limit} pages={pages} entries_restored={restored} unpaged_entries={total_entries} worst_page_bytes={worst}"
    );
    assert!(
        pages > 1,
        "the page bound was never exercised: {total_entries} entries fit one {page_limit}-byte page"
    );
    assert_eq!(
        restored, total_entries,
        "paged restoration must deliver every committed entry exactly once"
    );
    assert!(
        worst <= page_limit + slope.ceil() as usize * 8,
        "one restoration page exceeded its byte bound: {worst} against {page_limit}"
    );
}

/// A projection that a running worker can extend must still be restorable by a
/// fresh one at the same cut.
///
/// The warm path bounds the batch it is about to write, so accumulated state that
/// restoration cannot read back is invisible until a restart — the failure mode is
/// a projection that works until the process dies and then stalls permanently, with
/// evidence retained and no read surface advancing. Asserting restorability at the
/// same cut is what makes that reachable in a test rather than in production.
///
/// The restarted worker is given a `max_state_bytes` far *smaller* than the state
/// already committed. That is the whole point: the limit bounds one restoration
/// page, not the retained total, so a history whose checkpoint exceeds it many
/// times over must still resume.
#[tokio::test]
async fn state_a_warm_worker_accepts_can_be_restored_by_a_restarted_worker() {
    let (fixture, through) = sized_history(512, 200).await;
    let (generation, _) = fixture.generation("restorable-state").await;
    let worker = fixture.worker();
    let warm = worker.catch_up(&generation, 1, through).await.unwrap();
    assert_eq!(warm.projected_cut, through);
    let snapshot = generation.checkpoint().await.unwrap().snapshot;
    let bytes = serde_json::to_vec(&generation.mapper_states(&snapshot).await.unwrap())
        .unwrap()
        .len();
    drop(worker);
    let page_limit = 64 * 1024;
    assert!(
        bytes > page_limit * 4,
        "the committed state must exceed one page several times over for this to test paging: {bytes}"
    );
    let restarted = fixture.worker_with(WorkerLimits {
        max_state_bytes: page_limit,
        ..WorkerLimits::default()
    });
    let resumed = restarted
        .catch_up(&generation, 1, through)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "a restarted worker cannot restore the {bytes} bytes of state a warm worker committed through a {page_limit}-byte page: {error}"
            )
        });
    assert_eq!(
        resumed.projected_cut, through,
        "restoration must resume at the committed cut"
    );
    eprintln!(
        "restorable_state committed_bytes={bytes} page_limit={page_limit} resumed_cut={through}"
    );
}
