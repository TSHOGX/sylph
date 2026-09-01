//! The inversion: the agent asks, and a human answers.
//!
//! Moved from `backend/src/adapter/wire.rs`. `docs/session-protocol.md` § Requests
//! governs; `AdapterRequest` is renamed [`Request`] to match it, since nothing about
//! this shape is about an adapter subprocess any more.
//!
//! `rename_all_fields` on the tagged enum below is load-bearing, not tidiness — the
//! same measured trap `capabilities` documents.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// What kind of thing is being asked.
///
/// A closed enum where v3 carried a bare `String`, because this decides which surface
/// a client renders and an unrecognized value has no rendering. v3's `SERVER_ACCEPTS`
/// handshake existed to stop an adapter raising a kind nobody would answer; with the
/// adapters linked in, the enum *is* that negotiation and it happens at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    /// One proposed tool input, to allow or refuse. Its subject *is* `detail`, which
    /// is why an approval is the one kind that legitimately declares no [`Form`].
    Approval,
    /// The vendor's own dialog, whatever it is.
    Dialog,
    /// Free text the agent needs. MCP's elicitation.
    Elicitation,
    /// The agent authored the options and wants one or several picked.
    Choice,
}

impl RequestKind {
    /// The wire spelling, which is also what the journal's `kind` column stores.
    ///
    /// One function, for the same reason [`crate::EventKind::as_str`] is one: a storage
    /// write and a wire frame must not be able to disagree about the spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Dialog => "dialog",
            Self::Elicitation => "elicitation",
            Self::Choice => "choice",
        }
    }
}

impl std::fmt::Display for RequestKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How much care an option deserves in the UI. Advisory, for emphasis only.
///
/// Typed where v3 carried a `String`, for the same reason as [`RequestKind`]: a client
/// maps this onto colour and emphasis, and there is nothing to do with a fourth word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Safe,
    Destructive,
    Network,
}

impl Risk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Destructive => "destructive",
            Self::Network => "network",
        }
    }
}

impl std::fmt::Display for Risk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One choice within a [`FieldKind::Select`].
///
/// Separate from [`crate::ConfigChoice`] despite the similar shape: a config choice
/// names a setting's value, while this names an *answer* the agent authored. Merging
/// them would tie the request vocabulary to the config vocabulary, and the two change
/// for unrelated reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub value: String,
    /// Display text, when it differs from the value. Absent means render the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// What choosing this means.
    ///
    /// Load-bearing rather than decorative on a question the agent authored: "Tabs"
    /// and "Spaces" say far less than the sentences beside them, and that difference
    /// is most of what makes a terminal's picker readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Whether a select takes one answer or several.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectMode {
    /// The default, and the permissive direction is deliberate: reading a single
    /// answer as a list would send an agent an array where it expected a string.
    #[default]
    One,
    Many,
}

/// A free-text annotation a user may attach to an answer.
///
/// Distinct from the answer itself. `claude` carries these as
/// `annotations[question].notes` alongside the picked label, and the two are not
/// interchangeable: a note explains or qualifies a pick, while replacing the pick with
/// prose loses which option was chosen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotesSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
}

/// Which of the two input shapes a [`Field`] takes.
///
/// An enum carrying each shape's own data, rather than the flat `kind` +
/// union-of-fields the spec sketches. This is the one place this crate keeps a tagged
/// enum where the spec spells a discriminant, and the reason is that the payloads do
/// not overlap: `mode`, `choices`, `allow_other`, `other_label`, and `notes` are
/// meaningless on text, and `multiline`, `prefill`, and `placeholder` are meaningless
/// on a select. Flattening them would put seven fields on every field that must be
/// ignored on half of them, which is exactly the ambiguity that made a client sniff
/// `detail` for names it recognized.
///
/// `rename_all_fields` is required, not tidy: with only the container `rename_all` the
/// *variant names* are renamed and the fields inside them are not, so `allowOther` on
/// the wire parses as `false` while everything compiles and every non-variant field
/// works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum FieldKind {
    Select {
        #[serde(default)]
        mode: SelectMode,
        #[serde(default)]
        choices: Vec<Choice>,
        /// Whether an answer outside `choices` is allowed.
        ///
        /// `claude`'s question dialog always offers an "Other" row, and what a user
        /// types there is the answer — not a note on one of the offered options. A
        /// client must be able to send it, and the server must not refuse it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_other: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        other_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notes: Option<NotesSpec>,
    },
    Text {
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multiline: bool,
        /// Text to start from, which the user edits.
        ///
        /// Not in the spec's field listing, and kept because `pi` sends it for its
        /// `editor` dialog, where discarding it would lose the draft being revised.
        /// The spec is the lossy side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },
}

