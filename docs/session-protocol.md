# Session Protocol

The contract between the server and an agent. One event vocabulary, two ways of
obtaining events, three ways an adapter is asked for them.

## What changed from `sylph.agent.v3`, and why it is not a new version number

v3 was a wire protocol between the server and a subprocess speaking NDJSON on
stdio. Its **vocabulary** was right and is kept almost unchanged. Its **transport**
was a mistake this rewrite removes: an adapter is now a Rust trait linked into the
envagent, and NDJSON survives only on `sylph.env.v1`, where bytes genuinely cross a
machine boundary (`docs/environments.md`).

So there is no `v4`. The event and request vocabulary is versioned by the envagent
wire protocol it travels on, because that is now the only place a version can
disagree with anything.

Three things went away with the subprocess, and each had cost real debugging time:

**Agent discovery, and its two total failures.** Discovery spawned every candidate
adapter and exchanged a handshake. When it found none, `/agents` was empty, no
session could be created, every existing session reported `attached: false`, so
`commands` was `[]` and the composer's slash menu made `/` a dead keystroke — a
symptom three layers from the cause, logged at the same level as success. The second
instance was an E2E fixture holding only a stale `runner.sh`: the suite failed against
an agent that did not exist, reading as broken features. With adapters linked in, "is
this agent available" reduces to "is the vendor CLI on `PATH`", which is one question
with one answer and an error message that can say it.

**Two per-vendor normalizers for one vocabulary.** Live turns went through
`adapters/*/adapter.py`; native history went through trace-db's `parsers/`. Same
concepts, two implementations, two stores, two query paths — and the drift was
structural rather than incidental. They are now one module per vendor
(`docs/agents.md`).

**A protocol boundary that made vendor-key defects survivable.** The most-repeated
defect class in `AGENTS.md` is reading the wrong key off a vendor payload, and every
instance compiled and surfaced one or two layers from its cause. A
typed adapter does not prevent misreading a vendor's JSON, but it removes the JSON
round trip *behind* the adapter, so a mismatch between adapter and server is a
compile error rather than a field that parses as `None`.

What is **kept from v3** because it was right: facet-scoped capability updates, the
session-not-turn unit, typed config options, the request/resolve inversion with
declared forms, `span`-based delegated trajectories with promotion, and the
distinction between `UNSUPPORTED_FIELD` and `INVALID_VALUE`. Each of those exists
because a specific defect shipped without it, and every one is restated below rather
than referenced, because a reader implementing an adapter needs them in one place.

## The unit is a session, not a turn

An adapter holds a session for its lifetime. A turn is a message on it.

v1 spawned a process per turn, which made every stateful thing a vendor CLI knows —
loaded skills, permission grants, a warmed context, an MCP connection — reachable
only by restarting and re-resolving it. Worse, it made "the turn ended" and "the
process exited" the same event, so an agent that speaks after its turn (see § An
agent may start a turn) had nowhere to speak from.

So: `open` once, then a turn per `send`, and the outcome arrives as an event rather
than as an exit code.

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
success. That last one has its own paragraph in § A refused set because it shipped.

## Opening a session

`RunSpec` carries what a session is opened with:

| Field | Meaning |
| --- | --- |
| `workdir` | the agent's `current_dir`, already contained by the envagent |
| `native_id` | dictate this id, or `None` to resume an existing one, or `New` to let the vendor assign |
| `inject_identity` | whether to append `SOUL.md`; the server decides, not the adapter |
| `flags` | spawn-time options: model, effort, and whatever the agent declared |
| `permission_mode` | initial mode, if the vendor takes one at launch |

**Dictating the id is the preferred path and is already how it works.** *Measured
2026-08-27:* `claude`'s adapter mints a UUID and passes `--session-id`
(`adapters/claude/adapter.py:425`); `claude`, `pi`, and `gemini` accept the flag;
`codex` assigns its own and reports it. Dictating means the server knows a session's
identity before the first event, so nothing has to correlate afterwards.

An adapter that cannot dictate reports the vendor's id on `open`, and the server
keys the session on what came back. **An `open` that reports no id at all is a
failure**, not a degraded success: without an id the session can never be resumed,
so every link death — the 30-minute idle reaper, a restart, an explicit detach —
would make it permanently unlinkable. That was a real bug on the resume path.

