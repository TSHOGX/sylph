use super::*;
use sylph_protocol::trace::{IngestionScope, InventoryRef, PayloadRef};

#[derive(Default)]
struct MemoryTransport {
    pending: Mutex<BTreeMap<String, trace::Capture>>,
    retained: Mutex<BTreeMap<ObservationRef, trace::Capture>>,
    chunks: Mutex<BTreeMap<String, Vec<u8>>>,
    drop_receipt: AtomicBool,
}
#[async_trait]
impl CaptureTransport for MemoryTransport {
    async fn upload(&self, operation: CaptureUpload) -> Result<CaptureReply> {
        match operation {
            CaptureUpload::Begin { capture, .. } => {
                if self
                    .retained
                    .lock()
                    .await
                    .contains_key(&capture.observation_ref)
                {
                    return Ok(receipt(&capture));
                }
                let ticket = CaptureUploadTicket {
                    id: capture.observation_ref.encode(),
                    observation: capture.observation_ref.clone(),
                };
                self.pending.lock().await.insert(ticket.id.clone(), capture);
                Ok(CaptureReply::Pending { ticket })
            }
            CaptureUpload::Chunk {
                reference, bytes, ..
            } => {
                anyhow::ensure!(
                    blake3::hash(&bytes).to_hex().as_str() == reference.digest(),
                    "chunk changed"
                );
                self.chunks
                    .lock()
                    .await
                    .insert(reference.as_str().into(), bytes);
                Ok(CaptureReply::ChunkStored)
            }
            CaptureUpload::Commit { ticket } => {
                let capture = self
                    .pending
                    .lock()
                    .await
                    .remove(&ticket.id)
                    .context("unknown upload")?;
                self.retained
                    .lock()
                    .await
                    .insert(capture.observation_ref.clone(), capture.clone());
                if self.drop_receipt.swap(false, Ordering::SeqCst) {
                    return Err(anyhow!("connection closed after durable admission"));
                }
                Ok(receipt(&capture))
            }
        }
    }
    async fn notify(&self, _: WireMessage) -> Result<()> {
        Ok(())
    }
}
fn receipt(capture: &trace::Capture) -> CaptureReply {
    CaptureReply::Admitted {
        receipts: vec![sylph_protocol::CaptureReceipt {
            observation: capture.observation_ref.clone(),
            disposition: CaptureDisposition::Retained {
                admission_seq: capture.observation_ref.producer_seq,
            },
        }],
        accepted_through: capture.observation_ref.producer_seq,
        evidence_cut: capture.observation_ref.producer_seq,
    }
}
fn auth() -> CaptureAuthorization {
    CaptureAuthorization {
        scope: IngestionScope::Inventory(InventoryRef {
            environment: EnvironmentId::parse("env7").unwrap(),
            vendor: AgentName::parse("codex").unwrap(),
            instance: "launch-before-id".into(),
        }),
        epoch: trace::IngestionEpoch::new("epoch-one").unwrap(),
    }
}
async fn runtime(root: &Path, sink: Arc<dyn CaptureTransport>) -> EnvAgent {
    EnvAgent::open(
        EnvAgentConfig {
            environment_id: "env7".into(),
            producer_id: "runtime".into(),
            spool: SpoolConfig::new(root.join("spool.db"), 1024 * 1024),
            path_policy: PathPolicy::from_roots([root.to_path_buf()]).unwrap(),
        },
        sink,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn capture_replay_after_lost_receipt_preserves_identity_bytes_and_empty_prelaunch_subjects() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(MemoryTransport::default());
    let runtime = runtime(dir.path(), transport.clone()).await;
    let bytes = b" {\"id\":1,\"id\":2} \r\n".to_vec();
    let reference = runtime
        .registry
        .capture_frame(
            "process/rpc",
            trace::TransportKind::LiveStdout,
            bytes.clone(),
            local_authorization(&auth(), 123),
        )
        .await
        .unwrap();
    transport.drop_receipt.store(true, Ordering::SeqCst);
    assert!(runtime.flush_pending(1).await.is_err());
    assert!(!runtime.spool.is_empty().await.unwrap());
    assert_eq!(runtime.flush_pending(1).await.unwrap(), 1);
    assert!(runtime.spool.is_empty().await.unwrap());
    let retained = transport.retained.lock().await;
    assert_eq!(retained.len(), 1);
    let capture = &retained[&reference];
    assert!(capture.declared_subject_refs.is_empty());
    assert_eq!(capture.ingestion_authorization_epoch, auth().epoch);
    assert_eq!(
        transport.chunks.lock().await[PayloadRef::bytes(blake3::hash(&bytes).to_hex())
            .unwrap()
            .as_str()],
        bytes
    );
}

#[tokio::test]
async fn process_request_and_raw_reply_are_spooled_before_control_observes_native_identity() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(MemoryTransport::default());
    let runtime = runtime(dir.path(), transport.clone()).await;
    let capture = Arc::new(ProcessCapture {
        registry: runtime.registry.clone(),
        authorization: auth(),
        wake: runtime.wake.clone(),
    });
    let spawn=sylph_adapters::process::Spawn{program:"sh".into(),args:vec!["-c".into(),"IFS= read -r line; printf '{\"id\":1,\"result\":{\"thread\":{\"id\":\"real-native\"}}}\\n'".into()],cwd:dir.path().to_string_lossy().into_owned(),env:HashMap::new(),env_remove:vec![],reply_id:Some(Box::new(sylph_adapters::process::json_rpc_reply_id))};
    let (child, mut lines) = sylph_adapters::process::Child::spawn(spawn, capture)
        .await
        .unwrap();
    let reply = child
        .request_with_receipt(|id| serde_json::json!({"id":id,"method":"thread/start"}))
        .await
        .unwrap();
    assert_eq!(reply["result"]["thread"]["id"], "real-native");
    while lines.recv().await.is_some() {}
    runtime.flush_snapshot().await.unwrap();
    let captures = transport.retained.lock().await;
    assert!(captures.contains_key(&reply.observation));
    let reply_capture = &captures[&reply.observation];
    assert_eq!(
        transport.chunks.lock().await[reply_capture.payload_ref.as_str()],
        reply.raw
    );
    let request = captures
        .values()
        .find(|capture| capture.transport_kind == trace::TransportKind::LiveStdin)
        .unwrap();
    assert_eq!(
        request.source_position.source,
        reply_capture.source_position.source
    );
    assert!(request.observation_ref.producer_seq < reply.observation.producer_seq);
}

