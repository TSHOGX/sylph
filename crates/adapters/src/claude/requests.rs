//! `claude` asking a human something, and the human's answer going back.
//!
//! The request surface: `_vendor_request`, `_forward_question`,
//! `_forward_dialog`, `_risk` and `answer` (roughly :722-1000, :531-640).
//! `docs/session-protocol.md` § Requests governs.
//!
//! # Why the answer shapes are in their own module with their own tests
//!
//! **This is where the single most expensive vendor-key defect in this adapter's history
//! lived.** The reply wrote `questions[0].selectedOptions` — a key that occurs *nowhere*
//! in the 321 MB CLI binary, re-checked here against 2.1.251 and still zero. The CLI
//! built its tool result by reading `answers[question]`, found nothing, and told the
//! model *"The user did not answer the questions."* So a correct pick, correctly
//! rendered and correctly transported, presented as a UI that had lost the click, with
//! nothing erroring at any layer.
//!
//! The lesson that shaped this module: **assert on the payload the vendor reads, not on
//! whether the dialog closed.** A live dialog check passed throughout the bug, because
//! it asserted that a dialog arrived and that answering unblocked the turn — both true
//! while the answer was being dropped. So [`answer_payload`] is a pure function over
//! `(kind, resolution)` and the tests below assert its exact keys.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use sylph_protocol::{
    Choice, Field, FieldKind, Form, NotesSpec, Request, RequestKind, RequestOption, Resolution,
    Risk, SelectMode,
};

/// Which response shape a pending vendor request expects.
///
/// **Remembered from the request rather than inferred from the answer**, because sending
/// the wrong shape is not a validation error the CLI reports: it parks the dialog and the
/// turn stalls until its deadline, which reads as the agent hanging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskKind {
    /// A `can_use_tool` permission. Wants `{behavior: allow|deny}`.
    Permission,
    /// `AskUserQuestion`, which is a permission on the wire and a question in substance.
    /// Wants `{behavior: allow, updatedInput: {answers, annotations}}`.
    Question {
        /// The question texts in the order they were forwarded.
        ///
        /// **The text is the CLI's own key into `answers`**, so it has to survive the
        /// round trip — but sending it as a field id would put a whole sentence, and
        /// whatever punctuation it contains, into an identifier a client echoes back.
        /// Field ids are positional (`q:0`) and this is the map back.
        texts: Vec<String>,
        /// The tool input as it arrived, so the answers are written into the shape the
        /// tool expects rather than replacing it.
        input: Value,
    },
    /// A `request_user_dialog`. Wants `{behavior: completed|cancelled}`.
    Dialog,
}

/// One vendor `control_request`, projected.
///
/// `None` means this adapter does not model the subtype, and the caller must answer the
/// CLI with an error rather than leaving it blocked — a parked request stalls the turn
/// until its deadline.
pub struct Ask {
    pub request: Request,
    pub kind: AskKind,
}

/// Project one `control_request` payload onto a [`Request`].
///
/// `payload` is the whole envelope: `{type, request_id, request: {subtype, …}}`.
pub fn ask_from(payload: &Value) -> Option<Ask> {
    let vendor_id = payload.get("request_id").and_then(Value::as_str)?;
    let request = payload.get("request")?;
    match request.get("subtype").and_then(Value::as_str)? {
        "can_use_tool" => Some(permission_ask(vendor_id, request)),
        "request_user_dialog" => Some(dialog_ask(vendor_id, request)),
        _ => None,
    }
}