### A launch flag gating a runtime-mutable value must be set unconditionally

*Measured on `claude` 2.1.246.* `--permission-prompt-tool` can only be asked for on
the command line, while the mode it serves changes at runtime through
`set_permission_mode`. Gating it on the *launch* mode therefore lost the approval
channel permanently for anyone who switched: launch bypassing, switch to `default`,
and every tool call answers `Claude requested permissions to use Bash, but you
haven't granted it yet` — a session that could never return to an interactive mode,
which is worse than the bug that led there.

Register such a channel unconditionally. A mode that decides alone simply never
consults it.

## Capability facets

A capability update names the facets it speaks about. **A facet that is not
mentioned is not changed.**

Facets: `commands`, `subagents`, `config`, `permissionMode`, `contextWindow`.

This is the one shape v2 got wrong badly enough to need a version, and the reasoning
survives the transport change intact. `claude` emits `system/init` once **per turn**,
not once per process — so an adapter answering it with a whole-surface payload
carrying only the permission mode erased the project's 46 slash commands on the first
exchange of every session, and the symptom appeared two layers away as a composer
menu that would not open.

Merging partials would have fixed that instance at the same cost and left the hazard:
a message about one part of the surface would still be *able* to speak for the whole
of it, and the next adapter would rediscover it. Naming the facet makes silence mean
silence.

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
setting one can change another's choices. This is not defensive: `pi`'s thinking
levels depend on the selected model, and `claude` carries `supportedEffortLevels` per
model. A design where effort is one session-global enumeration is wrong for both,
which is what v2's flat `flags.choices` was.

`Select` and `Boolean` cover every knob the three supported agents expose. A numeric
or free-text kind is a later addition, not a placeholder now.

### A config value must be unique, and must not be delimited with `/`

**`checked` is computed as a filter** — every producer writes `value ==
current_value` — so the moment two choices share a value, both rows show a checkmark
and neither is identifiably the real one. *Measured on `pi` 0.84.3:* of 372 offered
models, `deepseek-v4-pro` and `glm-5.2` are each offered by two providers, and three
display names are duplicated. A bare id also made one of each pair *unreachable*,
since a by-id lookup can only answer with one provider.

A model's identity is `(provider, id)`, never the id alone. Three layers enforce it,
because a client renders whatever an agent it has never seen reports — including
agents written by other people: the adapter issues provider-qualified values, the
list component enforces one current row per single-select group, and the server
validates a value against what was enumerated.

**The delimiter is a space, not `/`.** 350 of `pi`'s 372 model ids already contain a
slash (`anthropic/claude-opus-5`, `stealth/ox-alpha`), so splitting on the first or
last one mis-parses the majority of the catalogue. No provider and no id contains a
space. A `ConfigChoice.value` is **opaque to the client by contract** — a token to
echo back, never to parse — which is what lets this encoding change without a client
change.

### A refused set: two codes, and why the difference is load-bearing

| Code | Means | What a client may do |
| --- | --- | --- |
| `UnsupportedField` | this agent has no such option | disable the control |
| `InvalidValue` | the option is real; *this value* was rejected | report it, leave the control usable |

The server maps these to distinct statuses (`InvalidValue` → 400, `UnsupportedField`
→ 409), so answering with the wrong one tells a user their agent cannot do something
it does perfectly well, and invites disabling a picker because one of its six values
was refused for a reason the session could change.

Both adapters already drew this line and the server used to flatten it into one
error type. `claude` is the case that showed why it matters: `set_permission_mode` to
`bypassPermissions` is **refused outright** unless the process was launched with a
flag marking that mode available — a refusal about a *precondition*, reported as a
missing capability, three layers from the picker it sent people to.

A typed error is why the code travels rather than the message: the text is the
adapter's and may be reworded; the code is declared contract. Do not recover a code
by parsing a message.

### Three rules about reading a vendor's answer

Each of these shipped, and each compiled.

**A vendor's `success` is not a promise it did what you asked.** *Measured on
`claude` 2.1.246:* `manual` answers success while confirming `default` — the vendor
normalizes silently — so a picker offering it lands on a different row. An adapter
therefore records `current_value` from the vendor's **confirmation**, never from the
request, and offers only values it has watched the vendor apply as given.