#[tokio::test]
async fn stale_connection_replies_cannot_release_a_new_capture_request() {
    let (tx, mut rx) = mpsc::channel(2);
    let transport = Arc::new(ChannelCaptureTransport::new(tx, 7));
    let mut request = {
        let transport = transport.clone();
        tokio::spawn(async move {
            transport
                .upload(CaptureUpload::Commit {
                    ticket: CaptureUploadTicket {
                        id: "upload".into(),
                        observation: ObservationRef {
                            issuer: Issuer::Environment(EnvironmentId::parse("env7").unwrap()),
                            producer_id: "p".into(),
                            producer_epoch: "e".into(),
                            producer_seq: 1,
                        },
                    },
                })
                .await
        })
    };
    let WireMessage::CaptureUpload { request_id, .. } = rx.recv().await.unwrap() else {
        panic!("capture request")
    };
    transport
        .reply(&request_id, CaptureReply::ChunkStored, 6)
        .await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(25), &mut request)
            .await
            .is_err(),
        "the earlier connection epoch must not resolve the current waiter"
    );
    transport
        .reply(&request_id, CaptureReply::ChunkStored, 7)
        .await;
    assert!(matches!(
        request.await.unwrap().unwrap(),
        CaptureReply::ChunkStored
    ));
}

#[tokio::test]
async fn retained_file_operations_stay_inside_the_recorded_workdir() {
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().join("work");
    std::fs::create_dir(&workdir).unwrap();
    std::fs::write(dir.path().join("outside"), "secret").unwrap();
    let runtime = runtime(dir.path(), Arc::new(MemoryTransport::default())).await;
    assert!(matches!(
        runtime
            .file_command_at_root(
                &workdir,
                FileOperation::Read {
                    relative: "../outside".into(),
                    limit: None
                }
            )
            .await,
        CommandOutcome::Rejected { .. }
    ));
    let data = base64::engine::general_purpose::STANDARD.encode(b"retained");
    let result = runtime
        .file_command_at_root(
            &workdir,
            FileOperation::Write {
                relative: "file".into(),
                data,
                overwrite: false,
            },
        )
        .await;
    assert!(matches!(result, CommandOutcome::Accepted { .. }));
    assert_eq!(std::fs::read(workdir.join("file")).unwrap(), b"retained");
}

