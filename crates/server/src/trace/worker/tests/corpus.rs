use super::*;
use crate::trace::reading_store::read_scope;

struct Corpus {
    vendor: &'static str,
    session: &'static str,
    native: &'static [u8],
    live: &'static [u8],
    namespace: &'static str,
    question_id: &'static str,
    answer_id: &'static str,
    question: &'static [u8],
    answer: &'static [u8],
}

const CODEX: Corpus = Corpus {
    vendor: "codex",
    session: "response-user",
    native: CODEX_NATIVE,
    live: b"",
    namespace: "codex.message",
    question_id: "ask-1",
    answer_id: "answer-1",
    question: b"Recover the missing transcript",
    answer: b"The transcript is available.",
};
const CLAUDE: Corpus = Corpus {
    vendor: "claude",
    session: "2ea06d27-a87f-458a-9724-9bafa7d64ba6",
    native: include_bytes!("../../../../../../fixtures/unified-trace/claude/native-basic.jsonl"),
    live: include_bytes!("../../../../../../fixtures/unified-trace/claude/live-basic.jsonl"),
    namespace: "claude.record",
    question_id: "10eaa5bd-758b-447a-b867-8a1dd070f69a",
    answer_id: "1150bf8e-6198-4ecb-91f4-32a22bca04cd",
    question: b"Reply with exactly SYLPH_CLAUDE_TRACE_OK.",
    answer: b"SYLPH_CLAUDE_TRACE_OK",
};
const PI: Corpus = Corpus {
    vendor: "pi",
    session: "01a0aac9-147d-70a3-9396-67f57c6d424d",
    native: include_bytes!("../../../../../../fixtures/unified-trace/pi/native-success.jsonl"),
    live: include_bytes!("../../../../../../fixtures/unified-trace/pi/stdout-success.jsonl"),
    namespace: "pi.entry.message",
    question_id: "77f57e95",
    answer_id: "11f59730",
    question: b"Reply with exactly SYLPH_PI_AA_OK.",
    answer: b"SYLPH_PI_AA_OK",
};

impl Corpus {
    fn owner(&self) -> AgentRef {
        AgentRef::Session(
            SessionId::decode(&format!("env7fk2:{}:{}", self.vendor, self.session)).unwrap(),
        )
    }
    fn message(&self, id: &str) -> MessageRef {
        MessageRef::native(self.owner(), self.namespace, vec![id.into()]).unwrap()
    }
}

#[derive(Clone, Copy)]
enum SourceOrder {
    NativeFirst,
    LiveFirst,
    Alternating,
}

struct Record {
    raw: &'static [u8],
    source: &'static str,
    ordinal: u64,
    offset: u64,
}

fn records(corpus: &Corpus, order: SourceOrder) -> Vec<Record> {
    source_records(corpus.native, corpus.live, order)
}

fn source_records(native_bytes: &'static [u8], live_bytes: &'static [u8], order: SourceOrder) -> Vec<Record> {
    let source = |raw: &'static [u8], name| {
        let mut offset = 0;
        raw.split_inclusive(|byte| *byte == b'\n')
            .enumerate()
            .map(|(ordinal, raw)| {
                let record = Record {
                    raw,
                    source: name,
                    ordinal: ordinal as u64,
                    offset,
                };
                offset += raw.len() as u64;
                record
            })
            .collect::<Vec<_>>()
    };
    let native = source(native_bytes, "corpus-native");
    let live = source(live_bytes, "corpus-live");
    match order {
        SourceOrder::NativeFirst => native.into_iter().chain(live).collect(),
        SourceOrder::LiveFirst => live.into_iter().chain(native).collect(),
        SourceOrder::Alternating => {
            let mut native = native.into_iter();
            let mut live = live.into_iter();
            let mut result = Vec::new();
            loop {
                match (native.next(), live.next()) {
                    (None, None) => break,
                    (left, right) => result.extend(left.into_iter().chain(right)),
                }
            }
            result
        }
    }
}

async fn admit_record(fixture: &Fixture, record: &Record) -> Capture {
    let issuer = Issuer::Environment(EnvironmentId::parse("env7fk2").unwrap());
    let native = record.source == "corpus-native";
    fixture
        .admit(
            ObservationRef {
                issuer: issuer.clone(),
                producer_id: record.source.into(),
                producer_epoch: "retained-capture".into(),
                producer_seq: record.ordinal + 1,
            },
            SourcePosition {
                source: SourceRef {
                    issuer,
                    instance: record.source.into(),
                },
                generation: 0,
                locator: if native {
                    SourceLocator::Jsonl {
                        byte_start: record.offset,
                        byte_end: record.offset + record.raw.len() as u64,
                        record_ordinal: record.ordinal,
                    }
                } else {
                    SourceLocator::Frame {
                        ordinal: record.ordinal,
                    }
                },
            },
            record.raw,
            if native {
                TransportKind::NativeJsonl
            } else {
                TransportKind::LiveStdout
            },
        )
        .await
}