/// A `can_use_tool`, which is either a permission or a question depending on the tool.
fn permission_ask(vendor_id: &str, request: &Value) -> Ask {
    let tool = request.get("tool_name").and_then(Value::as_str);

    // **`AskUserQuestion` is a permission in form and a question in substance**: its input
    // carries the agent's own options, and answering it "allow" merely lets the agent ask
    // — which is what made a terminal's picker come back here as a paragraph. Verified
    // against the real CLI: there is no separate dialog subtype for it, and the
    // `permission_ask_user_question` *dialog kind* this adapter declares is what stops the
    // CLI degrading the flow to prose. Two different mechanisms, one name.
    if tool == Some("AskUserQuestion") {
        return question_ask(vendor_id, request);
    }

    // Whether a lasting grant may be offered **at all**. The CLI sets this on asks where
    // accepting always would write a permission rule *broader than the ask itself*, and
    // its schema says a host should omit any persistent-rule affordance when it is set.
    // Offering it anyway turns a narrow yes into a wide one the user never read, so the
    // option is withheld rather than merely labelled differently.
    let allow_persistent = !request
        .get("suppressAlwaysAllowRule")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut options = vec![RequestOption {
        id: "allow".to_string(),
        label: "Allow once".to_string(),
        persistent: false,
        description: None,
    }];
    if allow_persistent {
        options.push(RequestOption {
            id: "allow_always".to_string(),
            label: format!("Always allow {}", tool.unwrap_or("this")),
            persistent: true,
            description: None,
        });
    }
    options.push(RequestOption {
        id: "deny".to_string(),
        label: "Deny".to_string(),
        persistent: false,
        description: None,
    });

    let title = request
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "Allow {}?",
                request
                    .get("display_name")
                    .and_then(Value::as_str)
                    .or(tool)
                    .unwrap_or("this action")
            )
        });

    Ask {
        request: Request {
            id: vendor_id.to_string(),
            kind: RequestKind::Approval,
            tool_use_id: request
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool: tool.map(str::to_string),
            title,
            detail: request.get("input").cloned(),
            options,
            // An approval is the one kind that legitimately declares no form: its subject
            // *is* one proposed tool input, which a client renders from `detail` and may
            // send back edited.
            form: None,
            default: Some("deny".to_string()),
            risk: Some(risk_of(tool, request.get("input"))),
            allow_persistent,
            expires_at_ms: None,
        },
        kind: AskKind::Permission,
    }
}

