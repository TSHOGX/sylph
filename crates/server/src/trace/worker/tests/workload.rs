//! S1.4 workload dimensions for the production projection path.
//!
//! `docs/unified-trace-conformance.md` § Performance fixtures names seven
//! independent dimensions. History length and superseded versions of hot objects
//! are bounded on the read side in `crate::trace::reading_store`, and the
//! cumulative reducer-claim ceiling is answered in `super::reconciliation`. This
//! module measures the three ingestion dimensions that had no measurement at all:
//! concurrent sources, child fan-out and activation count.
//!
//! These are workload measurements, not correctness replays. Source order
//! correctness lives in `super::corpus` and delegation semantics in
//! `super::delegation`; neither varies its dimension to observe cost.

use super::*;

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
        samples.push((
            sources,
            records_each,
            total_records,
            claims,
            progress.reduction.claims_resolved,
            progress.reader_rows_materialized,
        ));
    }
    for (sources, each, total, claims, resolved, rows) in &samples {
        eprintln!(
            "concurrent_source_workload sources={sources} records_each={each} total_records={total} retained_claims={claims} claims_resolved={resolved} reader_rows={rows}"
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