**A success and a refusal are different shapes, so a reader for one swallows the
other.** `claude` answers a refused control with `subtype: "error"` and **no**
`response` member. An adapter reading `response.get("response") or {}` mapped every
refusal onto the same empty value a successful no-payload control returns: selecting
`bypassPermissions` was refused by the CLI, recorded as applied, answered 200, and
rendered as the current mode while every tool call went on prompting. Read the
discriminant before the payload; `{}` must never be reachable from two opposite
outcomes.

**Read current values from the vendor, not from what you last set.** `pi`'s adapter
reported `current_value` from a field only one code path populated, so any surface
built by another path — a re-attach, a set reply — reported `None` while the session
plainly had a model, and the chip fell back to "agent default" with the checkmark on
no row. The vendor is also the only thing that knows whether a set was *honoured*,
and a session can be steered by the agent's own TUI besides.

## Two kinds of command

A slash command is either a **turn** (`/verify`, `/review` — it does work and
produces a transcript) or a **control** (`/model`, `/effort` — it changes state and
produces nothing). Sent as prompt text, `/model` returns a paragraph listing models,
which is the CLI behaving correctly, because a headless client has no picker to open.

So a control command is not translated; it is **typed state** a client renders as a
picker, and choosing sends a typed set. Three protocols reached this split
independently, which is what makes it a generalization rather than a guess: ACP has
`AvailableCommand` beside `SessionConfigOption`, `codex`'s app-server has no command
concept at all but does have `model/list` and `config/value/write`, and `claude`
exposes both.

A command declared by the agent carries whether it takes input; the composer's menu
renders from the `commands` facet, which is session state and not agent state — a
project's real command list depends on files the CLI reads at startup.

## Requests: the agent asks, a human answers

The inversion v1 could not express. A request is raised by the adapter, journaled by
the server, published to every attached client, and blocks nothing else.

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

### Announcing arrival without announcing departure lets a client's view only grow

A request was published when raised and on no path when it left `pending` — not on
resolve, not when a dying link abandoned it. So the set a client believed it was
blocked on was monotonic and shrank only when a 20-second backstop poll happened to
fire. That poll is what made it severe rather than merely late: background refetch is
off by default, so a backgrounded tab was not polling at all, and refetch-on-focus was
off too. A question answered on a phone therefore sat on the desktop until reload.

Three things make the fix hold rather than patching one path:

- **One publisher every transition calls.** Three call sites each remembering to
  announce is three chances to forget, which is how this arose.
- **The payload is the row, not the transition.** A client reconciles against a state
  rather than replaying a vocabulary of verbs, so a status added later needs no client
  change.
- **Published before the forward to the adapter.** The claim is what makes the answer
  final, so a request answered against a *dead* link must still retire everywhere.

Testing this by re-reading the request list proves nothing — the row was always
correct and only the notification was missing. Assert on the session stream, and
end-to-end use two browser **contexts** rather than two tabs, since two tabs share a
cookie jar and can pass while a genuinely separate client still hangs.

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

**A form is declared, never inferred.** Before this existed a client guessed what to
collect by sniffing `detail` for field names it recognized (`command`, `file_path`,
`url`). That is right for an approval, whose subject *is* one proposed tool input, and
unfixable for anything else: a question's detail carries none of those, so a `Choice`
rendered as bare buttons with nowhere to type and an `Elicitation` — whose entire
purpose is free text — rendered as a read-only dump of its own placeholder. Only the
agent knows what it is asking for.

**`options` are verbs; fields carry data.** A `Choice` previously overloaded `options`
as both the answer and the submit button, which is why picking had to submit
immediately and why a note had nowhere to live. When a surface cannot express
something, check whether one field is carrying two meanings.

**The server validates `input` against the stored form** before claiming the request:
an unknown field id, a `One` field given an array, a value outside `choices` where
`allow_other` is not set, or a missing `required` field is refused. Same reason as
config values — the client renders whatever an agent it has never seen reported, so
the server is what keeps a malformed answer from reaching a vendor CLI that will do
something undefined with it.

**Answering completely is not cosmetic.** *Measured against `claude` 2.1.238:* a reply
carrying every question's answer is delivered to the model as *"Your questions have
been answered … you can now continue with these answers in mind"*, while a reply
answering one of two arrives as the weaker *"The user answered …"*. Both look like
success from outside. That is why one form carries N fields rather than one question.