#[tokio::test]
async fn repeated_mutating_command_returns_its_durable_receipt_without_repeating_the_write() {
    use base64::Engine;
    let dir = tempfile::tempdir().unwrap();
    let runtime = runtime(dir.path(), Arc::new(MemoryTransport::default())).await;
    let command = EnvironmentCommand::Files {
        session_id: SessionId::decode("env7:codex:native").unwrap(),
        workdir: dir.path().to_string_lossy().into(),
        operation: FileOperation::Write {
            relative: "once.txt".into(),
            data: base64::engine::general_purpose::STANDARD.encode(b"immutable bytes"),
            overwrite: false,
        },
    };
    let id = CommandRequestId::parse("effect-once").unwrap();
    let first = runtime
        .execute_command(id.clone(), IdempotencyPolicy::AtMostOnce, command.clone())
        .await;
    assert!(
        matches!(first, CommandOutcome::Accepted { .. }),
        "{first:?}"
    );
    let repeated = runtime
        .execute_command(id.clone(), IdempotencyPolicy::AtMostOnce, command.clone())
        .await;
    assert_eq!(repeated, first);
    assert_eq!(
        std::fs::read(dir.path().join("once.txt")).unwrap(),
        b"immutable bytes"
    );
    let reused = runtime
        .execute_command(
            id,
            IdempotencyPolicy::AtMostOnce,
            EnvironmentCommand::Health,
        )
        .await;
    assert!(matches!(reused,CommandOutcome::Rejected {code,..} if code=="COMMAND_ID_REUSED"));
}

#[tokio::test]
async fn a_crashed_at_most_once_attempt_stays_unknown_after_runtime_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first = runtime(dir.path(), Arc::new(MemoryTransport::default())).await;
    let id = CommandRequestId::parse("ambiguous-mkdir").unwrap();
    let command = EnvironmentCommand::Files {
        session_id: SessionId::decode("env7:codex:native").unwrap(),
        workdir: dir.path().to_string_lossy().into(),
        operation: FileOperation::Mkdir {
            relative: "must-not-retry".into(),
        },
    };
    assert!(first
        .spool
        .begin_command(&id, IdempotencyPolicy::AtMostOnce, &command)
        .await
        .unwrap()
        .is_none());
    let restarted = runtime(dir.path(), Arc::new(MemoryTransport::default())).await;
    let outcome = restarted
        .execute_command(id, IdempotencyPolicy::AtMostOnce, command)
        .await;
    assert!(matches!(outcome, CommandOutcome::Unknown { .. }));
    assert!(!dir.path().join("must-not-retry").exists());
}

