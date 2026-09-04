# Session Protocol

The contract between the server and an agent. One event vocabulary, two ways of
obtaining events, three ways an adapter is asked for them.

An adapter is a Rust trait linked into the envagent, not a subprocess: events are
values, and the only place the vocabulary crosses a machine boundary is
`sylph.env.v1` (`docs/environments.md`). So the vocabulary is versioned by that wire
protocol, because that is the only place a version can disagree with anything.

**Read this before writing an adapter.** Every rule below is a contract an adapter
must satisfy, and most of them are rules whose violation is *silent* — a vendor
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

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Which agent this speaks for, and what the local CLI reports about itself.
    async fn probe(&self) -> Result<AgentInfo>;

    /// Hold a live session's stdio. The returned handle carries turns both ways.
    async fn run(&self, spec: RunSpec) -> Result<Box<dyn Session>>;

    /// Parse what the vendor's native store already holds, once.
    async fn adopt(&self, id: &NativeId) -> Result<EventStream>;

    /// Parse, then follow the native store as it grows.
    async fn watch(&self, id: &NativeId) -> Result<EventStream>;

    /// Which native sessions this machine's store holds.
    async fn enumerate(&self, filter: &StoreFilter) -> Result<Vec<NativeSummary>>;
}
```

**All three modes emit the same events.** That is the point of the shape: the
journal, the history fold, the timeline, replay, and search see one vocabulary and
cannot tell which mode produced a given row. The only visible difference is
`fidelity` on the record (`docs/architecture.md`), which says how much resolution the
source had.

| Mode | Source | Fidelity | Control facet |
| --- | --- | --- | --- |
| `run` | the process's own stdio | `stream` | full |
| `watch` | native store, followed | `blocks` or `messages`, per vendor | whatever the vendor offers a second party |
| `adopt` | native store, one pass | `blocks` or `messages`, per vendor | none while it runs; full once resumed by `run` |

`enumerate` is what makes adopt and watch offerable in a UI: it answers "what is on
this machine" without parsing every session.

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
`--session-id` the adapter mints; `codex` assigns its own and reports it. Dictating
means the server knows a session's identity before the first event, so nothing has to
correlate afterwards.

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

Kinds are `thinking`, `assistant`, `tool_call`, `tool_result`, `usage`, `status`,
with phases `started` / `delta` / `completed`. Beyond those:

| Kind | Carries |
| --- | --- |
| `subagent` | declares a delegated trajectory; its `stream_id` is that trajectory's id |
| `compact` | a compaction boundary, so the timeline shows where context was condensed |
| `turn` | `started` / `completed` with usage, cost, duration, stop reason |
| `capabilities` | facet-scoped updates (§ Capability facets) |
| `notice` | agent-originated notification worth surfacing |

`kind` is deliberately unconstrained at the storage layer: a new kind is additive, and
a `CHECK` listing them is a promise to break on the next adapter that adds one.

**Reserved, not implemented: `plan` and `diff`.** No adapter emits either and no
timeline branch handles them. They are recorded as the intended shape for making plan
mode reviewable and a file change render as a diff rather than a tool log — today a
plan arrives as an `Approval` whose detail is prose, and a diff is reconstructed
client-side from a tool payload. Do not write a client branch expecting them.

**A type in a vendor's event union is not a promise anything emits it.** Grep the
implementation for emit sites, not the types for declarations: a declared-only event
ships an adapter branch that never fires, and such a branch is indistinguishable from
one whose condition is merely rare. Note the symmetric trap — "I grepped and found
nothing" is a claim about your grep as much as about the vendor, so prefer an unbundled
source-mapped install to a minified bundle before concluding a type is dead
(`docs/agents.md`).

## Delegated trajectories

A turn is not a sequence. An agent delegates, its delegates delegate, and a workflow
fans out to a dozen agents and then synthesizes. **One field carries all of it:**

```
span    the trajectory this event happened in. Absent means the root turn.
```

The server's session-trace projection also carries `arrivalTurnId`: the turn on which
the event was received. It is provenance, not ownership. A child trajectory may finish
after its parent turn, so consumers must not use the arrival turn as the trajectory's
container; the first declaration (or promoted host) remains the owner.

A `subagent` event *is* the trajectory's own record: its `stream_id` is the trajectory
id and its `span` names the trajectory it was delegated from. A `thinking` block
inside that trajectory carries the same `span`. Depth is unbounded, siblings may run
concurrently, and a workflow phase is a trajectory whose children are trajectories. No
case in the protocol distinguishes any of these, which is what makes it a
generalization rather than three features.

**`span` is not `stream_id`.** `stream_id` identifies one block across its phases;
`span` identifies the trajectory a block ran in. A `subagent` event is the one place
they meet, because opening a trajectory is itself a block with a lifecycle.

### Time axes are independent

`seq` is the only total source/order axis. Timestamps can tie, arrive out of order, or
come from different machines, so replay and producer merging must never sort by time.
`arrivalAtMs` records when Sylph received the fact on the server clock and is always
available on new rows. `recordedAtMs` is the vendor's own timestamp, when the native
source supplied one; it is absent for records such as workflow journal closes that carry
no timestamp. The legacy `createdAtMs` remains for compatibility, but its clock depends
on the record mode and must not be used to infer a duration across live and native data.

Durations are exact only when both endpoints are in one domain: arrival start/end for a
live stream, or recorded start/end for native history. Missing recorded endpoints mean
the duration is unknown, not zero.

**A pointer, never nesting.** A child's events cannot arrive *nested inside* the parent
event: an envelope must be closed before it is sent, and a trajectory streams for
minutes. It also contradicts the journal, where an event is a row — a nested payload
makes one row hold a subtree, so a delta inside it cannot be addressed, folded, or
pruned. A flat stream with a parent pointer is what distributed tracing settled on, and
it is already how the vendors speak: `claude` tags a subagent's output with
`parent_tool_use_id`.

### Promotion: a trajectory is defined by use, not by declaration

**Any `stream_id` another event names as its `span` is a trajectory**, whatever kind
of event opened it. A `tool_call` so named is *promoted*: it stays a tool call and also
becomes the container its children already claim it is.

This is a definition, not a fallback, because **delegation is only knowable
retroactively**. An adapter learns that a tool call delegated when a child arrives
carrying its id — by which time the `tool_call` event has been sent and cannot be
recalled. Anything deciding at open time is a guess standing in for a fact that has not
arrived yet.

**So never classify a delegating tool by name.** A whitelist of tool names cannot be
completed, only extended after each new one is reported: a vendor adds delegating tools
over time, and an MCP server can add one at any moment. The failure is visible — a host
that reads *running* forever with no interior, or children rooted beside their host
rather than inside it — but only for tools nobody thought to list.

Promotion needs no list at all: an agent nobody has written yet nests correctly by
tagging its children, which is the same `span` it must already set for its output to be
attributed.

Declaration keeps a purpose promotion does not share — it can name a trajectory before
any child exists, and state an outcome independent of its host's. Both produce the
same tree.

**A promoted trajectory's lifecycle is its `tool_result`, not its phase.** A
`tool_call`'s `completed` phase means its *arguments* finished streaming. Reading the
phase reports every delegation as finished the instant its input parsed.

### `name` is absent when unknown

**`name` is optional on every phase, including `started`.** An adapter that does not
yet know what a delegation is called must **omit the field** rather than send a
placeholder.

This is not a tidiness rule. **A guess written here becomes permanent.** The history
fold keeps the best name any phase gave a block, so a later `delta` carrying the real
brief normally wins — but when the brief never finishes, because the delegation fails
early or the link dies mid-input, the placeholder *is* that trajectory's durable name and
is afterwards indistinguishable from one an agent chose. Nothing can tell the two apart
retroactively, so such rows are unknowable rather than data. **`Option`/`null` is the
only value that stays honest in a journal.**

Two consequences: a client renders its own fallback, and the server keeps the best name
any phase gave a block (`MAX` ignoring NULLs, so a phase that carried one wins). That
second part is what makes omitting at `started` safe.

**When two layers both supply a default for one field, the layer that writes data stops
first and the layer that renders changes second.** Reversed, one condition acquires two
labels — the producer's string for some rows and the new fallback for the rest — which is
worse than a single imprecise label and invisible until a card renders.

**Consolidation keeps the closing event's fields, so a name given at open must be
carried forward explicitly.** The fold keys a block on its `completed` event — right for
text, wrong for identity, since a trajectory is named at `started` and renamed by the
delta carrying its brief, while its close reports only an outcome. Drop it and every card
reads its fallback once a turn consolidates, which looks exactly like a frontend bug two
layers above where the name was lost. **Any per-block attribute that only an early phase
carries needs the same treatment.**

### Keying rules that produce corruption when broken

**Key vendor block state by `(trajectory, index)`, never by the vendor's index
alone.** `claude` numbers content blocks per message, so two concurrent subagents both
open index 0. An index-keyed map appends each one's deltas to the other's text — data
corruption rather than a display fault, and invisible until two delegates run at once.

**A trajectory id must be the delegate's, because one host may multiplex N of them.**
`claude`'s `Workflow` fans out under a single `tool_use_id`, so keying by the host
collapses a whole fan-out into one card whose name is whichever delegate reported last,
with the others absent from the timeline. The per-delegate identity is `task_id`, with
the host as its `span`. A fixture with one delegate per host cannot observe this: at N=1
the collapse is indistinguishable from correct.

**A vendor tag coarser than a trajectory is not refinable by guessing.** Where a vendor
tags forwarded output with the *host* id it multiplexes, that tag cannot say which
delegate produced a given event. Attribute such output to the **host**, which is true,
rather than picking a delegate, which is corruption indistinguishable from correct
output. Only N=1 is safe. Two obligations follow:

- **A re-label must be latched.** The delegate table grows while the turn runs, so
  answering freshly per event splits one delegate's output between container and
  delegate at whatever instant a notification landed, and both halves render nested.
- **Vendor block state stays keyed by the vendor's own tag**, not by the trajectory it
  resolves to, because the tag is the only part identical across `start`, `delta`, and
  `stop`.

### Structure survives the ephemeral/stable split

**A `subagent` event is stable at every phase.** Ordinarily `started` and `delta` are
ephemeral and never reach the journal. That is right for content and wrong for
structure: the `started` phase is where a trajectory's identity, name, and parent live,
so dropping it leaves every child pointing at a parent that was never recorded, and a
reload flattens the whole subtree into the parent's own work — the exact failure `span`
exists to prevent, reintroduced by the storage tier rather than by the protocol.

**One fact must have one derivation.** Stability is a field on the event; nothing may
recompute it from the phase. Two derivations agree until either grows a case — and this
one did, when `subagent` became stable at every phase for the structural reason above —
after which the recomputing copy silently disagrees and the event is published as stable
while never being journaled. **Read the field.**

**An unknown `span` is not an error.** A client receiving an event naming a trajectory
it has not seen renders it at the root rather than dropping it. The alternative loses
work exactly where structure is least reliable: a link that died mid-delegation, or an
adapter emitting out of order.

**Two nodes naming each other silently delete both.** A cycle in `span` pointers is
not merely a recursion hazard: attachment holds the first as an orphan waiting for the
second, which then adopts it *and* is adopted by it, so neither is a root, neither is
reachable, and the pair vanishes with nothing reporting it missing. No single-pass
attachment detects this — repair by reachability afterwards, and carry a visited guard
in any rollup, because unbounded recursion in a render path is a frozen tab.

### Outcomes, and what a turn may close

**A trajectory has its own outcome.** `completed` with an error flag closes it, so a
failure three levels down is reportable at every level above without a client inferring
failure from a missing result. An adapter that loses a trajectory leaves it open, and
the server closes open trajectories when the turn terminates — an unexplained gap is
worse than a recorded failure. Keep `abandoned` distinct from `failed`: the server
closing a trajectory an adapter left open means nobody reported an outcome, and
colouring that red reports an interrupted turn as a broken one.

**A collapsed subtree must report what it hides.** Hiding a delegation's work by
default is only defensible if a failure three levels down is still visible at the top;
otherwise the user must open every branch to learn something broke, which is the work
collapsing was supposed to save. A descendant failure escalates its ancestors' status,
and a failed branch expands itself to the tool that failed.

**Interior reachability is a typed trajectory fact, never a child-count inference.** A
`subagent` phase may carry `interior_state`: `pending`, `partial`, `full`,
`forwards_nothing`, or `unreadable`. The producer owns the transition: `pending` is a
source expected but not yet readable; `partial` is a live readable prefix; `full` is a
complete reachable source; `forwards_nothing` is a terminal vendor capability; and
`unreadable` is an expected source that could not be read. A source that has ended must
never leave `pending` in durable history. The frontend renders the reported fact; it may
use actual children only to decide whether an expansion has content *now*, never to decide
what absence means. Older rows lack this field and use the legacy fallback until rewritten.

**Progress is a snapshot, not delta text.** A vendor's progress notification re-sends its
whole current summary rather than a suffix, so appending each one repeats the same
sentence. Such a summary **replaces**, leaving `text` append-only as everywhere else.

**A trajectory id is session-scoped, not turn-scoped.** Two rules follow, and without
them a delegation that outlives its turn cannot be described at all.

*A turn closes only what it holds.* The turn-settle sweep resolves a trajectory only
when some event **on that turn** carries it as its `stream_id`. A bare `span` naming
something this turn never emitted is a reference outward, usually to a trajectory an
earlier turn opened; treating it as an opening fabricates a record — a host that
completed cleanly on its dispatching turn acquires a second, contradicting `abandoned`
close on a later turn for work it never ran there. Note that the obvious repair, "skip an
id another turn declared", is **not available**: consolidation has moved the earlier
turn's rows out by then, so the evidence of ownership is gone. Only the local rule is
decidable.

*A late outcome is recorded on the turn that owns the trajectory.* The event stays
where it arrived — that is when it happened — and the server additionally corrects the
owning turn's record. Only a provisional record may be corrected: a trajectory still
open, or a close the *server* authored. An outcome the agent itself stated is never
rewritten, because a later mention is a description, not a re-decision. This is what
makes `abandoned` genuinely non-final rather than merely documented as such.

*The turn does not read terminal until the sweep has finished.* A turn holding a known
outcome over an unfinished record has its own status, `settling`, which is **in flight**.
Every client question of the form "is this done" turns on the four terminal statuses, and
this is what lets one answer be enough: read `completed`, refetch history, find everything.

The alternative was tried and is worth naming, because it looks harmless. Writing the
outcome first and sweeping afterwards makes `status` answer two questions at once — what the
turn produced, and whether its record is complete — and no client can tell which one it is
reading. One that acts on the first meaning gets the second: it stops listening before the
sweep's own closes arrive, and its refetch reads a journal the sweep has not written yet, so
a delegation the server closed renders as running forever. The sweep usually won that race,
which is what made it present as flakiness. `docs/database-schema.md` § A terminal status
means the record is finished holds the measurement.

### `is_backgrounded` inverts the sweep

**An adapter that knows a delegation runs in the background must say so, and must
re-attach the flag to every later phase.**

The sweep closes trajectories an adapter left open so silence never has to explain a
gap. That is right in general and wrong for work the agent says is backgrounded, which
is *expected* to outlive its turn and report on a later agent-initiated turn: closing
it writes a contradiction the real completion cannot undo, and both records then exist
on different turns with the earlier card still saying the work never finished.

Three details, each of which is easy to get wrong in a way nothing reports:

- **Re-attach the flag to every phase.** A vendor may report it on the opening
  notification and on none of the later ones, so the adapter must remember it and stamp
  each subsequent phase. Set only at open, every backgrounded delegation is closed as
  `abandoned` anyway.
- **The server's exclusion must test any row sharing the trajectory**, not the row being
  swept — a correlated subquery over the trajectory, not a predicate on one row. And when
  the flag is read out of stored JSON, compare against SQLite's integer `1`: a comparison
  written against `true` matches nothing, and every backgrounded delegation is swept
  closed.
- **Skipping a step that writes a wrong record can delete the right one.** A trajectory
  with no close carries no text, so it folds into no block and its children reattach to
  the root — the flattening `span` exists to prevent, one layer down. Whenever a fix is
  "stop writing X", check what still writes the *record*.

Keep the exclusion narrow — an ordinary delegation left open is the permanent spinner
the sweep exists to prevent. And **startup recovery deliberately ignores the flag**,
because a dead process cannot report: "abandoned after a restart" is a truthful
terminal state and must stay distinguishable from "still running in the background".

## An agent may start a turn

**An event arriving with no turn in flight must not be dropped.** An agent speaks
between turns: a session with backgrounded work emits its delegate's completion *after*
the dispatching turn's result, then opens an exchange of its own to report it, with
stdout still open and the process still alive. Dropping those discards exactly the
completion a user was promised, and no monitor, schedule, or background job can reach a
client however the rest of the stack is built.

So the server opens an **agent-initiated turn** (`origin: "agent"`) on the first
out-of-turn event — the first of *any* kind. Waiting for the vendor's own `turn`/`started`
bracket does not work, because that arrives *after* the payload it brackets: the wait
keeps the announcement and drops the thing being announced.

Three consequences worth holding onto:

- Such a turn has **no waiter**, so settling it means recording terminal state
  directly rather than waking one. Leaving it unsettled wedges the session, since a
  live turn refuses new messages.
- Its request text is empty, so a client that does not render `origin` shows an answer
  to a message the user never wrote.
- A delegate closed by the turn-settle sweep may later report its *real* outcome on
  that turn, so `abandoned` is not final.

**A waiter's absence means two different things**, and `origin` is what separates them.
An agent turn never had a waiter; a user's turn whose waiter is gone already has an
owner, because claiming the waiter first is how a user's stop wins over the link teardown
it causes. Settle every waiter-less turn alike and a deliberate stop is reported as a
lost link.

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
