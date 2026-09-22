//! Deterministic Codex-shaped acquisition fixture. This feature replaces the runtime
//! adapter factory, so no vendor CLI, credentials, or paid model is reachable.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use sylph_protocol::*;
use tokio::sync::{mpsc, Mutex};

pub const AGENT_NAME: &str = "codex";
const CONTEXT_WINDOW: i64 = 200_000;
const REFUSED_VALUE: &str = "refused";

/// A long model catalogue: `(value, display name, provider)`.
///
/// **Seventeen entries, and the count is the test.** The threshold between "a menu" and
/// "a searchable list" is a real behaviour and nothing could observe it while every
/// fixture option had two choices — every symptom of a catalogue being unscrollable and
/// unsearchable was invisible to a green suite. Seventeen is well above the client's
/// search threshold of 8 and in the range real agents report: `pi` declares 21 across
/// providers and `claude` around a dozen. The ids are deliberately long enough to be the
/// kind that once widened a `<select>` past the right edge of a phone.
///
/// Descriptions carry the provider, so a spec can assert the filter matches the hint and
/// not only the label.
const CATALOGUE: &[(&str, &str, &str)] = &[
    ("opus-5-1m", "Opus 5 (1M context)", "Anthropic"),
    ("opus-5", "Opus 5", "Anthropic"),
    ("sonnet-5", "Sonnet 5", "Anthropic"),
    ("haiku-4-5", "Haiku 4.5", "Anthropic"),
    ("fable-5", "Fable 5", "Anthropic"),
    ("gpt-5-codex-preview", "GPT-5 Codex Preview", "OpenAI"),
    ("gpt-5-mini", "GPT-5 Mini", "OpenAI"),
    ("o5-reasoning", "o5 Reasoning", "OpenAI"),
    ("kimi-k2-thinking", "Kimi K2 Thinking", "Moonshot"),
    ("kimi-k2", "Kimi K2", "Moonshot"),
    ("llama-4-405b-instruct", "Llama 4 405B Instruct", "Meta"),
    ("mistral-large-3", "Mistral Large 3", "Mistral"),
    ("qwen-3-max", "Qwen 3 Max", "Alibaba"),
    ("deepseek-v4", "DeepSeek V4", "DeepSeek"),
    // Two providers offering one model under one display name, which is a **measured**
    // shape rather than an invented one: pi 0.84.3 offers two "Claude Opus 5" (anthropic
    // direct and via openrouter) and two each of "DeepSeek V4 Pro" and "GLM-5.2". The
    // reported bug was both rows showing a checkmark, because a producer's
    // `checked: value == current_value` is a *filter* and the bare id was not unique —
    // and the bare id also made one of each pair unreachable, since a by-id lookup can
    // only answer with one provider.
    //
    // Values are provider-qualified for the same reason `pi`'s now are, and delimited
    // with a **space** rather than a `/`: 350 of pi's 372 ids already contain a slash, so
    // splitting on one mis-parses the majority of the catalogue. The pair exists so a
    // spec can prove exactly one row reads as current and that each provider is
    // separately reachable.
    ("ikun glm-5.2", "GLM-5.2", "ikun-glm"),
    ("huaweicloud glm-5.2", "GLM-5.2", "huaweicloud"),
    // Refused by `set_config`, so the client's failure path is observable.
    (REFUSED_VALUE, "Unavailable Model", "Refuses to be set"),
];

/// The commands this agent reports.
///
/// **Deliberately a mix of prompt and control kinds.** The UI's routing between "send this
/// as a turn" and "open a picker" is a real behaviour, and a fixture reporting only
/// prompts could not exercise it.
fn commands() -> Vec<sylph_protocol::CommandSpec> {
    use sylph_protocol::{CommandKind, CommandSpec};
    let spec = |name: &str, description: &str, kind: CommandKind| CommandSpec {
        name: name.to_string(),
        description: description.to_string(),
        arg_hint: None,
        source: Some("builtin".to_string()),
        kind,
        config_id: None,
        aliases: Vec::new(),
    };
    vec![
        spec("status", "Report runner status", CommandKind::Prompt),
        CommandSpec {
            arg_hint: Some("[target]".to_string()),
            source: Some("skill".to_string()),
            ..spec("verify", "Verify a change end to end", CommandKind::Prompt)
        },
        CommandSpec {
            aliases: vec!["cost".to_string(), "stats".to_string()],
            ..spec("usage", "Show session cost", CommandKind::Prompt)
        },
        CommandSpec {
            config_id: Some("model".to_string()),
            ..spec("model", "Choose the model", CommandKind::Control)
        },
        // A control command onto the *long* catalogue. The dialog a control command opens
        // reaches the filterable list through a different surface than the composer's
        // popover does — an overlay owning its own dismissal — so without this the
        // dialog's search path had no coverage at all.
        CommandSpec {
            config_id: Some("provider".to_string()),
            ..spec(
                "provider",
                "Choose the provider model",
                CommandKind::Control,
            )
        },
    ]
}