/// One input a request wants collected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub id: String,
    /// Flattened, so a field's shape-specific data sits beside `id` and `label` in one
    /// object — which is the wire form v3 produced and every adapter emits.
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(default)]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

impl Field {
    /// Whether this field accepts `value`, and why not when it does not.
    ///
    /// The error text names the field and what was wrong with the value rather than
    /// saying the field is unknown: an error that names the wrong layer sends the
    /// investigation to the wrong layer, which is exactly what made a refused config
    /// write read as a dead control.
    fn accepts(&self, value: &serde_json::Value) -> Result<(), String> {
        let id = &self.id;
        match &self.kind {
            FieldKind::Select {
                mode,
                choices,
                allow_other,
                ..
            } => {
                let offered = |candidate: &str| {
                    *allow_other
                        || choices.is_empty()
                        || choices.iter().any(|choice| choice.value == candidate)
                };

                match mode {
                    SelectMode::One => {
                        let Some(candidate) = value.as_str() else {
                            return Err(format!("'{id}' takes a single string answer"));
                        };
                        if !offered(candidate) {
                            return Err(format!("'{candidate}' is not a choice '{id}' offered"));
                        }
                    }
                    SelectMode::Many => {
                        let Some(items) = value.as_array() else {
                            return Err(format!("'{id}' takes a list of answers"));
                        };
                        for item in items {
                            let Some(candidate) = item.as_str() else {
                                return Err(format!("'{id}' takes a list of strings"));
                            };
                            if !offered(candidate) {
                                return Err(format!(
                                    "'{candidate}' is not a choice '{id}' offered"
                                ));
                            }
                        }
                    }
                }
                Ok(())
            }
            FieldKind::Text { .. } => {
                if value.is_string() {
                    Ok(())
                } else {
                    Err(format!("'{id}' takes text"))
                }
            }
        }
    }

    fn takes_notes(&self) -> bool {
        matches!(&self.kind, FieldKind::Select { notes: Some(_), .. })
    }

    /// Whether `value` counts as an answer for a `required` field.
    ///
    /// Empty is unanswered rather than answered-with-nothing: an agent that marked a
    /// field required is blocked until it has something, and forwarding `""` would
    /// report an answer it never received.
    fn is_answered(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => !text.trim().is_empty(),
            serde_json::Value::Array(items) => !items.is_empty(),
            serde_json::Value::Null => false,
            _ => true,
        }
    }
}

/// The input a request wants collected, declared by the agent that raised it.
///
/// **A form is declared, never inferred.** Before this existed a client guessed what
/// to collect by sniffing `detail` for field names it recognized (`command`,
/// `file_path`, `url`). That is right for an approval, whose subject *is* one proposed
/// tool input, and unfixable for anything else: a question's detail carries none of
/// those, so a [`RequestKind::Choice`] rendered as bare buttons with nowhere to type
/// and an [`RequestKind::Elicitation`] — whose entire purpose is free text — rendered
/// as a read-only dump of its own placeholder. Only the agent knows what it is asking
/// for.
///
/// **N fields, not one question.** *Measured against `claude` 2.1.238:* a reply
/// carrying every question's answer is delivered to the model as *"Your questions have
/// been answered … you can now continue with these answers in mind"*, while a reply
/// answering one of two arrives as the weaker *"The user answered …"*. Both look like
/// success from outside.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Form {
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// A client's answers to a [`Form`], as they travel on a resolution's `input`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormAnswer {
    #[serde(default)]
    pub values: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub notes: HashMap<String, String>,
}

impl Form {
    pub fn field(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.id == id)
    }

    /// Whether `input` is a well-formed answer to this form.
    ///
    /// Validated by the server for the same reason a config value is: the client
    /// renders whatever an agent it has never seen reported, so the server is what
    /// keeps a malformed answer from reaching a vendor CLI whose handling of one is
    /// undefined. Checked *before* the request is claimed, so a rejected answer does
    /// not consume the single-winner update and leave the agent unanswerable.
    pub fn validate(&self, input: Option<&serde_json::Value>) -> Result<(), String> {
        let Some(input) = input else {
            // No input at all is only an answer if nothing was required. A form whose
            // fields are all optional is legitimately answerable by submitting it
            // empty.
            return match self.fields.iter().find(|field| field.required) {
                Some(field) => Err(format!("'{}' is required", field.id)),
                None => Ok(()),
            };
        };

        let answer: FormAnswer = serde_json::from_value(input.clone())
            .map_err(|_| "the answer is not shaped like a form answer".to_string())?;

        for (id, value) in &answer.values {
            let Some(field) = self.field(id) else {
                return Err(format!("'{id}' is not a field this request asked for"));
            };
            field.accepts(value)?;
        }

        for id in answer.notes.keys() {
            match self.field(id) {
                Some(field) if field.takes_notes() => {}
                Some(_) => return Err(format!("'{id}' does not take notes")),
                None => return Err(format!("'{id}' is not a field this request asked for")),
            }
        }

        for field in self.fields.iter().filter(|field| field.required) {
            let answered = answer.values.get(&field.id).is_some_and(Field::is_answered);
            if !answered {
                return Err(format!("'{}' is required", field.id));
            }
        }

        Ok(())
    }
}