#[tokio::test]
async fn sidecar_follow_keeps_parent_capture_immutable_and_retains_later_file_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let transport = Arc::new(MemoryTransport::default());
    let runtime = runtime(dir.path(), transport.clone()).await;
    let sidecar = dir.path().join("tool-output.txt");
    let raw=serde_json::to_vec(&serde_json::json!({"type":"user","tool_use_result":{"persistedOutputPath":sidecar},"message":{"content":[{"type":"tool_result","tool_use_id":"call-1","content":"formatted result"}]}})).unwrap();
    let parent = runtime
        .registry
        .capture_frame(
            "live-parent",
            trace::TransportKind::LiveStdout,
            raw.clone(),
            local_authorization(&auth(), 123),
        )
        .await
        .unwrap();
    runtime.flush_snapshot().await.unwrap();
    let first_events: Vec<_> = {
        let captures = transport.retained.lock().await;
        let chunks = transport.chunks.lock().await;
        captures
            .values()
            .filter(|capture| capture.transport_kind == trace::TransportKind::SourceAttachment)
            .map(|capture| {
                serde_json::from_slice::<SourceAttachmentEvent>(
                    &chunks[capture.payload_ref.as_str()],
                )
                .unwrap()
            })
            .collect()
    };
    assert_eq!(first_events.len(), 1);
    assert_eq!(first_events[0].parent_observation, parent);
    assert_eq!(first_events[0].parent_slot, "/message/content/0");
    assert_eq!(
        first_events[0].attachment.availability,
        trace::AttachmentAvailability::Missing
    );
    std::fs::write(&sidecar, b"exact output\r\n\0").unwrap();
    runtime.flush_snapshot().await.unwrap();
    std::fs::write(&sidecar, b"later output\n").unwrap();
    runtime.flush_snapshot().await.unwrap();
    std::fs::remove_file(&sidecar).unwrap();
    runtime.flush_snapshot().await.unwrap();
    let captures = transport.retained.lock().await;
    let chunks = transport.chunks.lock().await;
    assert!(captures[&parent].attachments.is_empty());
    assert_eq!(chunks[captures[&parent].payload_ref.as_str()], raw);
    let events: Vec<_> = captures
        .values()
        .filter(|capture| capture.transport_kind == trace::TransportKind::SourceAttachment)
        .map(|capture| {
            let event: SourceAttachmentEvent =
                serde_json::from_slice(&chunks[capture.payload_ref.as_str()]).unwrap();
            assert_eq!(capture.attachments, vec![event.attachment.clone()]);
            event
        })
        .collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].attachment.availability, trace::AttachmentAvailability::Missing);
    assert!(events[3].attachment.payload_ref.is_none());
    let retained: Vec<_> = events
        .iter()
        .filter_map(|event| {
            event
                .attachment
                .payload_ref
                .as_ref()
                .map(|payload| chunks[payload.as_str()].clone())
        })
        .collect();
    assert_eq!(
        retained,
        vec![b"exact output\r\n\0".to_vec(), b"later output\n".to_vec()]
    );
    assert_ne!(
        events[1].attachment.source_position,
        events[2].attachment.source_position
    );
}

#[derive(Clone)]
struct NativeOnlySource {
    path: PathBuf,
}
impl AdapterSource for NativeOnlySource {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        (name == "codex").then(|| Box::new(self.clone()) as Box<dyn Adapter>)
    }
    fn known_agents(&self) -> Vec<String> {
        vec!["codex".into()]
    }
}
#[async_trait]
impl Adapter for NativeOnlySource {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(AgentInfo {
            name: "codex".into(),
            version: Some("fixture".into()),
            flags: Default::default(),
            supports: vec![],
            dictates_native_id: false,
            native_fidelity: Fidelity::Blocks,
        })
    }
    async fn run(&self, _: RunSpec, _: Arc<dyn RawCaptureSink>) -> Result<Box<dyn Session>> {
        anyhow::bail!("this fixture only observes a native source")
    }
    async fn enumerate(&self, _: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(vec![])
    }
    async fn native_sources(&self, id: &NativeId) -> Result<Vec<NativeSource>> {
        Ok(if self.path.exists() {
            vec![NativeSource {
                locator: self.path.to_string_lossy().into_owned(),
                namespace: "codex.native".into(),
                format: NativeSourceFormat::Jsonl,
                companion_locator: None,
                owner: Some(NativeOwner::Session {
                    native_id: id.clone(),
                }),
                identity_record_ordinal: Some(0),
            }]
        } else {
            vec![]
        })
    }
}
#[tokio::test]
async fn source_state_preserves_partial_and_missing_native_coverage_and_records_follow_stop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.jsonl");
    let head = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"native\"}}\n";
    std::fs::write(&path, [head.as_slice(), b"partial"].concat()).unwrap();
    let transport = Arc::new(MemoryTransport::default());
    let runtime = EnvAgent::open_with_adapters(
        EnvAgentConfig {
            environment_id: "env7".into(),
            producer_id: "runtime".into(),
            spool: SpoolConfig::new(dir.path().join("spool.db"), 1024 * 1024),
            path_policy: PathPolicy::from_roots([dir.path().to_path_buf()]).unwrap(),
        },
        transport.clone(),
        Arc::new(NativeOnlySource { path: path.clone() }),
    )
    .await
    .unwrap();
    let id = SessionId::decode("env7:codex:native").unwrap();
    runtime
        .start_native(id.clone(), "codex".into(), auth(), true)
        .await
        .unwrap();
    runtime.flush_snapshot().await.unwrap();
    runtime
        .capture_native(&id, "codex", &auth(), true)
        .await
        .unwrap();
    runtime.flush_snapshot().await.unwrap();
    std::fs::remove_file(&path).unwrap();
    runtime
        .capture_native(&id, "codex", &auth(), true)
        .await
        .unwrap();
    runtime.close(&id).await.unwrap();
    runtime.flush_snapshot().await.unwrap();
    let captures = transport.retained.lock().await;
    let chunks = transport.chunks.lock().await;
    let states: Vec<SourceStateEvent> = captures
        .values()
        .filter(|capture| capture.transport_kind == trace::TransportKind::SourceState)
        .map(|capture| serde_json::from_slice(&chunks[capture.payload_ref.as_str()]).unwrap())
        .collect();
    assert_eq!(states.len(), 3);
    assert_eq!(states[0].state, NativeSourceState::Present);
    assert_eq!(states[0].complete_records, 1);
    assert_eq!(states[0].complete_bytes, head.len() as u64);
    assert_eq!(states[0].pending_bytes, 7);
    assert!(states[0].following);
    assert_eq!(states[1].state, NativeSourceState::Missing);
    assert!(states[1].following);
    assert_eq!(states[2].state, NativeSourceState::Missing);
    assert!(!states[2].following);
    assert_eq!(states[0].source, states[1].source);
    assert_eq!(states[0].generation, states[1].generation);
}

