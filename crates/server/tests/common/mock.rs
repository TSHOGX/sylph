//! A deterministic vendor capture source hosted inside the real envagent runtime.
use anyhow::Result;
use async_trait::async_trait;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use sylph_protocol::trace::TransportKind;
use sylph_protocol::*;
use tokio::sync::mpsc;

#[derive(Clone, Default)]
pub struct FixtureAgents {
    pub inputs: Arc<Mutex<Vec<TurnInput>>>,
    pub sends: Arc<AtomicU64>,
    pub refuse_send: bool,
    pub unknown_send: bool,
    pub emit_request: bool,
    pub resolutions: Arc<AtomicU64>,
    pub unknown_resolution: bool,
    /// Vendor interrupts actually delivered. Counted so a test can prove a refused
    /// control reached no vendor process, which a status code alone does not show.
    pub interrupts: Arc<AtomicU64>,
}
impl sylph_envagent::adapters::AdapterSource for FixtureAgents {
    fn for_agent(&self, name: &str) -> Option<Box<dyn Adapter>> {
        (name == "codex").then(|| Box::new(self.clone()) as Box<dyn Adapter>)
    }
    fn known_agents(&self) -> Vec<String> {
        vec!["codex".into()]
    }
}
#[async_trait]
impl Adapter for FixtureAgents {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(info())
    }
    async fn run(
        &self,
        spec: RunSpec,
        capture: Arc<dyn RawCaptureSink>,
    ) -> Result<Box<dyn Session>> {
        let native = match spec.native_id {
            RequestedNativeId::New => {
                NativeId::new(format!("fixture-native-{}", uuid::Uuid::new_v4()))
            }
            RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => id,
        };
        let stream = format!("fixture-{}", uuid::Uuid::new_v4());
        let proof=capture.capture(RawCapture{stream:stream.clone(),transport:TransportKind::NativeJsonl,bytes:serde_json::to_vec(&serde_json::json!({"type":"session_meta","payload":{"id":native.as_str(),"cwd":spec.workdir}}))?,captured_at_ms:1}).await?;
        let (tx, rx) = mpsc::channel(32);
        Ok(Box::new(FixtureSession {
            native,
            proof,
            capture,
            stream,
            source: self.clone(),
            sender: tx,
            events: Some(rx),
            last: Mutex::new(None),
        }))
    }
    async fn native_sources(&self, _: &NativeId) -> Result<Vec<NativeSource>> {
        Ok(vec![])
    }
    async fn enumerate(&self, _: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(vec![])
    }
}
pub fn info() -> AgentInfo {
    AgentInfo {
        name: "codex".into(),
        version: Some("fixture".into()),
        flags: Default::default(),
        supports: vec![
            Capability::Interrupt,
            Capability::Steer,
            Capability::Queue,
            Capability::SetConfig,
        ],
        dictates_native_id: false,
        native_fidelity: Fidelity::Messages,
    }
}
struct FixtureSession {
    native: NativeId,
    proof: sylph_protocol::trace::ObservationRef,
    capture: Arc<dyn RawCaptureSink>,
    stream: String,
    source: FixtureAgents,
    sender: mpsc::Sender<Result<SessionMessage>>,
    events: Option<mpsc::Receiver<Result<SessionMessage>>>,
    last: Mutex<Option<String>>,
}
impl FixtureSession {
    async fn raw(&self, value: serde_json::Value) -> Result<()> {
        self.capture
            .capture(RawCapture {
                stream: self.stream.clone(),
                transport: TransportKind::NativeJsonl,
                bytes: serde_json::to_vec(&value)?,
                captured_at_ms: 2,
            })
            .await?;
        Ok(())
    }
}
#[async_trait]
impl Session for FixtureSession {
    fn native_id(&self) -> Option<NativeId> {
        Some(self.native.clone())
    }
    async fn identity_evidence(&self) -> Vec<sylph_protocol::trace::ObservationRef> {
        vec![self.proof.clone()]
    }
    async fn capabilities(&self) -> Result<SessionCapabilities> {
        Ok(SessionCapabilities::default())
    }
    async fn send(&self, input: TurnInput) -> Result<()> {
        self.source.sends.fetch_add(1, Ordering::SeqCst);
        self.source.inputs.lock().unwrap().push(input.clone());
        if self.source.refuse_send {
            return Err(VendorRefusal {
                message: "fixture vendor rejected input".into(),
            }
            .into());
        }
        if self.source.unknown_send {
            anyhow::bail!("fixture response was lost");
        }
        let turn = format!("native-turn-{}", self.source.sends.load(Ordering::SeqCst));
        *self.last.lock().unwrap() = Some(turn.clone());
        self.sender
            .send(Ok(SessionMessage::Control(ControlNotice::Active {
                native_id: self.native.clone(),
                native_turn_id: Some(turn.clone()),
            })))
            .await?;
        self.raw(serde_json::json!({"type":"event_msg","payload":{"type":"task_started","turn_id":turn}})).await?;
        self.raw(serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("input-{turn}"),"role":"user","content":[{"type":"input_text","text":input.text}]}})).await?;
        self.raw(serde_json::json!({"type":"response_item","payload":{"type":"message","id":format!("answer-{turn}"),"role":"assistant","content":[{"type":"output_text","text":"Fixture answer"}]}})).await?;
        if self.source.emit_request {
            self.sender
                .send(Ok(SessionMessage::Request(serde_json::from_value(
                    serde_json::json!({
                        "id":"native-request", "kind":"approval", "title":"Fixture permission",
                        "options":[{"id":"allow","label":"Allow once"},{"id":"deny","label":"Deny"}]
                    }),
                )?)))
                .await?;
        } else {
            self.raw(serde_json::json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":turn}})).await?;
            self.sender
                .send(Ok(SessionMessage::Control(ControlNotice::Settled {
                    native_id: self.native.clone(),
                    native_turn_id: Some(turn),
                })))
                .await?;
        }
        Ok(())
    }
    async fn send_initial(&self, input: TurnInput) -> Result<NativeId> {
        self.send(input).await?;
        Ok(self.native.clone())
    }
    async fn last_sent_turn_id(&self) -> Option<String> {
        self.last.lock().unwrap().clone()
    }
    async fn interrupt(&self) -> Result<()> {
        self.source.interrupts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn steer(&self, _: &str, _: Option<&str>) -> Result<()> {
        Ok(())
    }
    async fn queue(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn set_config(&self, _: &str, _: serde_json::Value) -> Result<Vec<ConfigOption>> {
        Ok(vec![])
    }
    async fn resolve(&self, resolution: Resolution) -> Result<()> {
        assert_eq!(resolution.request_id, "native-request");
        self.source.resolutions.fetch_add(1, Ordering::SeqCst);
        if self.source.unknown_resolution {
            anyhow::bail!("fixture resolution response was lost");
        }
        Ok(())
    }
    fn messages(&mut self) -> Option<ControlStream> {
        self.events
            .take()
            .map(|rx| Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)) as ControlStream)
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}