/// The live config surface.
///
/// Three options covering both ARIA contracts on one screen, which is why the short and
/// long selects share a `category`: below the search threshold a config option renders as
/// a menu of radio items, and above it as a combobox over a listbox. That is a different
/// contract rather than merely a different width, so a fixture declaring only one size
/// leaves the other untested.
fn initial_config() -> Vec<ConfigOption> {
    let choice = |value: &str, name: &str, description: Option<&str>| ConfigChoice {
        value: value.to_string(),
        name: name.to_string(),
        description: description.map(str::to_string),
    };
    vec![
        // Two choices, so the menu pattern stays covered.
        ConfigOption {
            id: "model".to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: ConfigKind::Select,
            current_value: Some(json!("fast")),
            options: vec![
                choice("fast", "Fast", None),
                choice("thorough", "Thorough", None),
            ],
        },
        // Above the threshold, and in the same category so it lands on the composer's
        // control row beside the short one — which is what lets one screen assert both.
        ConfigOption {
            id: "provider".to_string(),
            name: "Provider model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: ConfigKind::Select,
            current_value: Some(json!("sonnet-5")),
            options: CATALOGUE
                .iter()
                .map(|(value, name, provider)| choice(value, name, Some(provider)))
                .collect(),
        },
        ConfigOption {
            id: "verbose".to_string(),
            name: "Verbose".to_string(),
            description: None,
            category: Some("output".to_string()),
            kind: ConfigKind::Boolean,
            current_value: Some(json!(false)),
            options: Vec::new(),
        },
    ]
}

