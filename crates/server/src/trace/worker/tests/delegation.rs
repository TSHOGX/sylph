use super::*;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

mod expected;

const RECORDER: &[u8] =
    include_bytes!("../../../../../../docs/measurements/codex/captures/s10_delegation_fork.jsonl");

fn owner(id: &str) -> AgentRef {
    AgentRef::Session(SessionId::decode(&format!("env7fk2:codex:{id}")).unwrap())
}

#[derive(Deserialize)]
struct Recorded<'a> {
    dir: String,
    #[serde(borrow)]
    frame: &'a RawValue,
}

struct Record {
    raw: Vec<u8>,
    original_line: usize,
    ordinal: usize,
    offset: u64,
    live: bool,
}

fn records() -> (Vec<Record>, Vec<Record>) {
    // These are observed frames, not reconstructed shapes. RawValue removes only
    // the recorder wrapper and preserves each complete vendor frame's lexical bytes.
    assert_eq!(
        format!("{:x}", Sha256::digest(RECORDER)),
        "b2b965778107d3868929c6c89fb5af63db9746e83231ed6de61dabdc03920562"
    );
    let mut live = Vec::new();
    for (line, raw) in RECORDER
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        let value: Recorded<'_> = serde_json::from_slice(raw).unwrap();
        if value.dir == "in" {
            live.push(Record {
                raw: value.frame.get().as_bytes().to_vec(),
                original_line: line + 1,
                ordinal: live.len() + 1,
                offset: 0,
                live: true,
            });
        }
    }
    assert_eq!(live.len(), 91);
    let mut offset = 0u64;
    let peer = CODEX_NATIVE
        .split_inclusive(|byte| *byte == b'\n')
        .enumerate()
        .map(|(index, raw)| {
            let record = Record {
                raw: raw.to_vec(),
                original_line: index + 1,
                ordinal: index + 1,
                offset,
                live: false,
            };
            offset += raw.len() as u64;
            record
        })
        .collect();
    (live, peer)
}

#[derive(Clone, Copy)]
enum Schedule {
    LiveFirst,
    PeerFirst,
    Alternating,
}

fn ordered(schedule: Schedule) -> Vec<Record> {
    let (live, peer) = records();
    match schedule {
        Schedule::LiveFirst => live.into_iter().chain(peer).collect(),
        Schedule::PeerFirst => peer.into_iter().chain(live).collect(),
        Schedule::Alternating => {
            let mut live = live.into_iter();
            let mut peer = peer.into_iter();
            let mut result = Vec::new();
            loop {
                match (live.next(), peer.next()) {
                    (None, None) => break,
                    (a, b) => result.extend(a.into_iter().chain(b)),
                }
            }
            result
        }
    }
}

async fn admit(fixture: &Fixture, record: &Record, peer_epoch: &IngestionEpoch) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let name = if record.live {
        "retained-delegation-live"
    } else {
        "retained-independent-native"
    };
    let source = SourceRef {
        issuer: issuer.clone(),
        instance: name.into(),
    };
    let capture = Capture {
        observation_ref: ObservationRef {
            issuer: issuer.clone(),
            producer_id: name.into(),
            producer_epoch: "retained".into(),
            producer_seq: record.ordinal as u64,
        },
        source_position: SourcePosition {
            source,
            generation: 0,
            locator: if record.live {
                SourceLocator::Frame {
                    ordinal: record.original_line as u64,
                }
            } else {
                SourceLocator::Jsonl {
                    byte_start: record.offset,
                    byte_end: record.offset + record.raw.len() as u64,
                    record_ordinal: record.ordinal as u64 - 1,
                }
            },
        },
        ingestion_authorization_epoch: if record.live {
            fixture.epoch.clone()
        } else {
            peer_epoch.clone()
        },
        declared_subject_refs: vec![],
        captured_at: TimeEvidence {
            domain: "replay.receipt".into(),
            value: 100,
            unit: TimeUnit::Milliseconds,
            precision: None,
            basis: TimeBasis::ServerReceipt,
        },
        source_clock: None,
        transport_kind: if record.live {
            TransportKind::LiveStdout
        } else {
            TransportKind::NativeJsonl
        },
        vendor_build: Some(
            if record.live {
                "0.150.1"
            } else {
                "retained-native-fixture"
            }
            .into(),
        ),
        schema_fingerprint: None,
        payload_ref: fixture.stage(&record.raw).await,
        attachments: vec![],
        capture_diagnostics: vec![],
    };
    fixture
        .evidence
        .admit_batch(&issuer, std::slice::from_ref(&capture))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .evidence
            .capture_payload(&capture.observation_ref, record.raw.len())
            .await
            .unwrap(),
        Some(record.raw.clone())
    );
    capture
}