/// An option offered on a request. **A verb, not data.**
///
/// `Choice` previously overloaded `options` as both the answer and the submit button,
/// which is why picking had to submit immediately and why a note had nowhere to live.
/// Data belongs in the [`Form`]. When a surface cannot express something, check whether
/// one field is carrying two meanings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOption {
    pub id: String,
    pub label: String,
    /// Whether choosing this is a lasting decision rather than a one-off, so a UI can
    /// mark "allow always" differently from "allow once".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub persistent: bool,
    /// What this option means, when the agent explained it.
    ///
    /// Load-bearing on a `Choice`, where the agent authored the options and the
    /// description is most of what makes them readable. Omitting the field from the
    /// type did not merely fail to use it: the options are re-serialized from this
    /// struct on their way to the journal, so a description the agent sent was
    /// silently discarded before any client could render it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A question the agent needs a human to answer.
///
/// **Durable state, not UI state.** It is journaled, survives a client disconnect, and
/// is re-delivered on reconnect — otherwise closing a laptop lid strands an agent
/// forever. If the link dies with requests outstanding they are journaled as
/// `abandoned`, so the timeline explains why the turn stopped rather than showing an
/// unexplained gap.
///
/// **First resolution wins**; a second is refused, naming the resolver, so two people
/// answering at once is a defined outcome rather than a race.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// The agent's own id for this ask, which a resolution must echo back.
    ///
    /// Scoped to one session's adapter. The server mints its own id for the journal
    /// row a client addresses, because a client-supplied adapter id is a way to answer
    /// a request that was never raised (`models/request.rs`).
    pub id: String,
    pub kind: RequestKind,
    /// The agent's own id for the tool call this concerns, when there is one.
    ///
    /// Not in the spec's struct and kept, because `PendingRequest` consumes it: it is
    /// what pairs an approval to the `tool_call` event already on the timeline, so
    /// dropping it would leave a dialog floating beside the call it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The tool's name, likewise consumed by `PendingRequest` and likewise absent from
    /// the spec's struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default)]
    pub title: String,
    /// The vendor's own payload, for display. For an approval this is the proposed
    /// tool input, which is also what an editing client sends back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<RequestOption>,
    /// The input this request wants collected, when it declares one.
    ///
    /// `None` is an approval: its subject is one proposed tool input, which a client
    /// renders from `detail`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Form>,
    /// Which option applies if the user declines to choose.
    ///
    /// **Deliberately not auto-applied by the server** when `expires_at_ms` lapses:
    /// that would remove the user from the decision this channel exists to give them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<Risk>,
    /// Whether a lasting grant may be offered at all.
    ///
    /// `false` means a client must omit the "always allow" affordance **entirely**,
    /// not merely de-emphasize it: `claude` sets it on asks where accepting always
    /// would write a permission rule *broader than the ask itself*, so offering it
    /// turns a narrow yes into a wide one the user never read. Absent means allowed,
    /// which keeps a minimal adapter correct.
    #[serde(default = "default_true")]
    pub allow_persistent: bool,
    /// When the agent stops waiting, if it will.
    ///
    /// Advisory and never acted on by the server. `pi` auto-resolves a dialog when its
    /// own timeout lapses, so a client with no deadline to show is racing a clock it
    /// cannot see and the user experiences a dialog that answers itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

fn default_true() -> bool {
    true
}