/// An `AskUserQuestion` call, as a form the user fills in.
///
/// **Every question is forwarded**, not only the first. *Measured against claude
/// 2.1.238:* answering all of them is delivered to the model as *"Your questions have
/// been answered … you can now continue with these answers in mind"*, while answering one
/// of two arrives as the weaker *"The user answered …"*. Both look like success from
/// outside, so a partial answer degrades the reply quietly. The tool caps a call at four
/// questions, so the panel stays finite.
fn question_ask(vendor_id: &str, request: &Value) -> Ask {
    let input = request.get("input").cloned().unwrap_or(Value::Null);
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut fields = Vec::new();
    let mut texts = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        if !question.is_object() {
            continue;
        }
        let text = question
            .get("question")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Question {}", index + 1));
        texts.push(text.clone());
        let multi = question
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let choices = question
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        // `value` is the **label**, because the label is what the CLI
                        // matches on when it builds the tool result — it never sent an id.
                        let (label, description) = match option {
                            Value::String(text) => (text.clone(), None),
                            _ => (
                                option
                                    .get("label")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                option
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            ),
                        };
                        if label.is_empty() {
                            return None;
                        }
                        Some(Choice {
                            value: label,
                            label: None,
                            description,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        fields.push(Field {
            id: format!("q:{index}"),
            kind: FieldKind::Select {
                mode: if multi {
                    SelectMode::Many
                } else {
                    SelectMode::One
                },
                choices,
                // The CLI's own dialog always offers an "Other" row and treats what is
                // typed there as *the answer*, not as a note on an offered option.
                // Withholding it would make this client strictly less able to answer than
                // the terminal it is modelled on.
                allow_other: true,
                other_label: None,
                // Notes ride in `annotations`, which the CLI reads *alongside* the pick
                // rather than instead of it. Offered only on a single select, matching the
                // terminal, where a note qualifies one chosen option.
                notes: if multi {
                    None
                } else {
                    Some(NotesSpec {
                        label: None,
                        placeholder: None,
                    })
                },
            },
            label: text,
            description: question
                .get("header")
                .and_then(Value::as_str)
                .map(str::to_string),
            required: false,
        });
    }

    let title = if texts.len() == 1 {
        texts[0].clone()
    } else {
        "Claude has a question".to_string()
    };

    Ask {
        request: Request {
            id: vendor_id.to_string(),
            kind: RequestKind::Choice,
            tool_use_id: request
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            tool: Some("AskUserQuestion".to_string()),
            title,
            detail: Some(json!({"questions": questions})),
            // **Verbs, not answers.** The form carries what was chosen, so an option is
            // what to *do* with it. `Choice` previously overloaded `options` as both,
            // which is why picking had to submit immediately and why a note had nowhere
            // to live.
            options: vec![
                RequestOption {
                    id: "submit".to_string(),
                    label: "Send answer".to_string(),
                    persistent: false,
                    description: None,
                },
                RequestOption {
                    id: "deny".to_string(),
                    label: "Don't answer".to_string(),
                    persistent: false,
                    description: None,
                },
            ],
            form: Some(Form { fields }),
            default: Some("deny".to_string()),
            risk: Some(Risk::Safe),
            allow_persistent: false,
            expires_at_ms: None,
        },
        kind: AskKind::Question { texts, input },
    }
}

/// A `request_user_dialog`, whose payload differs per `dialog_kind`.
///
/// A kind whose payload cannot be read still forwards as a two-option accept/reject
/// rather than being dropped: the turn is blocked either way, and a coarse choice beats
/// no choice.
///
/// **There is deliberately no `permission_ask_user_question` branch.** The kind is
/// declared at `initialize` — the declaration is what stops the CLI degrading the flow to
/// prose — but measured against 2.1.238 the ask arrives as a `can_use_tool` for the
/// `AskUserQuestion` tool and never as a dialog. A branch here was dead code that also
/// answered in the wrong shape, and a plausible-looking dead branch is worse than none:
/// it reads as the path being handled.
fn dialog_ask(vendor_id: &str, request: &Value) -> Ask {
    let kind = request
        .get("dialog_kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = request.get("payload").cloned().unwrap_or(Value::Null);

    let two = |allow: &str, deny: &str| {
        vec![
            RequestOption {
                id: "allow".to_string(),
                label: allow.to_string(),
                persistent: false,
                description: None,
            },
            RequestOption {
                id: "deny".to_string(),
                label: deny.to_string(),
                persistent: false,
                description: None,
            },
        ]
    };

    let mut request_out = Request {
        id: vendor_id.to_string(),
        kind: RequestKind::Dialog,
        tool_use_id: request
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        tool: None,
        title: String::new(),
        detail: None,
        options: Vec::new(),
        form: None,
        default: Some("deny".to_string()),
        risk: None,
        allow_persistent: true,
        expires_at_ms: None,
    };

    match kind {
        "permission_enter_plan_mode" => {
            request_out.title = "Approve this plan?".to_string();
            // The plan text is the thing being decided on, so it goes where a client
            // renders content rather than into a label.
            request_out.detail = Some(json!({
                "plan": payload.get("plan").or_else(|| payload.get("message"))
                    .and_then(Value::as_str).unwrap_or_default(),
            }));
            request_out.options = vec![
                RequestOption {
                    id: "allow".to_string(),
                    label: "Approve".to_string(),
                    persistent: false,
                    description: None,
                },
                RequestOption {
                    id: "allow_always".to_string(),
                    label: "Approve and auto-accept edits".to_string(),
                    persistent: true,
                    description: None,
                },
                RequestOption {
                    id: "deny".to_string(),
                    label: "Keep planning".to_string(),
                    persistent: false,
                    description: None,
                },
            ];
        }
        "cost_threshold" => {
            request_out.title = "Continue past the spending threshold?".to_string();
            request_out.detail = Some(payload);
            request_out.options = two("Continue", "Stop here");
        }
        // A permission-shaped dialog carries the tool call it is gating.
        _ if kind.starts_with("permission_") => {
            request_out.kind = RequestKind::Approval;
            let tool = payload
                .get("tool_name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| {
                    kind.strip_prefix("permission_").unwrap_or(kind).to_string()
                });
            request_out.title = payload
                .get("title")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Allow this action?")
                .to_string();
            request_out.risk = Some(risk_of(Some(tool.as_str()), payload.get("input")));
            request_out.tool = Some(tool);
            request_out.detail = Some(
                payload
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| payload.clone()),
            );
            request_out.options = vec![
                RequestOption {
                    id: "allow".to_string(),
                    label: "Allow once".to_string(),
                    persistent: false,
                    description: None,
                },
                RequestOption {
                    id: "allow_always".to_string(),
                    label: "Always allow".to_string(),
                    persistent: true,
                    description: None,
                },
                RequestOption {
                    id: "deny".to_string(),
                    label: "Deny".to_string(),
                    persistent: false,
                    description: None,
                },
            ];
        }
        _ => {
            request_out.title = payload
                .get("title")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Claude asks")
                .to_string();
            let mut detail = Map::new();
            detail.insert("dialogKind".to_string(), Value::String(kind.to_string()));
            if let Some(object) = payload.as_object() {
                for (key, value) in object {
                    detail.insert(key.clone(), value.clone());
                }
            }
            request_out.detail = Some(Value::Object(detail));
            request_out.options = two("Continue", "Cancel");
        }
    }

    Ask {
        request: request_out,
        kind: AskKind::Dialog,
    }
}