**Writing an answer into a field the vendor does not read is silent.** The
`AskUserQuestion` reply set `questions[0].selectedOptions`, a key that occurs
**nowhere** in the 321 MB `claude` binary. The CLI built its tool result from
`answers[question]`, found nothing, and told the model *"The user did not answer the
questions."* — so a correct pick, correctly rendered and correctly transported,
presented as a UI that had lost the click. Grep the binary for the key, and assert on
the tool result the model actually reads: "the panel closed" and "the answer arrived"
are different claims and only the second is the feature.

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
implementation for emit sites, not the types for declarations. A declared-only event
ships an adapter branch that never fires, and a branch that never fires is
indistinguishable from one whose condition is merely rare. This file previously
claimed `pi`'s `agent_start`/`agent_end` were such a case; **that was wrong**, and
re-measuring against an unbundled source-mapped install found both emitted and both
reaching the wire. "I grepped and found nothing" is a claim about your grep as much as
about the vendor — see `docs/agents.md`.

## Delegated trajectories

A turn is not a sequence. An agent delegates, its delegates delegate, and a workflow
fans out to a dozen agents and then synthesizes. **One field carries all of it:**

```
span    the trajectory this event happened in. Absent means the root turn.
```

A `subagent` event *is* the trajectory's own record: its `stream_id` is the trajectory
id and its `span` names the trajectory it was delegated from. A `thinking` block
inside that trajectory carries the same `span`. Depth is unbounded, siblings may run
concurrently, and a workflow phase is a trajectory whose children are trajectories. No
case in the protocol distinguishes any of these, which is what makes it a
generalization rather than three features.

**`span` is not `stream_id`.** `stream_id` identifies one block across its phases;
`span` identifies the trajectory a block ran in. A `subagent` event is the one place
they meet, because opening a trajectory is itself a block with a lifecycle.

**Why a pointer and not nesting.** The v3 table once said a child's events arrived
*nested inside* the parent event. That is unimplementable, not merely unimplemented:
an envelope must be closed before it is sent and a trajectory streams for minutes. It
also contradicts the journal, where an event is a row — a nested payload makes one row
hold a subtree, so a delta inside it cannot be addressed, folded, or pruned. A flat
stream with a parent pointer is what distributed tracing settled on, and it is already
how the vendor speaks: `claude` tags a subagent's output with `parent_tool_use_id`.

### Promotion: a trajectory is defined by use, not by declaration

**Any `stream_id` another event names as its `span` is a trajectory**, whatever kind
of event opened it. A `tool_call` so named is *promoted*: it stays a tool call and also
becomes the container its children already claim it is.

This is a definition, not a fallback, because **delegation is only knowable
retroactively**. An adapter learns a tool call delegated when a child arrives carrying
its id — by which time the `tool_call` event has been sent and cannot be recalled.
Anything deciding at open time is a guess standing in for a fact that has not arrived.
`claude`'s adapter guessed by tool name (`Task`), correct until the vendor shipped
`Agent` and `Workflow`; an MCP server can add another at any time, so the whitelist
could not be completed, only extended after each report. Two real `Workflow` cards
read *running* permanently with no interior, and the four `Agent` calls beside them had
their children rooted beside the call instead of inside it.

Promotion needs no list: an agent nobody has written yet nests correctly by tagging
its children, which is the same `span` it must already set for its output to be
attributed at all.

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
early or the link dies mid-input, the placeholder becomes that trajectory's durable
name, indistinguishable from one an agent actually chose. Rows already written that way
**cannot be repaired**: nothing can tell a guessed name from a real one after the fact,
so historical rows carrying a literal are unknowable rather than data. `Option`/`null`
is the only value that stays honest in a journal.

Two consequences: a client renders its own fallback, and the server keeps the best name
any phase gave a block (`MAX` ignoring NULLs, so a phase that carried one wins). That
second part is what makes omitting at `started` safe.

Landing such a change has an order, and reversing it is worse than doing nothing: the
**producer** stops sending a placeholder first, the **presentation** layer renames its
fallback second. Reversed, one condition acquires two labels.