/// A human's answer to a [`Request`].
///
/// Named rather than three loose arguments because these three travel together
/// everywhere — client to server, server to adapter, and into the journal — and a
/// positional `(&str, &str, Option<Value>)` at each hop is three chances to swap the
/// first two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    /// The request's own id, as the agent gave it.
    pub request_id: String,
    /// Which [`RequestOption`] was chosen.
    pub option: String,
    /// The collected input, which **may be an edited version of what the agent
    /// proposed**. That is how "allow, but change the command" works — a real terminal
    /// affordance a yes/no dialog drops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every part of a field has to survive the parse, and the wire form has to be
    /// the one the adapters already emit: `type` beside `id`, in one flat object.
    ///
    /// `allowOther` is the one worth naming: it lives inside an enum variant, where
    /// `rename_all` alone does not reach — it would read as `false` while everything
    /// compiled, silently refusing the answer a user typed into an "Other" row.
    #[test]
    fn a_form_parses_every_part_of_its_fields() {
        let form: Form = serde_json::from_str(
            r#"{"fields":[
              {"type":"select","id":"q:0","label":"Indentation?","mode":"one",
               "choices":[{"value":"Tabs","description":"Tab characters"},
                          {"value":"Spaces"}],
               "allowOther":true,"notes":{"label":"Notes"}},
              {"type":"select","id":"q:1","label":"Linters?","mode":"many",
               "choices":[{"value":"ESLint"},{"value":"Clippy"}],"required":true},
              {"type":"text","id":"why","label":"Why?","multiline":true,
               "prefill":"because"}]}"#,
        )
        .unwrap();
        assert_eq!(form.fields.len(), 3);

        let FieldKind::Select {
            mode,
            allow_other,
            choices,
            notes,
            ..
        } = &form.fields[0].kind
        else {
            panic!("the first field is a select");
        };
        assert_eq!(*mode, SelectMode::One);
        assert!(allow_other, "allowOther must survive the variant rename");
        assert_eq!(choices[0].description.as_deref(), Some("Tab characters"));
        assert!(choices[1].description.is_none());
        assert!(notes.is_some());

        assert!(form.fields[1].required);
        let FieldKind::Select { mode, .. } = &form.fields[1].kind else {
            panic!("the second field is a select");
        };
        assert_eq!(*mode, SelectMode::Many);

        let FieldKind::Text {
            multiline, prefill, ..
        } = &form.fields[2].kind
        else {
            panic!("the third field is text");
        };
        assert!(multiline);
        assert_eq!(prefill.as_deref(), Some("because"));
    }

    /// The flatten must keep the discriminant a sibling of `id`, not nest it under a
    /// `kind` object. Serializing is the half a parse test cannot observe.
    #[test]
    fn a_field_serializes_flat_with_its_type_beside_its_id() {
        let field = Field {
            id: "why".into(),
            kind: FieldKind::Text {
                multiline: true,
                prefill: None,
                placeholder: None,
            },
            label: "Why?".into(),
            description: None,
            required: true,
        };
        let wire = serde_json::to_value(&field).unwrap();
        assert_eq!(wire["type"], "text");
        assert_eq!(wire["id"], "why");
        assert_eq!(wire["multiline"], true);
        assert!(wire.get("kind").is_none(), "no nested wrapper object");
        assert_eq!(serde_json::from_value::<Field>(wire).unwrap(), field);
    }

    /// A field that omits `mode` is single-select. The default matters in the safe
    /// direction: reading one answer as a list would hand an agent an array where it
    /// expected a string.
    #[test]
    fn a_select_field_without_a_mode_takes_one_answer() {
        let field: Field =
            serde_json::from_str(r#"{"type":"select","id":"q","choices":[{"value":"a"}]}"#)
                .unwrap();
        let FieldKind::Select { mode, .. } = &field.kind else {
            panic!("must parse as a select");
        };
        assert_eq!(*mode, SelectMode::One);
    }

    fn two_field_form() -> Form {
        serde_json::from_str(
            r#"{"fields":[
                 {"type":"select","id":"q:0","mode":"one",
                  "choices":[{"value":"Tabs"},{"value":"Spaces"}],
                  "notes":{"label":"Notes"}},
                 {"type":"select","id":"q:1","mode":"many",
                  "choices":[{"value":"ESLint"},{"value":"Clippy"}]}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_well_formed_answer_to_a_form_is_accepted() {
        let form = two_field_form();
        let answer = json!({
            "values": {"q:0": "Tabs", "q:1": ["ESLint", "Clippy"]},
            "notes": {"q:0": "only for generated files"},
        });
        assert!(form.validate(Some(&answer)).is_ok());
    }

    /// The server refuses what the client should not have sent, for the same reason it
    /// refuses an option the agent never offered: an unrecognized value reaching a
    /// vendor CLI does something undefined, and the layer that is not at fault is the
    /// one that gets debugged.
    #[test]
    fn an_answer_the_form_did_not_ask_for_is_refused() {
        let form = two_field_form();

        assert!(form
            .validate(Some(&json!({"values": {"nope": "Tabs"}})))
            .is_err());

        // A single-select handed a list, and a multi-select handed a bare string.
        // Either would reach the adapter as the wrong JSON type.
        assert!(form
            .validate(Some(&json!({"values": {"q:0": ["Tabs"]}})))
            .is_err());
        assert!(form
            .validate(Some(&json!({"values": {"q:1": "ESLint"}})))
            .is_err());

        // A value outside `choices` on a field that did not permit one.
        assert!(form
            .validate(Some(&json!({"values": {"q:0": "Semicolons"}})))
            .is_err());

        // Notes on a field that never offered them.
        assert!(form
            .validate(Some(&json!({"values": {}, "notes": {"q:1": "hmm"}})))
            .is_err());
    }

    /// `allowOther` is what the terminal's "Other" row sends, and it is an answer
    /// rather than a note — so a value outside `choices` has to be accepted where the
    /// agent said it may be.
    #[test]
    fn a_field_allowing_other_accepts_a_value_it_never_offered() {
        let form: Form = serde_json::from_str(
            r#"{"fields":[{"type":"select","id":"q","mode":"one",
                 "choices":[{"value":"Tabs"}],"allowOther":true}]}"#,
        )
        .unwrap();
        assert!(form
            .validate(Some(&json!({"values": {"q": "Tabs in Makefiles only"}})))
            .is_ok());
    }

    /// Empty is unanswered, not answered-with-nothing.
    #[test]
    fn a_required_field_must_actually_be_answered() {
        let form: Form =
            serde_json::from_str(r#"{"fields":[{"type":"text","id":"why","required":true}]}"#)
                .unwrap();

        assert!(form.validate(None).is_err());
        assert!(form.validate(Some(&json!({"values": {}}))).is_err());
        assert!(form
            .validate(Some(&json!({"values": {"why": "   "}})))
            .is_err());
        assert!(form
            .validate(Some(&json!({"values": {"why": "because"}})))
            .is_ok());
    }

    /// A form whose fields are all optional is legitimately answerable by submitting
    /// it empty, so no input at all cannot be a blanket refusal.
    #[test]
    fn an_all_optional_form_accepts_an_empty_answer() {
        assert!(two_field_form().validate(None).is_ok());
    }

    #[test]
    fn an_approval_parses_with_its_options_and_proposed_input() {
        let request: Request = serde_json::from_str(
            r#"{"id":"r4","kind":"approval","toolUseId":"toolu_01",
                "tool":"Bash","title":"Run a shell command",
                "detail":{"command":"rm -rf build"},
                "options":[{"id":"allow","label":"Allow once"},
                           {"id":"allow_always","label":"Always","persistent":true}],
                "default":"deny","risk":"destructive"}"#,
        )
        .unwrap();
        assert_eq!(request.kind, RequestKind::Approval);
        assert_eq!(request.tool.as_deref(), Some("Bash"));
        assert_eq!(request.tool_use_id.as_deref(), Some("toolu_01"));
        assert_eq!(request.detail.unwrap()["command"], "rm -rf build");
        assert_eq!(request.options.len(), 2);
        assert!(request.options[1].persistent);
        assert_eq!(request.risk, Some(Risk::Destructive));
        // Absent means an approval renders `detail` and edits what it recognizes.
        assert!(request.form.is_none());
    }

    /// Absent means allowed, so a minimal adapter that never sets it stays correct; an
    /// explicit `false` is what forbids the affordance.
    #[test]
    fn an_approval_may_forbid_offering_a_lasting_grant() {
        let permissive: Request =
            serde_json::from_str(r#"{"id":"r","kind":"approval","title":"x"}"#).unwrap();
        assert!(permissive.allow_persistent);

        let narrow: Request = serde_json::from_str(
            r#"{"id":"r","kind":"approval","title":"x","allowPersistent":false}"#,
        )
        .unwrap();
        assert!(!narrow.allow_persistent);
    }

    /// The options reach the journal by being re-serialized from this struct, so a
    /// field the type drops is a field the client never sees. That is exactly how the
    /// descriptions on an `AskUserQuestion` were being lost while the adapter sent
    /// them correctly.
    #[test]
    fn an_option_keeps_the_explanation_the_agent_wrote_for_it() {
        let request: Request = serde_json::from_str(
            r#"{"id":"r7","kind":"choice","title":"Indentation?",
                "options":[{"id":"option:0","label":"Tabs","description":"Use tab characters"},
                           {"id":"option:1","label":"Spaces"}]}"#,
        )
        .unwrap();
        let wire = serde_json::to_value(&request.options).unwrap();
        assert_eq!(wire[0]["description"], "Use tab characters");
        assert!(
            wire[1].get("description").is_none(),
            "an absent one stays absent"
        );
    }
}
