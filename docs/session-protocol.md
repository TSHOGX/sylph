# Session Protocol

This document owns vendor process controls, requests and capabilities. The trace model
is defined by [unified-trace-foundation.md](unified-trace-foundation.md), and exact capture,
receipts and pure mapping by [unified-trace-storage.md](unified-trace-storage.md).

Vendor runtime code is linked into the envagent and holds its process. Server mappers
read retained observations independently of that process or machine. The two communicate
through the typed capture and operational contracts over `sylph.env.v1`.

**Read this and the dedicated vendor mapping before writing an adapter.** Control and
capability rules below are contracts an adapter must satisfy, and most of them are rules whose violation is *silent* — a vendor
reports success, the turn completes, and the record is wrong. They are stated as
requirements rather than as advice for that reason.

Two properties of the design carry most of the weight, and both are stated once here
because everything else assumes them:

- **A typed adapter makes an adapter/server mismatch a compile error**, so the
  remaining risk is entirely at the vendor boundary — reading a vendor's JSON. That
  is the most-repeated defect class in this repo, and `docs/agents.md` § Reading a
  vendor payload owns it.
- **Availability is "is the vendor CLI on this machine's `PATH`"**, one question with
  one answer and an error message that can name it. There is no discovery handshake
  and no adapter directory.

## The unit is a session, not a turn

An adapter holds a session for its lifetime: `run` once, then input through `send`.
Its operational message stream reports native activity and settlement. The trace mapper
reads execution outcomes from the exact retained vendor frames and source records.

A process per *turn* cannot work, for two independent reasons. Everything stateful a
vendor CLI knows — loaded skills, permission grants, a warmed context, an MCP
connection — would be reachable only by restarting and re-resolving it. And it makes
"the turn ended" and "the process exited" the same event, leaving an agent that speaks
after its turn (§ An agent may start a turn) nowhere to speak from.

## The acquisition and control contract

`Adapter::run(RunSpec, Arc<dyn RawCaptureSink>)` returns a held `Session`. Its
`Session::messages()` yields one `ControlStream` carrying `Request`, `Capabilities`, and
`ControlNotice::{Identity, Active, Settled}`. The raw sink commits exact stdin, stdout,
stderr and measured process lifecycle bytes before runtime decoding or reply routing.

`Adapter::native_sources()` returns source locators, formats and independently evidenced
owner hints; `enumerate()` returns cheap catalogue metadata. The envagent's shared durable
registry acquires and follows files, snapshots and consistent native SQLite reads.
Adoption, following and re-reading all use that registry. Server replay uses the archive
without a vendor process or filesystem call.

### What the adapter owns, and what it must not

The adapter owns vendor argv, request/reply protocol, native locators, capability discovery
and control validation. A locator or requested native id is not identity proof. The adapter
returns retained observation references for the id the vendor actually reports.

The server decides identity injection. The adapter cannot invent semantic activations,
promote tool calls into agents, retry an ambiguous input, or report an unanswered vendor
request as rejected. Typed trace facts are produced only by pure server mappers.

## Opening a session

`RunSpec` carries what a session is opened with:

| Field | Meaning |
| --- | --- |
| `workdir` | the agent's `current_dir`, already contained by the envagent |
| `native_id` | `Dictate` a new id, `Resume` an existing id, or `New` and let the vendor assign one |
| `inject_identity` | whether to append `SOUL.md`; the server decides, not the adapter |
| `flags` | spawn-time options: model, effort, and whatever the agent declared |
| `permission_mode` | initial mode, if the vendor takes one at launch |

**Dictate the id where the vendor allows it.** `claude` and `pi` accept a
`--session-id`; `codex` assigns its own and reports it. Every path still requires retained
vendor confirmation. A requested id cannot establish that the vendor actually used it.

