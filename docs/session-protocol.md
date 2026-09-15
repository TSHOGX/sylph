# Session Protocol

**Authority split:** this document owns vendor process/control behavior, requests and
capabilities. [unified-trace-foundation.md](unified-trace-foundation.md) replaces the
former event/block/span semantics, and [unified-trace-storage.md](unified-trace-storage.md)
defines capture and pure mapping. Superseded trace headings below remain source-comment
navigation points; they do not authorize an alternate trace model.

The process/control contract between the server and an agent. Capture can observe live
I/O, native history, or both; one pure semantic mapping interprets that evidence.

Vendor runtime code is linked into the envagent and talks to the vendor process. The
pure mapping module is callable by the server over retained observations. Capture
envelopes cross `sylph.env.v1` (`docs/environments.md`); schema/build identity is explicit
at that boundary and at semantic generation publication.

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

An adapter holds a session for its lifetime: `open` once, then a turn per `send`, and
the outcome arrives as an event rather than as an exit code.

A process per *turn* cannot work, for two independent reasons. Everything stateful a
vendor CLI knows — loaded skills, permission grants, a warmed context, an MCP
connection — would be reachable only by restarting and re-resolving it. And it makes
"the turn ended" and "the process exited" the same event, leaving an agent that speaks
after its turn (§ An agent may start a turn) nowhere to speak from.

## An adapter is a trait, with three modes

An adapter package contains envagent capture/control and a pure server semantic mapper.
Run captures a controlled live process, adopt captures an available native inventory,
and watch follows source changes. They all feed the same evidence contract and mapper;
none fabricates live events from native messages to satisfy the former block API.

[unified-trace-storage.md](unified-trace-storage.md#capture-contract) owns the replacement
interface. Existing Rust trait signatures are implementation context until that interface
lands; there is no compatibility adapter wrapping the obsolete vocabulary.

### What the adapter owns, and what it must not

**Owns:** the vendor CLI's command line and native store format, translation in both
directions, the vendor's own ids, and the decision to omit rather than guess.

**Must not:** decide identity injection (the server passes it), invent an event kind
the vocabulary does not have, retry a turn on its own, or report a refusal as a
success. The last of those is the easiest to do by accident — see § A refused set.

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
`--session-id`; `codex` assigns its own and reports it. Dictating means the identity is
known before the first event, so nothing has to correlate afterwards.

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

An adapter that cannot dictate reports the vendor's id when it starts the first turn,
and the server keys the durable session on what came back. A prepared link may report
no id at all while it is idle; that is valid because it has no durable record to resume.
The adapter must return a non-empty id from the first-turn promotion operation. If it
cannot, promotion fails and the prepared link is closed without writing a session or
turn row.

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
    pub category: String,       // advisory grouping; a client groups without knowing the vocabulary
    pub kind: ConfigKind,       // Select | Boolean
    pub current_value: Option<String>,
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
reports — including agents written by other people. The adapter issues
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
client. A request is raised by the adapter, journaled by the server, published to every
attached client, and blocks nothing else.

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

**A pending request is durable state, not UI state.** It is journaled, survives a
client disconnect, and is re-delivered on reconnect — otherwise closing a laptop lid
strands an agent forever. If the link dies with requests outstanding they are
journaled as `abandoned`, so the timeline explains why the turn stopped rather than
showing an unexplained gap.

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

**First resolution wins**; a second is refused, naming the resolver, so two people
answering at once is a defined outcome rather than a race.

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

## Events

The normalized vocabulary is the typed fact families in
[unified-trace-foundation.md](unified-trace-foundation.md#evidence-claims-and-deterministic-reduction).
Capture occurrence, semantic claim, resolved object and query change have distinct
identities. A kind/phase/stream/span envelope is not the replacement object model.

## Delegated trajectories

Delegation connects independently advancing AgentThreads through invocation, spawn,
delivery and activation evidence. Operations and Resources remain independently typed.
The governing rules are
[unified-trace-foundation.md](unified-trace-foundation.md#ownership-topology-interaction-and-context).

### Time axes are independent

See [unified-trace-foundation.md](unified-trace-foundation.md#ordering-clocks-and-measurements).
Source order, receipt order, causal order and history selection remain separate; endpoint
wall elapsed time does not replace a vendor's reported monotonic duration.

### Promotion: a trajectory is defined by use, not by declaration

This former promotion rule is replaced. A tool hosting nested calls remains an Operation;
actual native child identity establishes an AgentThread. Arguments, returns, independent
resources and activations have separate lifecycles under
[unified-trace-foundation.md](unified-trace-foundation.md#execution-results-and-resources).

### `name` is absent when unknown

Names are optional evidenced metadata, never identity or proof of a child. Read-time
fallback labels belong to the reader and cannot enter durable authored content.
See [unified-trace-foundation.md](unified-trace-foundation.md#identity-and-reference-resolution).

### Keying rules that produce corruption when broken

Typed native scopes and source occurrence identities replace unscoped block/span keys.
Owner, actual record UUID and content-part index remain independently represented.
[unified-trace-foundation.md](unified-trace-foundation.md#identity-and-reference-resolution)
is authoritative; the dedicated vendor specs establish each actual namespace.

### Structure survives the ephemeral/stable split

There is no semantic ephemeral/stable retention split in the replacement. Every admitted
observation, including live deltas and lifecycle boundaries, is durably retained before
acknowledgment. Query caching is independently disposable. See
[unified-trace-storage.md](unified-trace-storage.md#admission-and-acknowledgments).

### Outcomes, and what a turn may close

[unified-trace-foundation.md](unified-trace-foundation.md#independent-lifecycle-claims)
owns outcome, close reason and observation state. A parent ending cannot sweep independent
children/resources closed. A terminal execution is not a promise that no later archive
evidence can arrive. Command cancellation acceptance is not proof of vendor cancellation.

### `is_backgrounded` inverts the sweep

The replacement removes parent-close sweeps entirely. A vendor background flag is evidence
about an operation/resource, not the exception that makes independently owned work survive.
See [unified-trace-foundation.md](unified-trace-foundation.md#operation-contract).

## An agent may start a turn

An Activation may follow a human delivery, another agent, a goal continuation or a
native/harness trigger. It is created from an observed execution boundary; input acceptance
alone cannot create one. Missing association stays unknown. Control waiter state and
trace ownership are independent under
[unified-trace-foundation.md](unified-trace-foundation.md#messages-deliveries-and-model-visibility).

## Failure

An adapter reports what it knows and omits what it does not. Three rules:

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

An envagent allocates `producer_id` and `producer_seq` before sending. It keeps
each item in its WAL spool until the server has durably ingested it and replies
with an `ack` for a contiguous prefix. The server assigns `server_seq` inside
the event/receipt transaction; replaying a producer item is therefore an
idempotent no-op and never creates a second canonical event.

Hello/welcome carries version negotiation, environment identity, connection
epoch, agent inventory, health, and the server's accepted cursors. A stale
epoch cannot acknowledge a newer connection. Commands carry an opaque request
id plus `replayable` or `at_most_once`; a result may be `accepted`, `rejected`,
or `unknown`, and an unknown at-most-once command is never retried blindly.

The wire reserves a `TurnInputFact` container message rather than a synthetic
user event, and typed `SessionEdge` rows for delegation and fork provenance.
The current adapter stream still emits only `Event`/`Request`/`Capabilities`/
`NativeRecord`; carrying those two facts (and child events) into the wire is an
explicit platform integration gate, not a synthesized timeline projection.