**Consolidation keeps the closing event's fields, so a name given at open is lost
unless it is carried forward.** The fold keys a block on its `completed` event — right
for text, wrong for identity, since a trajectory is named at `started` and renamed by
the delta carrying its brief, while its close reports an outcome. Every subagent card
read the fallback once a turn consolidated, which looks exactly like a frontend bug two
layers above where the name was dropped. Any per-block attribute only an early phase
carries needs the same treatment.

### Keying rules that produce corruption when broken

**Vendor block state must be keyed by `(trajectory, index)`, never by the vendor's
index alone.** `claude` numbers content blocks per message, so two concurrent
subagents both open index 0. One index-keyed map made each one's deltas append to the
other's text — data corruption, not a display fault, and invisible until two delegates
run at once.

**One host may multiplex N delegates, so a trajectory id must be the delegate's.**
`claude`'s `Workflow` fans out under a single `tool_use_id`. Keyed by the host, a
two-agent workflow rendered as **one** card reading `Demo: agent-2` — the last name to
arrive won and `agent-1` was absent from the timeline entirely. The per-delegate
identity is `task_id`, with the host as its `span`. A fixture with one delegate per
host cannot observe this: at N=1 the collapse is indistinguishable from correct.

**A vendor tag coarser than a trajectory is not refinable by guessing.** `claude` tags
forwarded output with `parent_tool_use_id` — the *host*, which is precisely the id it
multiplexes — so with N delegates the vendor's own tag cannot say which produced a
given event. Attribute such output to the **host**, which is true, rather than picking
a delegate, which is corruption indistinguishable from correct output. Only N=1 is
safe. Two obligations follow, both real defects rather than hypotheses: a re-label must
be **latched** (the table grows while the turn runs, so answering freshly per event
splits one delegate's output across container and delegate at whatever instant the
notification landed, and both halves render nested), and vendor block state must stay
keyed by the vendor's tag rather than by the trajectory it resolves to (the tag is the
only part identical across `start`, `delta`, and `stop`).

### Structure survives the ephemeral/stable split

**A `subagent` event is stable at every phase.** Ordinarily `started` and `delta` are
ephemeral and never reach the journal. That is right for content and wrong for
structure: the `started` phase is where a trajectory's identity, name, and parent live,
so dropping it leaves every child pointing at a parent that was never recorded, and a
reload flattens the whole subtree into the parent's own work — the pre-`span` bug,
reintroduced by the storage tier instead of the protocol.

**One fact with two derivations diverges the moment either grows a case.** Stability
was both a field on the event and a method recomputing it from the phase. Equivalent,
until `subagent` became stable at every phase for the structural reason above — after
which a trajectory's `started` was classified stable, published as stable, and silently
not journaled, because the method disagreed with the field beside it. Read the field.

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

**A trajectory with no interior says so.** A vendor may report that delegated work
exists without forwarding any of it. *Measured on claude 2.x:* `--forward-subagent-text`
forwards only for the built-in `Task` — an `Agent` delegation produced **zero**
envelopes carrying `parent_tool_use_id`, and the delegate's own `Read` arrived as a
top-level assistant message indistinguishable from the root agent's work. So for every
delegating tool except `Task` the interior cannot be attributed at all, and a *closed*
trajectory with no children is a real terminal state rather than a loading one. The card
has to say so: presented as expandable it invites opening a branch that can never fill,
and saying it only after the click is the lossy collapse the tree exists to avoid.

**Progress is a snapshot, not delta text.** A vendor's progress notification re-sends
its whole current summary rather than a suffix, so appending each one repeats it —
measured at six copies of one sentence. Such a summary **replaces**, leaving `text`
append-only as everywhere else.

**A trajectory id is session-scoped, not turn-scoped.** Two rules follow, and without
them a delegation that outlives its turn cannot be described at all.

*A turn closes only what it holds.* The turn-settle sweep resolves a trajectory only
when some event **on that turn** carries it as its `stream_id`. A bare `span` naming
something this turn never emitted is a reference outward, usually to a trajectory an
earlier turn opened; treating it as an opening fabricates a record — measured, a
`Workflow` that completed cleanly on the dispatching turn acquired a second
`abandoned` close on a later turn for work it never ran. Note the obvious repair,
"skip an id another turn declared", is **not available**: consolidation has moved the
earlier turn's rows out by then, so the evidence of ownership is gone. The decidable
rule is the local one.