**Which of the two applies is `AgentInfo::dictates_native_id`, read from a probe** — never
a list of agent names held beside the adapters, which would diverge from them the moment
either grew a case. **The envagent mints the dictated id; no adapter ever mints one.** The
envagent is the process that holds the adapter and builds the `RunSpec`, so it is the only
place that can read the fact and act on it. An adapter minting its own would write an id
that looks like the vendor's and is Sylph's — the synthesized-identity failure `NativeId`
forbids, and the one `pi`'s v1 committed with `uuid4()`. That is why `claude` and `pi`
refuse `New` instead of inventing an id, and why `New` reaching either of them is a bug in
the envagent rather than something for the adapter to paper over.

A minted id is **not a second id space** (`docs/architecture.md` § Identity). It is handed
to the vendor on `--session-id`, and the vendor adopts it as its own native id, so the
value Sylph stores and the value the vendor's store holds are one value.

A prepared link can have no native id while idle. Capture starts under a server-issued
inventory authorization before native identity exists. The session returns a non-empty
id and `identity_evidence()` when the vendor establishes it; Claude may need the first
input, while Codex can establish it during `thread/start`.

The server binds a launch only from admitted identity evidence. Captures, accepted input
and command target keep their original identities and authorization epochs. A lost reply
or unresolved identity leaves explicit ambiguity; it cannot create a guessed session or
replace the accepted input with a vendor-transformed representation.

### A launch flag gating a runtime-mutable value must be set unconditionally

**Register such a channel unconditionally.** A mode that decides alone simply never
consults it, so there is no cost to registering it always — and a real cost to not.

`claude`'s `--permission-prompt-tool` is the case that defines the rule: it can only
be asked for on the command line, while the mode it serves changes at runtime. Gate it
on the *launch* mode and a session that starts in a bypassing mode can never return to
an interactive one, because the approval channel it would need was never registered.
Any spawn-time flag serving a runtime-mutable value has this shape.

## Capability facets

A capability update names the facets it speaks about. **A facet that is not
mentioned is not changed.**

Facets: `commands`, `subagents`, `config`, `permissionMode`, `contextWindow`.

The hazard this closes is specific. `claude` emits `system/init` once **per turn**,
not once per process, so an adapter answering it with a whole-surface payload that
happens to carry only the permission mode erases every slash command the project
declared — and the symptom surfaces two layers away, as a composer menu that will not
open.

**Naming the facet makes silence mean silence.** Merging partials would fix that one
instance at the same cost and leave the hazard in place: a message about one part of
the surface would still be *able* to speak for all of it, and the next adapter would
rediscover it. Prefer a shape that cannot express the mistake over a rule asking
everyone to avoid it.

Capability changes are **pushed, not polled**. A skill edited on disk or a plugin
loading changes what the composer should offer, and a list a client refreshes on its
own schedule is wrong for however long that schedule is.

## Session config

