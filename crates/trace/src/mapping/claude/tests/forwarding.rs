use super::*;

fn route() -> String {
    correlation_key(&owner("root"), "claude.parent-tool-use", &["host".into()])
}
fn child() -> AgentRef {
    Context::child(&owner("root"), "native-child")
}
fn certificate(index: u64) -> ObservationRef {
    let mut value = capture(index, false).observation_ref;
    value.producer_id = format!("proof-{index}");
    value
}
fn record(index: u64) -> (Capture, Vec<u8>) {
    (capture(index,true),serde_json::to_vec(&serde_json::json!({"type":"user","session_id":"root","parent_tool_use_id":"host","uuid":format!("input-{index}"),"message":{"role":"user","content":"same forwarded bytes"}})).unwrap())
}
fn introduce(archive: &mut ArchiveContext, index: u64) -> MapOutput {
    let mut output = MapOutput::default();
    output
        .correlations
        .insert(route(), AnyObjectRef::Agent(child()));
    output
        .correlation_evidence
        .insert(route(), vec![certificate(index)]);
    output.corroborating_correlations.insert(route());
    archive.observations.insert(certificate(index));
    output
}
fn drain(
    archive: &mut ArchiveContext,
    capture: &Capture,
    raw: &[u8],
    mut output: MapOutput,
) -> usize {
    let mut pages = 0;
    loop {
        pages += 1;
        assert!(pages < 1000);
        assert!(output.claims.len() <= crate::mapping::claude::forwarding::PAGE_WORK);
        let next = output.continuation_state.clone();
        let more = output.requests.iter().any(ArchiveRequest::is_continuation);
        retain_output(archive, capture, &output);
        if !more {
            break;
        }
        output = ClaudeMapper.map(MapInput {
            capture,
            canonical_observation: &capture.observation_ref,
            raw,
            state: &next.unwrap(),
            archive,
        });
    }
    pages
}
fn bind(archive: &mut ArchiveContext, index: u64) -> usize {
    let (capture, raw) = record(index + 1000);
    let mut output = introduce(archive, index);
    crate::mapping::claude::forwarding::refine(
        &MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &MapperState::default(),
            archive,
        },
        &mut output,
    );
    crate::mapping::finish_refinements(
        &MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &MapperState::default(),
            archive,
        },
        &mut output,
    );
    drain(archive, &capture, &raw, output)
}
fn admit(archive: &mut ArchiveContext, index: u64) {
    let (capture, raw) = record(index);
    let output = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &raw,
        state: &MapperState::default(),
        archive,
    });
    drain(archive, &capture, &raw, output);
}

#[test]
fn late_independent_forwarding_certificates_produce_the_same_claims_without_borrowing_each_others_retention(
) {
    let mut expected = None;
    for order in [[0, 1, 2], [1, 0, 2], [2, 0, 1]] {
        let mut archive = ArchiveContext::default();
        for step in order {
            if step == 0 {
                admit(&mut archive, 0);
            } else {
                bind(&mut archive, step);
            }
        }
        // No certificate at the first read leaves a resolvable capture request;
        // the real worker retries that raw occurrence when its key becomes known.
        if order[0] == 0 {
            admit(&mut archive, 0);
        }
        let claims: Vec<_> = archive.claims.values().cloned().collect();
        assert!(!claims.is_empty());
        for claim in &claims {
            let count = [1, 2]
                .into_iter()
                .filter(|index| claim.evidence.contains(&certificate(*index)))
                .count();
            assert_eq!(
                count, 1,
                "independent routing proofs became one conjunction"
            );
            assert!(claim.evidence.contains(&capture(0, true).observation_ref));
        }
        let messages: Vec<_> = claims
            .iter()
            .filter_map(|claim| match &claim.fact {
                Fact::Declare {
                    object: TraceObject::Message(message),
                } => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.author == Author::Unknown
                && message.message_ref.scope() == &child()));
        if let Some(expected) = &expected {
            assert_eq!(&claims, expected);
        } else {
            expected = Some(claims);
        }
    }
}
#[test]
fn late_forwarding_certificates_resume_bounded_pages_after_serializing_the_checkpoint() {
    let mut archive = ArchiveContext::default();
    bind(&mut archive, 1);
    for index in 0..40 {
        admit(&mut archive, index);
    }
    let before = archive.claims.len();
    let pages = bind(&mut archive, 2);
    assert!(pages > 1);
    assert_eq!(archive.claims.len(), before * 2);
    let (capture, raw) = record(999);
    let mut output = introduce(&mut archive, 3);
    crate::mapping::claude::forwarding::refine(
        &MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: &raw,
            state: &MapperState::default(),
            archive: &archive,
        },
        &mut output,
    );
    assert_eq!(
        output.claims.len(),
        crate::mapping::claude::forwarding::PAGE_WORK
    );
    let state: MapperState = serde_json::from_slice(
        &serde_json::to_vec(output.continuation_state.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    retain_output(&mut archive, &capture, &output);
    let resumed = ClaudeMapper.map(MapInput {
        capture: &capture,
        canonical_observation: &capture.observation_ref,
        raw: &raw,
        state: &state,
        archive: &archive,
    });
    drain(&mut archive, &capture, &raw, resumed);
    assert_eq!(archive.claims.len(), before * 3);
}

#[test]
fn correlations_from_forwarded_frames_keep_each_transitive_routing_certificate() {
    let mut archive = ArchiveContext::default();
    bind(&mut archive, 1);
    let observed = capture(0, true);
    let raw=br#"{"type":"assistant","session_id":"root","parent_tool_use_id":"host","uuid":"answer","message":{"id":"model-response","role":"assistant","content":[{"type":"text","text":"forwarded answer"}]}}"#;
    let output = ClaudeMapper.map(MapInput {
        capture: &observed,
        canonical_observation: &observed.observation_ref,
        raw,
        state: &MapperState::default(),
        archive: &archive,
    });
    drain(&mut archive, &observed, raw, output);
    bind(&mut archive, 2);
    let key = correlation_key(
        &child(),
        "claude.model-record",
        &["model-response".into(), "answer".into()],
    );
    let proofs = &archive.correlation_proofs[&key];
    assert_eq!(proofs.len(), 2);
    for proof in proofs {
        assert!(proof.evidence.contains(&observed.observation_ref));
        assert_eq!(
            [1, 2]
                .into_iter()
                .filter(|index| proof.evidence.contains(&certificate(*index)))
                .count(),
            1
        );
        assert!(
            matches!(&proof.reference,AnyObjectRef::Message(message) if message.scope()==&child())
        );
    }
}