/// The whole surface this agent reports on open.
fn initial_surface() -> SessionCapabilities {
    SessionCapabilities {
        commands: commands(),
        subagents: vec![SubagentSpec {
            name: "Explore".to_string(),
            description: "Read-only search".to_string(),
        }],
        config: initial_config(),
        permission_mode: Some("default".to_string()),
        context_window: Some(CONTEXT_WINDOW),
    }
}
#[derive(Default)]
pub struct E2eAdapter;
impl E2eAdapter {
    pub fn new() -> Self {
        Self
    }
}
#[async_trait]
impl Adapter for E2eAdapter {
    async fn probe(&self) -> Result<AgentInfo> {
        Ok(AgentInfo {
            name: AGENT_NAME.into(),
            version: Some("fixture-1".into()),
            flags: BTreeMap::new(),
            supports: vec![
                Capability::Interrupt,
                Capability::Queue,
                Capability::SetConfig,
            ],
            dictates_native_id: true,
            native_fidelity: Fidelity::Messages,
        })
    }
    async fn run(
        &self,
        spec: RunSpec,
        capture: Arc<dyn RawCaptureSink>,
    ) -> Result<Box<dyn Session>> {
        let id = match spec.native_id {
            RequestedNativeId::Dictate(id) | RequestedNativeId::Resume(id) => id,
            RequestedNativeId::New => {
                return Err(anyhow!("fixture requires dictated native identity"))
            }
        };
        let stream = format!("fixture/{}/{}", id, uuid::Uuid::new_v4());
        let raw = json!({"method":"thread/started","params":{"thread":{"id":id.as_str()}}});
        let evidence = record(&capture, &stream, raw).await?;
        let (tx, rx) = mpsc::channel(128);
        Ok(Box::new(FixtureSession {
            id,
            capture,
            stream,
            evidence,
            events: Mutex::new(Some(rx)),
            tx,
            turn: Arc::new(AtomicU64::new(0)),
            active: Arc::new(Mutex::new(None)),
            surface: Mutex::new(initial_surface()),
            worker: Mutex::new(None),
            queued: Arc::new(Mutex::new(std::collections::VecDeque::new())),
        }))
    }
    async fn native_sources(&self, _: &NativeId) -> Result<Vec<NativeSource>> {
        Ok(vec![])
    }
    async fn enumerate(&self, _: &StoreFilter) -> Result<Vec<NativeSummary>> {
        Ok(vec![])
    }
}
async fn record(
    capture: &Arc<dyn RawCaptureSink>,
    stream: &str,
    value: serde_json::Value,
) -> Result<trace::ObservationRef> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    capture
        .capture(RawCapture {
            stream: stream.into(),
            transport: trace::TransportKind::LiveStdout,
            bytes,
            captured_at_ms: crate::now_ms(),
        })
        .await
}
struct FixtureSession {
    id: NativeId,
    capture: Arc<dyn RawCaptureSink>,
    stream: String,
    evidence: trace::ObservationRef,
    events: Mutex<Option<mpsc::Receiver<Result<SessionMessage>>>>,
    tx: mpsc::Sender<Result<SessionMessage>>,
    turn: Arc<AtomicU64>,
    active: Arc<Mutex<Option<String>>>,
    surface: Mutex<SessionCapabilities>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    queued: Arc<Mutex<std::collections::VecDeque<(sylph_protocol::CommandRequestId, String)>>>,
}
#[async_trait]
impl Session for FixtureSession {
    fn native_id(&self) -> Option<NativeId> {
        Some(self.id.clone())
    }
    async fn identity_evidence(&self) -> Vec<trace::ObservationRef> {
        vec![self.evidence.clone()]
    }
    async fn capabilities(&self) -> Result<SessionCapabilities> {
        Ok(self.surface.lock().await.clone())
    }
    async fn send(
        &self,
        command_id: &sylph_protocol::CommandRequestId,
        input: TurnInput,
    ) -> Result<()> {
        if self.active.lock().await.is_some() {
            return Err(anyhow!("fixture already has an active turn"));
        }
        let ordinal = self.turn.fetch_add(1, Ordering::SeqCst) + 1;
        let turn = format!("turn-{ordinal}");
        *self.active.lock().await = Some(turn.clone());
        record(&self.capture,&self.stream,json!({"method":"turn/started","params":{"threadId":self.id.as_str(),"turn":{"id":turn,"status":"inProgress"}}})).await?;
        let _ = self
            .tx
            .send(Ok(SessionMessage::Control(ControlNotice::Active {
                native_id: self.id.clone(),
                native_turn_id: Some(turn.clone()),
            })))
            .await;
        record(&self.capture,&self.stream,json!({"method":"item/completed","params":{"threadId":self.id.as_str(),"turnId":turn,"item":{"type":"userMessage","id":format!("user-{ordinal}"),"clientId":sylph_protocol::trace::input_client_id(command_id.as_str()),"content":[{"type":"text","text":input.text,"text_elements":[]}]}}})).await?;
        let capture = self.capture.clone();
        let stream = self.stream.clone();
        let id = self.id.clone();
        let tx = self.tx.clone();
        let active = self.active.clone();
        let queued = self.queued.clone();
        let next_turn = self.turn.clone();
        let worker = tokio::spawn(async move {
            let mut ordinal = ordinal;
            let mut turn = turn;
            let mut prompt = input.text;
            loop {
                let delay = if prompt.contains("paced") || prompt.contains("slow") {
                    1500
                } else {
                    80
                };
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                let mut held = active.lock().await;
                if held.as_deref() != Some(&turn) {
                    return;
                }
                let text = if prompt.starts_with("echo ") {
                    prompt.trim_start_matches("echo ").to_owned()
                } else {
                    "Hello from the E2E runner.".into()
                };
                if record(&capture,&stream,json!({"method":"item/agentMessage/delta","params":{"threadId":id.as_str(),"turnId":turn,"itemId":format!("answer-{ordinal}"),"delta":text}})).await.is_err(){return;}
                if record(&capture,&stream,json!({"method":"item/completed","params":{"threadId":id.as_str(),"turnId":turn,"item":{"type":"agentMessage","id":format!("answer-{ordinal}"),"text":text,"phase":"final"}}})).await.is_err(){return;}
                if record(&capture,&stream,json!({"method":"turn/completed","params":{"threadId":id.as_str(),"turn":{"id":turn,"status":"completed"}}})).await.is_err(){return;}
                let _ = tx
                    .send(Ok(SessionMessage::Control(ControlNotice::Settled {
                        native_id: id.clone(),
                        native_turn_id: Some(turn),
                    })))
                    .await;
                let Some((next_command, next)) = queued.lock().await.pop_front() else {
                    *held = None;
                    return;
                };
                prompt = next;
                ordinal = next_turn.fetch_add(1, Ordering::SeqCst) + 1;
                turn = format!("turn-{ordinal}");
                *held = Some(turn.clone());
                if record(&capture,&stream,json!({"method":"turn/started","params":{"threadId":id.as_str(),"turn":{"id":turn,"status":"inProgress"}}})).await.is_err(){return;}
                let _ = tx
                    .send(Ok(SessionMessage::Control(ControlNotice::Active {
                        native_id: id.clone(),
                        native_turn_id: Some(turn.clone()),
                    })))
                    .await;
                if record(&capture,&stream,json!({"method":"item/completed","params":{"threadId":id.as_str(),"turnId":turn,"item":{"type":"userMessage","id":format!("user-{ordinal}"),"clientId":sylph_protocol::trace::input_client_id(next_command.as_str()),"content":[{"type":"text","text":prompt,"text_elements":[]}]}}})).await.is_err(){return;}
            }
        });
        *self.worker.lock().await = Some(worker);
        Ok(())
    }
    async fn send_initial(
        &self,
        command_id: &sylph_protocol::CommandRequestId,
        input: TurnInput,
    ) -> Result<NativeId> {
        self.send(command_id, input).await?;
        Ok(self.id.clone())
    }
    async fn last_sent_turn_id(&self) -> Option<String> {
        self.active.lock().await.clone()
    }
    async fn interrupt(&self) -> Result<()> {
        if let Some(worker) = self.worker.lock().await.take() {
            worker.abort();
            let _ = worker.await;
        }
        if let Some(turn) = self.active.lock().await.take() {
            record(&self.capture,&self.stream,json!({"method":"turn/completed","params":{"threadId":self.id.as_str(),"turn":{"id":turn,"status":"interrupted"}}})).await?;
            let _ = self
                .tx
                .send(Ok(SessionMessage::Control(ControlNotice::Settled {
                    native_id: self.id.clone(),
                    native_turn_id: Some(turn),
                })))
                .await;
        }
        Ok(())
    }
    async fn steer(
        &self,
        _: &sylph_protocol::CommandRequestId,
        _: &str,
        _: Option<&str>,
    ) -> Result<()> {
        Err(anyhow!("fixture has no steer"))
    }
    async fn queue(&self, command_id: &sylph_protocol::CommandRequestId, text: &str) -> Result<()> {
        let active = self.active.lock().await;
        if active.is_some() {
            self.queued
                .lock()
                .await
                .push_back((command_id.clone(), text.into()));
            return Ok(());
        }
        drop(active);
        self.send(
            command_id,
            TurnInput {
                text: text.into(),
                ..Default::default()
            },
        )
        .await
    }
    async fn set_config(&self, id: &str, value: serde_json::Value) -> Result<Vec<ConfigOption>> {
        let mut surface = self.surface.lock().await;
        let option = surface
            .config
            .iter_mut()
            .find(|option| option.id == id)
            .ok_or_else(|| {
                AdapterError::unsupported_field(format!("no such config option: {id}"))
            })?;
        if value.as_str() == Some(REFUSED_VALUE) {
            return Err(AdapterError::invalid_value("this agent refuses that value").into());
        }
        match option.kind {
            ConfigKind::Boolean if !value.is_boolean() => {
                return Err(AdapterError::invalid_value("expected a boolean").into())
            }
            ConfigKind::Select
                if !option
                    .options
                    .iter()
                    .any(|choice| Some(choice.value.as_str()) == value.as_str()) =>
            {
                return Err(AdapterError::invalid_value("value is not an offered choice").into())
            }
            _ => {}
        }
        option.current_value = Some(value);
        Ok(surface.config.clone())
    }
    async fn resolve(&self, _: Resolution) -> Result<()> {
        Ok(())
    }
    fn messages(&mut self) -> Option<ControlStream> {
        self.events.try_lock().ok()?.take().map(|rx| {
            Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|value| (value, rx))
            })) as ControlStream
        })
    }
    async fn close(&self) -> Result<()> {
        self.interrupt().await
    }
}
