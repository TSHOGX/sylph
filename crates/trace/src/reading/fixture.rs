use super::*;
use crate::mapping::{ArchiveContext, MapInput, MapperState, SemanticMapper};
use crate::reducer::Reducer;

pub(super) fn fixture() -> serde_json::Value {
    let raw = include_bytes!("../../../../fixtures/native/codex/user-input-response-item.jsonl");
    let agent = AgentRef::Session(
        sylph_protocol::SessionId::decode("envfixture:codex:response-user").unwrap(),
    );
    let source = SourceRef {
        issuer: Issuer::Environment(agent.environment().clone()),
        instance: "native-user-response-fixture".into(),
    };
    let mut state = MapperState::default();
    let mut reducer = Reducer::default();
    let mut previews = BTreeMap::new();
    let mut payloads = serde_json::Map::new();
    let mut available = BTreeSet::new();
    let mut offset = 0;
    for (index, line) in raw.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let capture = Capture {
            observation_ref: ObservationRef {
                issuer: source.issuer.clone(),
                producer_id: "fixture".into(),
                producer_epoch: "fixed".into(),
                producer_seq: index as u64 + 1,
            },
            source_position: SourcePosition {
                source: source.clone(),
                generation: 0,
                locator: SourceLocator::Jsonl {
                    byte_start: offset,
                    byte_end: offset + line.len() as u64,
                    record_ordinal: index as u64,
                },
            },
            ingestion_authorization_epoch: IngestionEpoch::new("fixture-epoch").unwrap(),
            declared_subject_refs: vec![agent.clone()],
            captured_at: TimeEvidence {
                domain: "fixture-capture".into(),
                value: index as i64,
                unit: TimeUnit::Milliseconds,
                precision: None,
                basis: TimeBasis::ServerReceipt,
            },
            source_clock: None,
            transport_kind: TransportKind::NativeJsonl,
            vendor_build: Some("0.153.4".into()),
            schema_fingerprint: None,
            payload_ref: PayloadRef::bytes(blake3::hash(line).to_hex()).unwrap(),
            attachments: vec![],
            capture_diagnostics: vec![],
        };
        offset += line.len() as u64;
        available.insert(capture.observation_ref.clone());
        let output = crate::mapping::codex::CodexMapper.map(MapInput {
            capture: &capture,
            canonical_observation: &capture.observation_ref,
            raw: line,
            state: &state,
            archive: &ArchiveContext::default(),
        });
        reducer.apply(&output.claims, &available, 100).unwrap();
        state = output.state;
        for payload in output.payloads {
            let text = String::from_utf8(payload.bytes.clone()).unwrap();
            payloads.insert(payload.reference.to_string(), serde_json::json!(text));
            previews.insert(
                payload.reference,
                PayloadPreview {
                    chunks: payload
                        .manifest
                        .as_ref()
                        .map(|manifest| manifest.chunks.clone()),
                    preview_limit: 2048,
                    preview: text,
                    preview_truncated: false,
                    total_bytes: Some(payload.bytes.len() as u64),
                    ranges: vec![PayloadRange {
                        start: 0,
                        end_exclusive: payload.bytes.len() as u64,
                    }],
                    availability: Some(ReadAvailability::Available),
                },
            );
        }
    }
    let projection = project(&reducer.resolve(), &previews, 32);
    let view = ReadView {
        token: "fixture-view".into(),
        generation: "fixture-generation".into(),
        evidence_cut: available.len() as u64,
        view_revision: 1,
        expires_at_ms: 4_000_000_000_000,
    };
    let limits = ReadLimits {
        max_items: 64,
        max_summary_bytes: 262_144,
        max_preview_bytes: 2048,
        max_payload_bytes: 1_048_576,
        max_change_items: 1024,
        max_mounted_rows: 200,
        max_metadata_cache_bytes: 16_777_216,
        max_payload_cache_bytes: 33_554_432,
        max_hidden_locations: 10,
        max_visible_interests: 16,
    };
    let units = projection.windows[&ReadingScope {
        agent: agent.clone(),
        selection: None,
    }]
        .clone();
    let window = ReadPage {
        view: view.clone(),
        returned_items: units.len(),
        returned_bytes: serde_json::to_vec(&units).unwrap().len(),
        items: units,
        before_cursor: None,
        after_cursor: None,
        has_before: false,
        has_after: false,
        total: ReadCount {
            value: Some(2),
            scope: "retained reading units".into(),
            complete: true,
        },
        state: ReadPageState::Complete,
        limits,
    };
    let bootstrap = ReaderBootstrap {
        view: view.clone(),
        header: projection.headers[&agent].clone(),
        window,
        change_cursor: "fixture-generation:1".into(),
        progress: TraceProgress {
            evidence_cut: 5,
            projection_cut: 5,
            search_cut: None,
            pending_outbox: 0,
            oldest_outbox_age_ms: None,
            source_connected: false,
            fault: None,
        },
        control: ReadControlAvailability::Unavailable {
            reason: "Retained fixture has no live control route".into(),
        },
        authorization_scope: "fixture-reader".into(),
    };
    let objects: Vec<_> = projection
        .objects
        .into_values()
        .map(|mut object| {
            object.view = view.clone();
            object
        })
        .collect();
    serde_json::json!({"source":{"path":"fixtures/native/codex/user-input-response-item.jsonl","blake3":blake3::hash(raw).to_hex().as_str(),"method":"production Codex mapper, shared reducer and reading projector; fixed capture/view metadata"},"bootstrap":bootstrap,"objects":objects,"payloads":payloads})
}

#[test]
fn browser_semantic_fixture_is_the_production_projection_of_retained_vendor_bytes() {
    let value = fixture();
    let rows = value["bootstrap"]["window"]["items"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]["content"]["parts"][0]["preview"],
        "Recover the missing transcript"
    );
    assert_eq!(
        rows[1]["content"]["parts"][0]["preview"],
        "The transcript is available."
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/unified-trace/reading/native-codex.json");
    if std::env::var_os("SYLPH_WRITE_READER_FIXTURE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap() + "\n").unwrap();
    }
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        blake3::hash(&serde_json::to_vec(&value).unwrap()),
        blake3::hash(&serde_json::to_vec(&expected).unwrap()),
        "production reader fixture differs; inspect the generated JSON before updating it"
    );
}