#[tokio::test]
async fn browse_contract_and_quick_start_use_the_environment_root_with_contained_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("allowed");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::create_dir_all(root.join("node_modules")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
    std::os::unix::fs::symlink(root.join("project"), root.join("linked")).unwrap();
    let runtime = runtime(&root, Arc::new(MemoryTransport::default())).await;
    let roots = runtime.browse_roots();
    assert_eq!(roots["roots"][0]["name"], "Home");
    assert_eq!(
        roots["roots"][0]["path"],
        root.canonicalize().unwrap().to_str().unwrap()
    );
    let tree = runtime.browse_tree(None).unwrap();
    assert_eq!(tree["parent"], serde_json::Value::Null);
    assert_eq!(tree["truncated"], false);
    let entries = tree["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["name"], "linked");
    assert_eq!(entries[0]["isSymlink"], true);
    assert!(entries[0]["modifiedAtMs"]
        .as_i64()
        .is_some_and(|time| time > 0));
    let quick = runtime.resolve_workdir(QUICK_START_SENTINEL).unwrap();
    assert_eq!(quick, root.canonicalize().unwrap().join("scratch"));
    assert!(quick.is_dir());
}

#[tokio::test]
async fn replaced_native_source_delivers_its_terminal_staged_bytes_after_the_queue_drains() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.jsonl");
    let transport = Arc::new(MemoryTransport::default());
    let runtime = runtime(dir.path(), transport.clone()).await;
    let source = runtime.registry.register_jsonl("native", &path).await.unwrap();
    std::fs::write(&path, b"first\n{broken").unwrap();
    runtime.registry.capture_jsonl(&source, local_authorization(&auth(), 100)).await.unwrap();
    runtime.flush_snapshot().await.unwrap();
    std::fs::write(&path, b"replacement\n").unwrap();
    runtime.registry.capture_jsonl(&source, local_authorization(&auth(), 200)).await.unwrap();
    runtime.flush_snapshot().await.unwrap();
    let retained = transport.retained.lock().await;
    let chunks = transport.chunks.lock().await;
    let terminal = retained.values().find(|capture| chunks.get(capture.payload_ref.as_str()).is_some_and(|bytes| bytes == b"{broken"))
        .expect("the replaced generation's staged fragment must reach retained evidence");
    assert_eq!(terminal.source_position.generation, 0);
    assert_eq!(terminal.source_position.locator, trace::SourceLocator::Jsonl {byte_start:6,byte_end:13,record_ordinal:1});
    assert_eq!(terminal.ingestion_authorization_epoch, auth().epoch);
    assert_eq!(terminal.captured_at.value, 100);
    assert!(terminal.capture_diagnostics.iter().any(|diagnostic|diagnostic.code==trace::CaptureDiagnosticCode::IncompleteRecord));
    assert_eq!(runtime.spool.size_bytes().await.unwrap(), 0);
    assert!(runtime.spool.is_empty().await.unwrap());
}