#[derive(Debug, PartialEq, Eq)]
struct Projection {
    claims: BTreeMap<ClaimRef, Claim>,
    reader: ReaderProjection,
}

async fn projected(fixture: &Fixture, store: &GenerationStore) -> Projection {
    Projection {
        claims: claims(store)
            .await
            .into_iter()
            .map(|claim| (claim.claim_ref.clone(), claim))
            .collect(),
        reader: reader_projection(fixture, store).await,
    }
}

async fn run(schedule: Schedule, incremental: bool, repeat: bool) -> Projection {
    let fixture = Fixture::new().await;
    let peer_epoch = fixture
        .evidence
        .authorize_scope(
            &IngestionScope::Inventory(InventoryRef {
                environment: EnvironmentId::parse("env7fk2").unwrap(),
                vendor: AgentName::parse("codex").unwrap(),
                instance: "independent-native".into(),
            }),
            &[],
        )
        .await
        .unwrap();
    assert_ne!(fixture.epoch, peer_epoch);
    let (mut store, _) = fixture.generation("observed-delegation").await;
    let sizes = [1, 3, 8, 2, 5];
    let mut next_boundary = 1;
    let mut step = 0;
    let mut last_live = 0;
    let mut captures = Vec::new();
    for (index, record) in ordered(schedule).iter().enumerate() {
        let capture = admit(&fixture, record, &peer_epoch).await;
        if record.live {
            last_live = record.original_line;
        }
        if repeat {
            let cut = fixture.evidence.evidence_cut().await.unwrap();
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
            assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), cut);
            if !record.live {
                let mut reread = capture.clone();
                reread.observation_ref.producer_id = "overlapping-native-reader".into();
                fixture
                    .evidence
                    .admit_batch(&reread.observation_ref.issuer, &[reread.clone()])
                    .await
                    .unwrap();
            }
        }
        captures.push(capture);
        let milestone =
            record.live && [20, 22, 30, 31, 38, 72, 87, 95, 97].contains(&record.original_line);
        if incremental && (index + 1 >= next_boundary || milestone) {
            let worker = fixture.worker_with(WorkerLimits {
                max_unit_observations: sizes[step % sizes.len()],
                ..Default::default()
            });
            let progress = worker
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            assert_eq!(progress.pending_mappings, 0);
            assert_eq!(progress.suspended_sources, 0);
            if milestone {
                expected::assert_expected(&fixture, &store, last_live).await;
            }
            let path = store.path().to_path_buf();
            let manifest = store.manifest().clone();
            let limits = store.limits().clone();
            store.close().await;
            store = GenerationStore::open(path, manifest, limits).await.unwrap();
            step += 1;
            next_boundary = index + 1 + sizes[step % sizes.len()];
        }
    }
    let worker = fixture.worker();
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&store, 1, cut).await.unwrap();
    expected::assert_expected(&fixture, &store, 97).await;
    let before = projected(&fixture, &store).await;
    if repeat {
        for mut capture in captures
            .into_iter()
            .filter(|capture| capture.transport_kind == TransportKind::NativeJsonl)
        {
            capture.observation_ref.producer_id = "complete-native-reread".into();
            fixture
                .evidence
                .admit_batch(&capture.observation_ref.issuer, &[capture.clone()])
                .await
                .unwrap();
        }
        worker
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_eq!(projected(&fixture, &store).await, before);
    }
    let (fresh, _) = fixture.generation("delegation-rebuild").await;
    let mut oracle = fixture.worker();
    oracle.full_reader_rebuilds = true;
    oracle.catch_up(&fresh, 1, cut).await.unwrap();
    expected::assert_expected(&fixture, &fresh, 97).await;
    assert_eq!(fresh.checkpoint().await.unwrap().snapshot.evidence_cut, cut);
    assert_eq!(projected(&fixture, &fresh).await, before);
    let result = Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    };
    fresh.close().await;
    store.close().await;
    fixture.evidence.close().await;
    result
}

#[tokio::test]
async fn observed_delegation_preserves_own_lifetimes_exact_bytes_and_reader_state_across_replay_schedules(
) {
    let cold = run(Schedule::LiveFirst, false, false).await;
    assert_eq!(run(Schedule::LiveFirst, true, true).await, cold);
    assert_eq!(run(Schedule::PeerFirst, true, false).await, cold);
    assert_eq!(run(Schedule::Alternating, true, true).await, cold);
}