/// Advisory emphasis only.
///
/// Deliberately crude: a real risk model belongs in the agent's own permission rules, not
/// in a display hint.
fn risk_of(tool: Option<&str>, input: Option<&Value>) -> Risk {
    match tool {
        Some("Write" | "Edit" | "NotebookEdit") => Risk::Destructive,
        Some("WebFetch" | "WebSearch") => Risk::Network,
        Some("Bash") => {
            let command = input
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if ["rm ", "git push", "curl ", "sudo "]
                .iter()
                .any(|token| command.contains(token))
            {
                Risk::Destructive
            } else {
                Risk::Safe
            }
        }
        _ => Risk::Safe,
    }
}

/// A form answer, split into picks and notes.
///
/// The `{values, notes}` envelope is **Sylph's transport, not a shape any vendor dialog
/// expects back**, so every consumer here unwraps it rather than forwarding it.
fn form_answer_of(input: Option<&Value>) -> (BTreeMap<String, Value>, BTreeMap<String, String>) {
    let mut values = BTreeMap::new();
    let mut notes = BTreeMap::new();
    let Some(input) = input else {
        return (values, notes);
    };
    if let Some(object) = input.get("values").and_then(Value::as_object) {
        for (key, value) in object {
            values.insert(key.clone(), value.clone());
        }
    }
    if let Some(object) = input.get("notes").and_then(Value::as_object) {
        for (key, value) in object {
            if let Some(text) = value.as_str() {
                notes.insert(key.clone(), text.to_string());
            }
        }
    }
    (values, notes)
}