#[derive(Debug, PartialEq, Eq)]
struct Projection {
    claims: BTreeMap<ClaimRef, Claim>,
    reader: ReaderProjection,
}

async fn projection(fixture: &Fixture, store: &GenerationStore) -> Projection {
    Projection {
        claims: claims(store)
            .await
            .into_iter()
            .map(|claim| (claim.claim_ref.clone(), claim))
            .collect(),
        reader: reader_projection(fixture, store).await,
    }
}

fn reduction(claims: &[Claim]) -> Reduction {
    let evidence = claims
        .iter()
        .flat_map(|claim| claim.evidence.iter().cloned())
        .collect();
    let mut reducer = Reducer::default();
    reducer.apply(claims, &evidence, 4096).unwrap();
    reducer.resolve()
}

async fn assert_expected(corpus: &Corpus, fixture: &Fixture, store: &GenerationStore) {
    let retained = claims(store).await;
    let reduced = reduction(&retained);
    let owner = corpus.owner();
    let answer = corpus.message(corpus.answer_id);
    let question = corpus.message(corpus.question_id);
    let resolved = reduced
        .object(&AnyObjectRef::Message(answer.clone()))
        .unwrap();
    let Some(TraceObject::Message(message)) = resolved.declaration.known() else {
        panic!("The retained assistant identity must resolve without a declaration conflict")
    };
    assert_eq!(message.message_ref, answer);
    assert_eq!(
        message.author,
        Author::Agent {
            agent: OwnerRef::Agent {
                agent: owner.clone()
            }
        }
    );
    assert_eq!(
        message.production.as_ref().unwrap().owner,
        OwnerRef::Agent {
            agent: owner.clone()
        }
    );
    let answer_revisions = revision_bytes(fixture, store, &answer).await;
    let question_revisions = revision_bytes(fixture, store, &question).await;
    assert_eq!(
        answer_revisions.len(),
        if corpus.vendor == "codex" { 1 } else { 2 }
    );
    assert!(answer_revisions.iter().all(|body| body == corpus.answer));
    assert!(!question_revisions.is_empty());
    assert!(question_revisions
        .iter()
        .all(|body| body == corpus.question));
    if corpus.vendor == "claude" {
        let Some(TraceObject::Message(message)) = reduced
            .object(&AnyObjectRef::Message(question.clone()))
            .unwrap()
            .declaration
            .known()
        else {
            panic!("The SDK-origin native input must remain present")
        };
        assert_eq!(message.author, Author::Unknown);
    }

    let activations: Vec<_> = reduced
        .executions
        .iter()
        .filter(|(reference, _)| matches!(reference, AnyObjectRef::Activation(_)))
        .collect();
    assert_eq!(activations.len(), 1);
    let (execution, state) = activations[0];
    let AnyObjectRef::Activation(activation) = execution else {
        panic!("The marker run must close its own activation")
    };
    assert_eq!(activation.address().scope(), &owner);
    match (corpus.vendor, activation.address()) {
        ("codex", ObjectAddress::Native { namespace, key, .. }) => {
            assert_eq!(namespace, "codex.turn");
            assert_eq!(key, &["turn-user"]);
        }
        (
            "claude" | "pi",
            ObjectAddress::Occurrence {
                position,
                local_slot,
                ..
            },
        ) => {
            assert_eq!(position.source.instance, "corpus-live");
            let (ordinal, slot) = if corpus.vendor == "claude" {
                (2, "sdk-loop")
            } else {
                (3, "outer-run")
            };
            assert_eq!(position.locator, SourceLocator::Frame { ordinal });
            assert_eq!(local_slot, &[slot]);
        }
        _ => panic!("The activation must preserve its vendor boundary's identity"),
    }
    assert_eq!(state.outcome.known(), Some(&TerminalOutcome::Succeeded));
    assert!(!state.started.is_empty());
    assert!(!state.terminated.is_empty());
    assert!(reduced.resource_states.is_empty());
    assert!(!retained
        .iter()
        .any(|claim| matches!(claim.fact, Fact::Equivalence { .. })));
    let (terminal_producer, terminal_seq) = match corpus.vendor {
        "claude" => ("corpus-live", 13),
        "pi" => ("corpus-live", 18),
        _ => ("corpus-native", 5),
    };
    assert!(retained.iter().any(|claim| {
        matches!(&claim.fact, Fact::Execution {
            execution: ExecutionRef::Activation(actual),
            boundary: ExecutionBoundary::Outcome { outcome: TerminalOutcome::Succeeded, .. },
        } if actual == activation)
            && claim.evidence.iter().any(|observation| {
                observation.producer_id == terminal_producer
                    && observation.producer_seq == terminal_seq
            })
    }));

    let snapshot = store.checkpoint().await.unwrap().snapshot;
    let page = store
        .read_page(
            &snapshot,
            &read_scope(&owner, None, "history"),
            None,
            None,
            None,
            64,
            false,
        )
        .await
        .unwrap();
    assert!(!page.has_after && !page.work_exhausted);
    let units = page
        .rows
        .into_iter()
        .map(|row| serde_json::from_value::<ReadingUnit>(row.body).unwrap())
        .collect::<Vec<_>>();
    for (reference, expected) in [(&question, corpus.question), (&answer, corpus.answer)] {
        let rows = units
            .iter()
            .filter_map(|unit| match &unit.content {
                ReadingContent::Message {
                    message_ref, parts, ..
                } if message_ref == reference => Some(parts),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            1,
            "The native message {reference:?} must have one readable row"
        );
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].preview.as_bytes(), expected);
        assert_eq!(
            fixture
                .evidence
                .payload_bytes(&rows[0][0].payload.payload_ref, 1024)
                .await
                .unwrap(),
            expected
        );
    }
    if corpus.vendor == "pi" {
        // get_entries admits native IDs but supplies no join to ID-less RPC messages.
        let live_answers = units
            .iter()
            .filter_map(|unit| match &unit.content {
                ReadingContent::Message {
                    message_ref,
                    author: ReadAuthor::Agent { .. },
                    parts,
                    ..
                } if message_ref != &answer => Some((message_ref, parts)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(live_answers.len(), 1);
        assert!(matches!(
            live_answers[0].0.address(),
            ObjectAddress::Occurrence { .. }
        ));
        assert_eq!(live_answers[0].1[0].preview.as_bytes(), corpus.answer);
        let selected = reduced.histories[&owner].selected_entries.as_ref().unwrap();
        let selected_ids = selected
            .iter()
            .map(|entry| match entry.address() {
                ObjectAddress::Native { namespace, key, .. } => {
                    assert_eq!(namespace, "pi.entry");
                    key[0].as_str()
                }
                _ => panic!("Native selection must not silently admit provisional live entries"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selected_ids,
            ["14d8566d", "e499d53c", "77f57e95", "11f59730"]
        );
    }
}

async fn reopened(store: GenerationStore) -> GenerationStore {
    let path = store.path().to_path_buf();
    let manifest = store.manifest().clone();
    let limits = store.limits().clone();
    store.close().await;
    GenerationStore::open(path, manifest, limits).await.unwrap()
}

async fn run(corpus: &Corpus, order: SourceOrder, incremental: bool, repeated: bool) -> Projection {
    let fixture = Fixture::for_vendor(corpus.vendor).await;
    let (mut store, _) = fixture.generation("corpus").await;
    let mut captures = Vec::new();
    let sizes = [1, 4, 2, 7, 3];
    let mut boundary = sizes[0];
    let mut step = 0;
    for (index, record) in records(corpus, order).iter().enumerate() {
        let capture = admit_record(&fixture, record).await;
        if repeated {
            let before = fixture.evidence.evidence_cut().await.unwrap();
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
            assert_eq!(fixture.evidence.evidence_cut().await.unwrap(), before);
            if capture.transport_kind == TransportKind::NativeJsonl {
                let mut overlap = capture.clone();
                overlap.observation_ref.producer_id = "overlapping-native-follower".into();
                fixture
                    .evidence
                    .admit_batch(&overlap.observation_ref.issuer, &[overlap.clone()])
                    .await
                    .unwrap();
            }
        }
        captures.push(capture);
        if incremental && index + 1 == boundary {
            let worker = fixture.worker_with(WorkerLimits {
                max_unit_observations: sizes[step % sizes.len()],
                ..WorkerLimits::default()
            });
            worker
                .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
                .await
                .unwrap();
            drop(worker);
            store = reopened(store).await;
            step += 1;
            boundary += sizes[step % sizes.len()];
        }
    }
    let worker = fixture.worker();
    let cut = fixture.evidence.evidence_cut().await.unwrap();
    worker.catch_up(&store, 1, cut).await.unwrap();
    assert_expected(corpus, &fixture, &store).await;
    let before = projection(&fixture, &store).await;
    if repeated {
        for mut capture in captures {
            if capture.transport_kind != TransportKind::NativeJsonl {
                continue;
            }
            capture.observation_ref.producer_id = "native-reread".into();
            fixture
                .evidence
                .admit_batch(
                    &capture.observation_ref.issuer,
                    std::slice::from_ref(&capture),
                )
                .await
                .unwrap();
        }
        worker
            .catch_up(&store, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        assert_expected(corpus, &fixture, &store).await;
        assert_eq!(projection(&fixture, &store).await, before);
    }
    let (fresh, _) = fixture.generation("same-cut-rebuild").await;
    fixture.worker().catch_up(&fresh, 1, cut).await.unwrap();
    assert_expected(corpus, &fixture, &fresh).await;
    assert_eq!(fresh.checkpoint().await.unwrap().snapshot.evidence_cut, cut);
    assert_eq!(projection(&fixture, &fresh).await, before);
    Projection {
        claims: before.claims,
        reader: before
            .reader
            .normalize_separate_archive_epochs(&fixture.catalogue)
            .await,
    }
}

async fn replay(corpus: &Corpus) {
    // These schedules exercise retained records, not delayed sidecars or source-gap repair.
    // The capture manifest distinguishes observed Pi process output from its synthetic
    // loopback-provider content; this test creates no source-derived runtime envelopes.
    if !corpus.live.is_empty() {
        let fixture = Fixture::for_vendor(corpus.vendor).await;
        for record in records(corpus, SourceOrder::NativeFirst)
            .iter()
            .filter(|record| record.source == "corpus-native")
        {
            admit_record(&fixture, record).await;
        }
        let (native, _) = fixture.generation("native-subset").await;
        fixture
            .worker()
            .catch_up(&native, 1, fixture.evidence.evidence_cut().await.unwrap())
            .await
            .unwrap();
        let reduced = reduction(&claims(&native).await);
        assert!(
            reduced.executions.is_empty(),
            "Native marker content is not evidence of the live outer run"
        );
        assert_eq!(
            revision_bytes(&fixture, &native, &corpus.message(corpus.answer_id)).await,
            [corpus.answer.to_vec()]
        );
    }
    let cold = run(corpus, SourceOrder::NativeFirst, false, false).await;
    assert_eq!(
        run(corpus, SourceOrder::NativeFirst, true, true).await,
        cold
    );
    if !corpus.live.is_empty() {
        assert_eq!(run(corpus, SourceOrder::LiveFirst, true, false).await, cold);
        assert_eq!(
            run(corpus, SourceOrder::Alternating, true, true).await,
            cold
        );
    }
}

#[tokio::test]
async fn codex_retained_corpus_survives_restart_redelivery_reread_and_same_cut_rebuild() {
    replay(&CODEX).await;
}

#[tokio::test]
async fn claude_retained_pair_preserves_record_identity_across_source_orders_and_restarts() {
    replay(&CLAUDE).await;
}

#[tokio::test]
async fn pi_retained_pair_preserves_unjoined_live_messages_across_source_orders_and_restarts() {
    replay(&PI).await;
}

fn assert_projection(actual: &Projection, expected: &Projection) {
    fn maps<K: Ord + std::fmt::Debug, V: PartialEq + std::fmt::Debug>(
        name: &str,
        actual: &BTreeMap<K, V>,
        expected: &BTreeMap<K, V>,
    ) {
        for key in actual.keys().chain(expected.keys()) {
            if actual.get(key) != expected.get(key) {
                panic!("{name} differs: actual_count={} expected_count={}\nkey={:.250}\nactual={:.2200}\nexpected={:.2200}",actual.len(),expected.len(),format!("{key:?}"),format!("{:?}",actual.get(key)),format!("{:?}",expected.get(key)));
            }
        }
    }
    maps("claims", &actual.claims, &expected.claims);
    maps("reader rows", &actual.reader.rows, &expected.reader.rows);
    maps(
        "memberships",
        &actual.reader.memberships,
        &expected.reader.memberships,
    );
    maps("anchors", &actual.reader.anchors, &expected.reader.anchors);
    maps(
        "read counts",
        &actual.reader.read_counts,
        &expected.reader.read_counts,
    );
    maps(
        "membership counts",
        &actual.reader.membership_counts,
        &expected.reader.membership_counts,
    );
    maps(
        "payloads",
        &actual.reader.payloads,
        &expected.reader.payloads,
    );
}

mod claude_workflow;
mod pi_maintenance;
mod codex_goal;
mod claude_tools;
mod pi_tools;
mod claude_schedule;

mod accounting;

mod codex_reuse;

mod claude_delegation;

mod codex_input;

mod codex_context;

mod fork_lineage;