Everything adjustable on a live session, in one shape (ACP's `SessionConfigOption`,
adopted rather than invented because it already covers `claude`'s model / effort /
permission mode / output style, `pi`'s model / thinking level / queue modes, and
`codex`'s approval policy):

```rust
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    pub category: Option<String>,       // advisory grouping; a client groups without knowing the vocabulary
    pub kind: ConfigKind,       // Select | Boolean
    pub current_value: Option<serde_json::Value>,
    pub options: Vec<ConfigChoice>,
}
```

Setting one option returns the **refreshed** array, not an acknowledgement, because
setting one can change another's choices. This is not defensive: `pi`'s thinking levels
depend on the selected model, and `claude` reports supported effort levels per model.
A session-global enumeration of effort is therefore wrong for both.

`Select` and `Boolean` cover every knob the three supported agents expose. A numeric
or free-text kind is a later addition, not a placeholder now.

### A config value must be unique, and must not be delimited with `/`

**A value must identify exactly one choice.** `checked` is computed as a filter —
every producer writes `value == current_value` — so two choices sharing a value put a
checkmark on both and make neither identifiably the real one. A bare id also makes one
of the pair *unreachable*, since a by-id lookup can only answer with one of them.

This is not hypothetical: on a real `pi` install some model ids are offered by more
than one provider, and some display names are duplicated outright. So **a model's
identity is `(provider, id)`, never the id alone.**

Three layers enforce it, because a client renders whatever an agent it has never seen
reports. The adapter issues
provider-qualified values, the list component enforces one current row per
single-select group, and the server validates a value against what was enumerated.

**The delimiter is a space, not `/`.** Most of `pi`'s model ids already contain a
slash (`anthropic/claude-opus-5`, `stealth/ox-alpha`), so splitting on the first or
last one mis-parses the majority of a real catalogue. No provider and no id contains a
space. A `ConfigChoice.value` is **opaque to the client by contract** — a token to echo
back, never to parse — which is what lets this encoding change without a client
change.

### A refused set: two codes, and why the difference is load-bearing

| Code | Means | What a client may do |
| --- | --- | --- |
| `UnsupportedField` | this agent has no such option | disable the control |
| `InvalidValue` | the option is real; *this value* was rejected | report it, leave the control usable |

The server maps these to distinct statuses (`InvalidValue` → 400, `UnsupportedField`
→ 409). Flattening them tells a user their agent cannot do something it does perfectly
well, and invites disabling a whole picker because one of its values was refused for a
reason the session could change — `claude` refuses `bypassPermissions` unless launched
with a flag marking that mode available, which is a *precondition*, not a missing
capability.

**The code travels, not the message.** The text belongs to the adapter and may be
reworded; the code is declared contract. Never recover a code by parsing a message.

### Three rules about reading a vendor's answer

Each of these is a way a write can fail to happen while every observable layer reports
success. `docs/agents.md` carries the per-vendor instances.

**A vendor's `success` is not a promise it did what you asked.** Vendors variously
refuse a value outright, accept and ignore it, or silently substitute the nearest one
they support — and only the first is visible to a caller checking for an error. So an
adapter records `current_value` from the vendor's **confirmation**, never from the
request, and offers only values it has watched the vendor apply as given.

**A success and a refusal are different shapes, so a reader for one swallows the
other.** Where a refusal carries a discriminant and *no* payload member, code reading
the payload directly maps every refusal onto the same empty value a successful
no-payload control returns — so a refused write is recorded as applied and rendered as
current. Read the discriminant before the payload: **`{}` must never be reachable from
two opposite outcomes.**

**Read current values from the vendor, not from what you last set.** An adapter
reporting `current_value` from its own memory reports nothing on any path that did not
write that memory — a re-attach, a set reply — so a session plainly holding a model
reports none. The vendor is also the only thing that knows whether a set was
*honoured*, and a session may be steered by the agent's own TUI besides.

## Two kinds of command

A slash command is either a **turn** (`/verify`, `/review` — it does work and produces
a transcript) or a **control** (`/model`, `/effort` — it changes state and produces
nothing). Sent as prompt text, a control command returns a paragraph *describing* the
options, which is the CLI behaving correctly: a headless client has no picker to open.

So a control command is never translated to prompt text. It is **typed state** a client
renders as a picker, and choosing sends a typed set. Three protocols reached this split
independently, which is what makes it a generalization rather than a guess: ACP has
`AvailableCommand` beside `SessionConfigOption`, `codex`'s app-server has no command
concept at all but does have `model/list` and `config/value/write`, and `claude`
exposes both.

A command declared by the agent carries whether it takes input; the composer's menu
renders from the `commands` facet, which is session state and not agent state — a
project's real command list depends on files the CLI reads at startup.

## Requests: the agent asks, a human answers

The agent asks a question mid-turn and waits; a human answers from any attached
client. The adapter raises the native ask, the envagent scopes its public id to the held
process, and the server stores and publishes its operational state.

```rust
pub struct Request {
    pub id: String,
    pub kind: RequestKind,          // Approval | Dialog | Elicitation | Choice
    pub title: String,
    pub detail: serde_json::Value,  // the vendor's own payload, for display
    pub form: Option<Form>,         // what input to collect (§ Request forms)
    pub options: Vec<RequestOption>,
    pub default: Option<String>,
    pub risk: Option<Risk>,
    pub allow_persistent: bool,
    pub expires_at_ms: Option<i64>,
}
```

A resolution carries the chosen option and optionally `input` — which may be an
**edited** version of what the agent proposed. That is how "allow, but change the
command" works, a real terminal affordance a yes/no dialog drops.

**A pending request is durable state.** Reconnect of the same held process repeats the
same public request id. A successor process receives a new scope even when its native
request counter restarts. A resolution must name a pending public id; the envagent
translates it back to the native id immediately before vendor forwarding.

Claiming an answer and admitting its immutable command occur atomically. A duplicate
request notification cannot revive a resolved or ambiguously dispatched answer. The
server records waiting, resolving and resolved operational states separately from the
vendor's retained request/result evidence.

**`allow_persistent: false` means a client must omit the "always allow" affordance
entirely**, not merely discourage it. `claude` sets it on asks where accepting always
would write a permission rule *broader than the ask itself*, so offering it converts
a narrow yes into a wide one the user did not read. Absent means allowed, which keeps
a minimal adapter correct.

**`expires_at_ms` is advisory and present when the agent will stop waiting.** `pi`
auto-resolves a dialog when its own timeout lapses, so a client with no deadline to
show is racing a clock it cannot see and the user experiences a dialog that answers
itself. The server never auto-applies a default when it lapses — that would remove
the user from the decision this channel exists to give them.

**First resolution wins.** A concurrent answer cannot dispatch another vendor decision
for the same public request. A transport failure after dispatch remains unknown.

### Announce every departure, not only the arrival

**A request must be published on every transition out of `pending`** — resolved,
expired, or abandoned by a dying link — and not only when it is raised. Announce
arrival alone and a client's set of outstanding questions can only grow: it is correct
on the way in and never shrinks, so a question answered on a phone stays on the desktop
until someone reloads. A periodic refetch does not save this, because a backgrounded tab
may not be polling at all.

Three properties make that hold rather than patching one path:

- **One publisher that every transition calls.** Three call sites each remembering to
  announce is three chances to forget.
- **The payload is the row, not the transition.** A client reconciles against a state
  rather than replaying a vocabulary of verbs, so a status added later needs no client
  change.
- **Published before the forward to the adapter.** The claim is what makes the answer
  final, so a request answered against a *dead* link must still retire everywhere.

Testing this by re-reading the request list proves nothing: the row is correct either
way, and only the notification is missing. Assert on the session stream, and end-to-end
use two browser **contexts** rather than two tabs — two tabs share a cookie jar and can
pass while a genuinely separate client still hangs.

### Request forms

An approval asks one thing about one proposed input. A *question* is not that: the
agent authored the choices, there may be several questions in one ask, and the answer
can legitimately be something the agent did not offer. So a request declares the input
it wants:

```rust
pub struct Form { pub fields: Vec<Field> }

pub struct Field {
    pub id: String,
    pub kind: FieldKind,            // Select { mode: One | Many } | Text { multiline: bool }
    pub label: String,
    pub choices: Vec<Choice>,       // each carrying the agent's own description
    pub required: bool,
    pub allow_other: bool,          // a value outside `choices` is a real answer
    pub other_label: Option<String>,
    pub notes: Option<NotesSpec>,   // annotation attached to the pick, not replacing it
}
```

**A form is declared, never inferred.** A client cannot decide what to collect by
inspecting `detail` for field names it recognizes: that works only for an approval,
whose subject *is* one proposed tool input, and fails for everything else — a question's
detail carries no such fields, so it renders as bare buttons with nowhere to type, and an
elicitation whose entire purpose is free text renders as a read-only dump of its own
placeholder. **Only the agent knows what it is asking for**, so the agent declares it.

**`options` are verbs; fields carry data.** Overloading `options` as both the answer and
the submit button is what forces picking to submit immediately and leaves a note nowhere
to live. When a surface cannot express something, check whether one field is carrying two
meanings.

**The server validates `input` against the stored form** before claiming the request:
an unknown field id, a `One` field given an array, a value outside `choices` where
`allow_other` is not set, or a missing `required` field is refused. Same reason as
config values — the client renders whatever an agent it has never seen reported, so
the server is what keeps a malformed answer from reaching a vendor CLI that will do
something undefined with it.

**Answer every question a vendor asked, not the first.** A vendor ask may carry
several questions, and a partial reply degrades by *wording* rather than by error — the
model is told the user answered something, with no indication that the rest went
unanswered. Both look like success from outside, which is why one form carries N fields
rather than one question.

**Writing an answer into a field the vendor does not read is silent.** The vendor builds
its tool result from the key it expects; write a plausible neighbouring key and it finds
nothing and tells the model the user did not answer — so a correct pick, correctly
rendered and correctly transported, presents as a UI that lost the click. Two rules
follow: confirm the key exists in the vendor's own binary or schema before writing it,
and assert on **the tool result the model reads**, because "the panel closed" and "the
answer arrived" are different claims and only the second is the feature.

## Trace evidence

Capture occurrence, semantic claim, resolved object and query change have distinct
identities. The typed fact families and deterministic reduction rules live in
[unified-trace-foundation.md](unified-trace-foundation.md#evidence-claims-and-deterministic-reduction).
A runtime control message does not substitute for execution evidence.

### Time axes are independent

Source order, receipt order, causal order and selected history remain separate. Clock
units, domains and uncertainty follow
[unified-trace-foundation.md](unified-trace-foundation.md#ordering-clocks-and-measurements).

### `name` is absent when unknown

Names are optional evidenced metadata. Reader fallback labels never enter durable
authored content or supply child identity. Native identity and occurrence namespaces
are defined by the dedicated vendor mappings.

### Outcomes, and what a turn may close

The [independent lifecycle contract](unified-trace-foundation.md#independent-lifecycle-claims)
separates vendor outcome, control acceptance and observation loss. A process exit can
report only its evidenced scope. A parent ending cannot close independent children or
resources; later retained evidence may still arrive.

## An agent may start a turn

An Activation may follow a human delivery, another agent, a goal continuation or a
native/harness trigger. It is created from an observed execution boundary; input acceptance
alone cannot create one. Missing association stays unknown. Control waiter state and
trace ownership are independent under
[unified-trace-foundation.md](unified-trace-foundation.md#messages-deliveries-and-model-visibility).

## Failure

An adapter reports what it knows and omits what it does not:

- **Never report a refusal as a success** (§ A refused set).
- **Never synthesize a value the vendor did not give.** A synthesized id, name, or
  fingerprint is a lie the journal makes permanent.
- **An error must name the layer that is actually at fault.** Answering a malformed
  *value* with "no such option" says the option does not exist when the option was fine,
  which sends the reader to the wrong layer entirely — so an adapter separates
  unknown-option from invalid-value. The same applies one layer out: a refused write with
  no error path in the client presents as a dead control, and the layer that is *not* at
  fault is the one that gets debugged.
- **A turn's outcome is a vendor fact, so read it rather than writing a literal.** An
  adapter that hardcodes a clean stop reason turns every vendor-side failure into a turn
  that completed and simply said nothing — the hardest outcome to debug, because silence
  looks identical whether the model errored, the agent chose not to answer, or the
  adapter dropped the stream. Any path that can end a turn without content must say why.

## Environment wire semantics

The machine boundary is `sylph.env.v1`, implemented by the serde-only values in
`crates/protocol/src/env.rs` and the length-delimited runtime in
`crates/envagent/src/transport.rs`.

A raw capture carries its producer observation identity, source position and fixed
ingestion authorization epoch. Begin/chunk/commit upload receives a terminal retained or
authorized-deletion receipt before the spool releases bytes. The server's evidence cut
and trace publication watermark are separate from source occurrence order.

Hello/welcome negotiates version and connection epoch, and reports agent inventory and
health. Capture replies are scoped to that epoch. Commands carry immutable request ids
and `replayable` or `at_most_once` policy; outcomes are `accepted`, `rejected` or `unknown`.
An ambiguous at-most-once command is not repeated after reconnect or process restart.

Current capability snapshots, pending requests and control leases replay independently
of semantic mapping. [environments.md](environments.md#message-classes) owns the wire
message list and runtime scheduling rules.