/// The `control_response` body for one answered request.
///
/// Pure over `(kind, resolution)` so the wire shape is assertable without a process —
/// which is the whole lesson of the `selectedOptions` defect: "the panel closed" and "the
/// answer arrived" are different claims, and only the second is the feature.
pub fn answer_payload(kind: &AskKind, resolution: &Resolution) -> Value {
    let allow = matches!(resolution.option.as_str(), "allow" | "allow_always");
    match kind {
        AskKind::Question { texts, input } => {
            if matches!(resolution.option.as_str(), "deny" | "cancel") {
                // Declining to answer is a denied tool call, which the agent handles as
                // "the user did not pick one" rather than as an error. **`message`, never
                // `feedback`**: measured against 2.1.238, a `feedback` key here is
                // rejected as a malformed permission result and the model is told the
                // callback is broken — so the user's refusal is reported as an
                // integration bug.
                return json!({
                    "behavior": "deny",
                    "message": "The user declined to answer.",
                });
            }

            let (values, notes) = form_answer_of(resolution.input.as_ref());
            let mut answers = Map::new();
            let mut annotations = Map::new();
            for (index, text) in texts.iter().enumerate() {
                let id = format!("q:{index}");
                let picked = match values.get(&id) {
                    // Multi-select arrives as a list and is delivered **comma-joined**,
                    // which is what the tool's own output schema documents and what its
                    // parser reads back. The flattening happens here because it is a
                    // vendor detail; the protocol carries a list.
                    Some(Value::Array(items)) => items
                        .iter()
                        .map(|item| match item {
                            // A bare string, not its JSON spelling: the CLI matches the
                            // label it offered, and `"Tabs"` with quotes matches nothing.
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        })
                        .filter(|item| !item.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(", "),
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => other.to_string(),
                };
                if !picked.is_empty() {
                    answers.insert(text.clone(), Value::String(picked));
                }
                if let Some(note) = notes.get(&id).map(|note| note.trim()).filter(|n| !n.is_empty())
                {
                    annotations.insert(text.clone(), json!({"notes": note}));
                }
            }

            // Written **into the input the tool sent**, because the CLI builds its tool
            // result by reading `answers[question]` off the updated input. This is the key
            // the `selectedOptions` bug missed.
            let mut updated = match input {
                Value::Object(object) => object.clone(),
                _ => Map::new(),
            };
            let empty = answers.is_empty() && annotations.is_empty();
            updated.insert("answers".to_string(), Value::Object(answers));
            if !annotations.is_empty() {
                updated.insert("annotations".to_string(), Value::Object(annotations));
            }
            if empty {
                // Submitted with nothing filled in. Sent as `response` rather than as
                // empty answers, because the CLI reads that as "the user replied in prose"
                // instead of "the user answered nothing".
                updated.insert(
                    "response".to_string(),
                    Value::String("The user submitted no answer.".to_string()),
                );
            }
            json!({"behavior": "allow", "updatedInput": Value::Object(updated)})
        }
        AskKind::Permission => {
            if !allow {
                return json!({
                    "behavior": "deny",
                    "message": "The user declined this request.",
                });
            }
            let mut body = Map::new();
            body.insert("behavior".to_string(), Value::String("allow".to_string()));
            if let Some(input) = &resolution.input {
                // **What an editing client sends back**, which is how "allow, but change
                // the command" works — a real terminal affordance a yes/no dialog drops.
                body.insert("updatedInput".to_string(), input.clone());
            }
            if resolution.option == "allow_always" {
                // A lasting grant, which is a policy change rather than one decision —
                // hence a separate option in the UI.
                body.insert(
                    "updatedPermissions".to_string(),
                    json!([{"behavior": "allow"}]),
                );
            }
            Value::Object(body)
        }
        AskKind::Dialog => {
            // `cancelled` is the CLI's own spelling for "the user declined", and it is
            // distinct from a permission's `deny`: the flow was abandoned rather than
            // refused, so Claude continues differently.
            if !allow && resolution.option != "submit" {
                return json!({"behavior": "cancelled"});
            }
            let mut body = Map::new();
            body.insert(
                "behavior".to_string(),
                Value::String("completed".to_string()),
            );
            if let Some(input) = &resolution.input {
                let (values, _) = form_answer_of(Some(input));
                body.insert(
                    "result".to_string(),
                    if values.is_empty() {
                        input.clone()
                    } else {
                        Value::Object(values.into_iter().collect())
                    },
                );
            }
            Value::Object(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylph_protocol::RequestKind;

    /// One real `can_use_tool` envelope, in the CLI's own shape.
    fn permission(tool: &str, input: Value, suppress: bool) -> Value {
        let mut request = json!({
            "subtype": "can_use_tool",
            "tool_name": tool,
            "tool_use_id": "toolu_01ABC",
            "input": input,
        });
        if suppress {
            request["suppressAlwaysAllowRule"] = json!(true);
        }
        json!({"type": "control_request", "request_id": "req-1", "request": request})
    }

    /// One real `AskUserQuestion` envelope, two questions, one of them multi-select.
    fn question_envelope() -> Value {
        json!({
            "type": "control_request",
            "request_id": "req-q",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "AskUserQuestion",
                "tool_use_id": "toolu_01Q",
                "input": {
                    "questions": [
                        {"question": "Indentation?", "header": "Style",
                         "options": [{"label": "Tabs", "description": "Tab characters"},
                                     {"label": "Spaces"}]},
                        {"question": "Linters?", "multiSelect": true,
                         "options": [{"label": "ESLint"}, {"label": "Clippy"}]}
                    ]
                }
            }
        })
    }

    fn answer(kind: &AskKind, option: &str, input: Option<Value>) -> Value {
        answer_payload(
            kind,
            &Resolution {
                request_id: "req".to_string(),
                option: option.to_string(),
                input,
            },
        )
    }

    /// **The single most expensive vendor-key defect in this adapter's history.**
    ///
    /// The answer went into `questions[0].selectedOptions`, a key that occurs nowhere in the
    /// CLI binary — re-grepped against 2.1.251 and still zero. The CLI reads
    /// `answers[question]` off the updated input, found nothing, and told the model *"The
    /// user did not answer the questions."* So this asserts the exact keys the vendor reads,
    /// not that a payload was produced: a live check asserted a dialog arrived and
    /// that answering unblocked the turn — both true throughout the bug.
    #[test]
    fn a_question_answer_lands_under_answers_keyed_by_the_question_text() {
        let ask = ask_from(&question_envelope()).expect("a question is an ask");
        let body = answer(
            &ask.kind,
            "submit",
            Some(json!({"values": {"q:0": "Tabs", "q:1": ["ESLint", "Clippy"]}})),
        );

        assert_eq!(body["behavior"], json!("allow"));
        let answers = &body["updatedInput"]["answers"];
        assert_eq!(answers["Indentation?"], json!("Tabs"));
        // Multi-select is delivered **comma-joined**, which is what the tool's own output
        // schema documents and what its parser reads back. The protocol carries a list; the
        // flattening is a vendor detail and belongs here.
        assert_eq!(answers["Linters?"], json!("ESLint, Clippy"));
        // The key that shipped, asserted absent so its return is a failure rather than a
        // silent regression.
        assert!(
            body["updatedInput"].get("selectedOptions").is_none(),
            "the key the CLI does not read must not reappear"
        );
    }

    /// **Every question is answered, not only the first.**
    ///
    /// *Measured on 2.1.238:* a complete reply is delivered as *"Your questions have been
    /// answered … you can now continue with these answers in mind"*, while one of two
    /// arrives as the weaker *"The user answered …"*. Both look like success from outside,
    /// so the degradation is invisible to an error check.
    #[test]
    fn every_forwarded_question_gets_a_field_and_can_be_answered() {
        let ask = ask_from(&question_envelope()).unwrap();
        let form = ask.request.form.as_ref().expect("a question declares a form");
        assert_eq!(form.fields.len(), 2);
        assert_eq!(form.fields[0].id, "q:0");
        assert_eq!(form.fields[1].id, "q:1");
        // The label is the question text, and the id is positional — sending the text as
        // the id would put a whole sentence into an identifier a client echoes back.
        assert_eq!(form.fields[0].label, "Indentation?");
        assert_eq!(form.fields[0].description.as_deref(), Some("Style"));
    }

    /// The answer the *agent* authored is what a choice's value must be.
    ///
    /// The CLI matches on the **label** when it builds the tool result — it never sent an
    /// id — so a synthesized value would answer with something the vendor cannot match.
    #[test]
    fn a_choice_value_is_the_label_the_vendor_matches_on() {
        let ask = ask_from(&question_envelope()).unwrap();
        let form = ask.request.form.as_ref().unwrap();
        let FieldKind::Select { choices, mode, allow_other, notes, .. } = &form.fields[0].kind
        else {
            panic!("a question is a select");
        };
        assert_eq!(*mode, SelectMode::One);
        assert_eq!(choices[0].value, "Tabs");
        assert_eq!(choices[0].description.as_deref(), Some("Tab characters"));
        // The CLI's own dialog always offers an "Other" row and treats what is typed there
        // as the answer, so withholding it makes this client less able than the terminal.
        assert!(*allow_other);
        // Notes ride in `annotations`, offered only on a single select — matching the
        // terminal, where a note qualifies one chosen option.
        assert!(notes.is_some());

        let FieldKind::Select { mode, notes, .. } = &form.fields[1].kind else {
            panic!("a multi-select is a select");
        };
        assert_eq!(*mode, SelectMode::Many);
        assert!(notes.is_none(), "a multi-select offers no notes");
    }

    /// A note qualifies a pick rather than replacing it, which is why it travels in
    /// `annotations` keyed by the same question text.
    #[test]
    fn a_note_travels_in_annotations_beside_the_pick_not_instead_of_it() {
        let ask = ask_from(&question_envelope()).unwrap();
        let body = answer(
            &ask.kind,
            "submit",
            Some(json!({
                "values": {"q:0": "Spaces"},
                "notes": {"q:0": "  two of them  "},
            })),
        );
        assert_eq!(body["updatedInput"]["answers"]["Indentation?"], json!("Spaces"));
        assert_eq!(
            body["updatedInput"]["annotations"]["Indentation?"]["notes"],
            json!("two of them")
        );
    }

    /// Declining to answer is a **denied tool call**, and the key is `message`.
    ///
    /// *Measured on 2.1.238:* a `feedback` key here is rejected as a malformed permission
    /// result and the model is told the callback is broken — so the user's refusal would be
    /// reported to the agent as an integration bug.
    #[test]
    fn declining_a_question_denies_the_tool_call_with_a_message() {
        let ask = ask_from(&question_envelope()).unwrap();
        let body = answer(&ask.kind, "deny", None);
        assert_eq!(body["behavior"], json!("deny"));
        assert!(body.get("message").is_some());
        assert!(
            body.get("feedback").is_none(),
            "`feedback` is rejected as malformed"
        );
    }

    /// An empty submission says so in prose rather than as empty answers, because the CLI
    /// reads the two differently.
    #[test]
    fn an_empty_submission_is_reported_as_prose_not_as_no_answers() {
        let ask = ask_from(&question_envelope()).unwrap();
        let body = answer(&ask.kind, "submit", Some(json!({"values": {}})));
        assert_eq!(
            body["updatedInput"]["response"],
            json!("The user submitted no answer.")
        );
    }

    /// A permission's edited input is what "allow, but change the command" *is*.
    #[test]
    fn an_approval_forwards_the_edited_input_and_a_lasting_grant() {
        let ask = ask_from(&permission("Bash", json!({"command": "ls"}), false)).unwrap();
        assert_eq!(ask.kind, AskKind::Permission);

        let once = answer(&ask.kind, "allow", Some(json!({"command": "ls -la"})));
        assert_eq!(once["behavior"], json!("allow"));
        assert_eq!(once["updatedInput"]["command"], json!("ls -la"));
        assert!(once.get("updatedPermissions").is_none());

        let always = answer(&ask.kind, "allow_always", None);
        assert_eq!(always["updatedPermissions"], json!([{"behavior": "allow"}]));

        let denied = answer(&ask.kind, "deny", None);
        assert_eq!(denied["behavior"], json!("deny"));
        assert!(denied.get("message").is_some());
    }

    /// **`allow_persistent: false` means the affordance is omitted entirely.**
    ///
    /// The CLI sets `suppressAlwaysAllowRule` on asks where accepting always would write a
    /// rule *broader than the ask itself*, so offering it converts a narrow yes into a wide
    /// one the user never read. Withheld from `options` too, not merely flagged.
    #[test]
    fn a_suppressed_rule_removes_the_always_allow_option_rather_than_labelling_it() {
        let ask = ask_from(&permission("Bash", json!({"command": "ls"}), true)).unwrap();
        assert!(!ask.request.allow_persistent);
        assert!(
            !ask.request.options.iter().any(|option| option.persistent),
            "got {:?}",
            ask.request.options
        );

        let offered = ask_from(&permission("Bash", json!({"command": "ls"}), false)).unwrap();
        assert!(offered.request.allow_persistent);
        assert!(offered.request.options.iter().any(|option| option.id == "allow_always"));
    }

    /// `AskUserQuestion` is a permission on the wire and a question in substance, so it must
    /// not render as an approval — answering "allow" merely lets the agent ask, which is
    /// what made a terminal's picker come back here as a paragraph.
    #[test]
    fn ask_user_question_becomes_a_choice_rather_than_an_approval() {
        let ask = ask_from(&question_envelope()).unwrap();
        assert_eq!(ask.request.kind, RequestKind::Choice);
        assert_eq!(ask.request.tool.as_deref(), Some("AskUserQuestion"));
        // Verbs, not answers: the form carries what was chosen.
        let ids: Vec<&str> = ask.request.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["submit", "deny"]);
    }

    /// A dialog's decline is `cancelled`, which the CLI treats differently from a
    /// permission's `deny`: the flow was abandoned rather than refused.
    #[test]
    fn a_dialog_decline_is_cancelled_not_denied() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-d",
            "request": {"subtype": "request_user_dialog", "dialog_kind": "cost_threshold",
                        "payload": {"spent": 12.5}}
        });
        let ask = ask_from(&envelope).unwrap();
        assert_eq!(ask.kind, AskKind::Dialog);
        assert_eq!(ask.request.title, "Continue past the spending threshold?");
        assert_eq!(ask.request.detail, Some(json!({"spent": 12.5})));

        assert_eq!(answer(&ask.kind, "deny", None), json!({"behavior": "cancelled"}));
        assert_eq!(answer(&ask.kind, "allow", None)["behavior"], json!("completed"));
    }

    /// A dialog's answer is **unwrapped**: the `{values, notes}` envelope is Sylph's
    /// transport, not a shape any vendor dialog expects back.
    #[test]
    fn a_dialog_result_is_unwrapped_from_the_form_envelope() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-e",
            "request": {"subtype": "request_user_dialog", "dialog_kind": "mcp_url_elicitation",
                        "payload": {"message": "Which URL?"}}
        });
        let ask = ask_from(&envelope).unwrap();
        let body = answer(
            &ask.kind,
            "submit",
            Some(json!({"values": {"url": "https://example.test"}})),
        );
        assert_eq!(body["behavior"], json!("completed"));
        assert_eq!(body["result"], json!({"url": "https://example.test"}));
        assert!(
            body["result"].get("values").is_none(),
            "the transport envelope must not reach the vendor"
        );
    }

    /// A plan dialog puts the plan in `detail`, because the plan is the thing being decided
    /// on rather than a label.
    #[test]
    fn a_plan_dialog_carries_its_plan_where_content_is_rendered() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-p",
            "request": {"subtype": "request_user_dialog",
                        "dialog_kind": "permission_enter_plan_mode",
                        "payload": {"plan": "1. Read the file\n2. Fix it"}}
        });
        let ask = ask_from(&envelope).unwrap();
        assert_eq!(ask.request.detail.unwrap()["plan"], json!("1. Read the file\n2. Fix it"));
        assert!(ask.request.options.iter().any(|o| o.id == "allow_always"));
    }

    /// A permission-shaped dialog is an approval and names the tool it gates.
    #[test]
    fn a_permission_shaped_dialog_becomes_an_approval_naming_its_tool() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-b",
            "request": {"subtype": "request_user_dialog", "dialog_kind": "permission_bash",
                        "tool_use_id": "toolu_9",
                        "payload": {"tool_name": "Bash", "input": {"command": "rm -rf build"}}}
        });
        let ask = ask_from(&envelope).unwrap();
        assert_eq!(ask.request.kind, RequestKind::Approval);
        assert_eq!(ask.request.tool.as_deref(), Some("Bash"));
        assert_eq!(ask.request.tool_use_id.as_deref(), Some("toolu_9"));
        assert_eq!(ask.request.risk, Some(Risk::Destructive));
        assert_eq!(ask.request.detail, Some(json!({"command": "rm -rf build"})));
    }

    /// An unrecognized dialog kind is still answerable, because a parked dialog blocks the
    /// turn until its deadline and a coarse choice beats silence.
    #[test]
    fn an_unknown_dialog_kind_is_still_answerable_and_names_itself() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-u",
            "request": {"subtype": "request_user_dialog", "dialog_kind": "something_new",
                        "payload": {"title": "Proceed?"}}
        });
        let ask = ask_from(&envelope).unwrap();
        assert_eq!(ask.request.title, "Proceed?");
        assert_eq!(ask.request.detail.unwrap()["dialogKind"], json!("something_new"));
        assert_eq!(ask.request.options.len(), 2);
    }

    /// A subtype this adapter does not model answers `None`, so the caller knows it has to
    /// tell the CLI rather than leaving it blocked.
    #[test]
    fn an_unmodelled_subtype_is_reported_rather_than_silently_dropped() {
        let envelope = json!({
            "type": "control_request", "request_id": "req-x",
            "request": {"subtype": "some_future_ask"}
        });
        assert!(ask_from(&envelope).is_none());
    }

    /// Risk is advisory, and deliberately crude — but a destructive command must not read
    /// as safe.
    #[test]
    fn risk_marks_the_shapes_a_user_should_look_twice_at() {
        assert_eq!(risk_of(Some("Write"), None), Risk::Destructive);
        assert_eq!(risk_of(Some("WebFetch"), None), Risk::Network);
        assert_eq!(
            risk_of(Some("Bash"), Some(&json!({"command": "git push --force"}))),
            Risk::Destructive
        );
        assert_eq!(risk_of(Some("Bash"), Some(&json!({"command": "ls"}))), Risk::Safe);
        assert_eq!(risk_of(Some("Read"), None), Risk::Safe);
    }
}