*A late outcome is recorded on the turn that owns the trajectory.* The event stays
where it arrived — that is when it happened — and the server additionally corrects the
owning turn's record. Only a provisional record may be corrected: a trajectory still
open, or a close the *server* authored. An outcome the agent itself stated is never
rewritten, because a later mention is a description, not a re-decision. This is what
makes `abandoned` genuinely non-final rather than merely documented as such.

### `is_backgrounded` inverts the sweep

**An adapter that knows a delegation runs in the background must say so, and must
re-attach the flag to every later phase.**

The sweep closes trajectories an adapter left open so silence never has to explain a
gap. That is right in general and wrong for work the agent says is backgrounded, which
is *expected* to outlive its turn and report on a later agent-initiated turn: closing
it writes a contradiction the real completion cannot undo, and both records then exist
on different turns with the earlier card still saying the work never finished.

Three details, each of which was a defect:

- **Re-attaching is what an adapter author will otherwise get wrong.** `claude`
  reports the flag on `task_started` and on **no** later notification, so the adapter
  remembers it and stamps every subsequent phase. Set only at open, every backgrounded
  delegation is closed as `abandoned`.
- **The server's exclusion must test any row sharing the trajectory**, not the row
  being swept — a correlated subquery over the trajectory, not a predicate on one row.
  And when the flag is read out of stored JSON, compare against SQLite's integer `1`:
  a comparison written against `true` matches nothing, and every backgrounded
  delegation is swept closed.
- **Skipping a step that writes a wrong record can delete the right one.** The
  exclusion removed the contradiction and, unnoticed, the delegation: a trajectory with
  no close carries no text, so it folded into no block and its children reattached to
  the root — the flattening `span` exists to prevent, one layer down. When a fix is
  "stop writing X", check what still writes the *record*.

Keep the exclusion narrow — an ordinary delegation left open is the permanent spinner
the sweep exists to prevent. And **startup recovery deliberately ignores the flag**,
because a dead process cannot report: "abandoned after a restart" is a truthful
terminal state and must stay distinguishable from "still running in the background".

## An agent may start a turn

Events arriving with no turn in flight were dropped — defensibly, since an event
carries no turn id and a guessed one misplaces it in the transcript forever. The error
was the conclusion. *Measured on `claude` 2.x:* a session with backgrounded work emits
the delegate's completion **after** its turn's `result`, then an entire exchange of its
own to report it, stdout still open, process still alive. So the drop discarded exactly
the completion a user had been promised, and no monitor, schedule, or background job
could reach a client however the rest of the stack was built.

The server opens an **agent-initiated turn** (`origin: "agent"`) on the first
out-of-turn event — the first of *any* kind, because the vendor's own `turn`/`started`
arrives after the payload, so waiting for the bracket keeps the announcement and drops
what it announces.

Three consequences worth holding onto:

- Such a turn has **no waiter**, so settling it means recording terminal state
  directly rather than waking one. Leaving it unsettled wedges the session, since a
  live turn refuses new messages.
- Its request text is empty, so a client that does not render `origin` shows an answer
  to a message the user never wrote.
- A delegate closed by the turn-settle sweep may later report its *real* outcome on
  that turn, so `abandoned` is not final.

**A waiter's absence means two different things.** Settling every waiter-less turn made
a deliberate stop get reported as a lost link, because claiming the waiter first is how
a user's stop wins over the link teardown it causes. An agent turn never had a waiter; a
user's turn whose waiter is gone has an owner already. `origin` is what separates them.

## Failure

An adapter reports what it knows and omits what it does not. Three rules, each from a
shipped defect:

- **Never report a refusal as a success** (§ A refused set).
- **Never synthesize a value the vendor did not give.** A synthesized id, name, or
  fingerprint is a lie a journal makes permanent.
- **An error must name the layer that is actually at fault.** Answering a malformed
  value with "`model` is not settable on a pi session" says the option does not exist
  when the option was fine and the value was not. `pi`'s adapter separates unknown-option
  from invalid-value for exactly this reason. The general form cost two rounds: a refused
  write with no error path in the client presents as a dead control, so the layer that is
  *not* at fault is the one that gets debugged.
