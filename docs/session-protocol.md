# Agent Session Protocol (`sylph.agent.v3`)

The contract between the Sylph server and a pluggable agent.

## What v3 changed, and why it needed a version

v2 held for one agent and broke on the second. Two of its choices were wrong in a
way no amount of adapter code could hide.

**A capability surface was one value, replaced wholesale.** `claude` emits its
`system/init` message *once per turn*, not once per process — measured, two user
messages produce two `init`s. The adapter turned each into a `capabilities` event
carrying only the permission mode, and the server assigned it over the cached
surface. So the first turn of every session erased the project's 46 slash
commands, and the composer's menu stopped opening. Merging partials would have
patched the symptom; the defect was that a message speaking about one part of the
surface could silently erase another. v3 makes a capability update **facet-scoped**
(§ Capability facets), so an unmentioned facet cannot be cleared.

**Every slash command was modelled as a kind of turn.** That is right for
`/verify` and wrong for `/model`. Sent as prompt text, `/model` returns a
paragraph listing the models — which is the CLI behaving correctly, because a
headless client has no picker to open. The fix is not in the translation. It is
that a control command needs typed state a client can render as a picker, so
choosing an option sends a typed set rather than prose.

Three protocols reached the same split independently, which is what makes it a
generalization rather than a guess:

| | commands | model and mode |
| --- | --- | --- |
| **ACP** (`gemini --acp`, `opencode acp`) | `AvailableCommand {name, description, input?}` | `SessionModelState`, `SessionModeState`, `SessionConfigOption` |
| **codex app-server** | no command concept at all | `model/list`, `config/value/write`, `thread/settings/updated` |
| **pi** (`--mode rpc`) | `get_commands`, which *excludes* built-in TUI commands by design | `set_model`, `set_thinking_level`, `get_available_models` |

`claude` is the outlier: `/model`, `/context`, `/compact` and `/clear` sit in the
same list as `/verify` with no field telling them apart. Since no vendor supplies
that discriminator, **the adapter classifies** — it owns vendor translation, and
which of an agent's commands are control surfaces is a vendor fact. A hardcoded
list in the server or the UI would be wrong for the next agent, which is the
mistake v3 exists to stop repeating.

## Why the old contract cannot be extended

The v1 contract is: pass the prompt on stdin, **close stdin**, read NDJSON from
stdout, trust the last line. Closing stdin is load-bearing there — it is how the
runner knows the prompt ended — and it is also what makes a native experience
impossible.

This was measured, not assumed. Running `claude -p --input-format stream-json
--output-format stream-json --permission-prompt-tool stdio` against a project
with an `ask` permission rule, the CLI sends a `can_use_tool` request back over
stdin and waits. With v1's `stdin.close()` the turn dies:

```
Tool permission request failed: AbortError: Stream closed
```

Holding stdin open, the same request arrives, is answerable, and the turn
proceeds. Every native capability follows from that one difference:

| Capability | Native channel | v1 result |
| --- | --- | --- |
| Approval prompts | `can_use_tool` request, awaits a reply | Stream closed; tool fails |
| Slash commands | `initialize` returns 46 real commands here | 1–2 hand-declared in `--describe` |
| Subagents | `task_*` notifications, `parent_tool_use_id` | Flattened into opaque tool calls |
| Interrupt | `interrupt` request, session survives | SIGTERM kills the session |
| Mid-turn steering | Further stdin messages | stdin already closed |
| Model / mode switch | `set_model`, `set_permission_mode` | Only at spawn |
| Context usage | `get_context_usage` | Unavailable |

The gap is not a missing feature. It is that v1 models a **turn** as the unit of
execution, and every agent CLI models a **session**.

## The unit is a session, not a turn

A turn is an event inside a conversation. v1 spawns one process per turn, so
anything the agent learned about *being in a conversation* — pending approvals,
loaded skills, permission decisions, subagent state — is discarded between
turns and rebuilt from a resume id.

v3 keeps a live process for the session and runs turns inside it. The process is
called a **link**, and a session is `attached` (a link is live) or `detached`.

A link is **not** required for a session to work. Links are expensive and do not
survive a server restart, so:

* A turn on a detached session **establishes a link first**, resuming the
  agent's native session by id. This is the ordinary path after any restart, not
  an error path.
* A link idles out (default 30 minutes) and is torn down. History is unaffected;
  the next turn re-establishes.
* Interactive capability is a property of the link, so the UI reports it as
  live state rather than promising it unconditionally.

This is what makes the browser experience match the terminal: while attached,
the same requests and notifications a TUI would render are available; while
detached, the session is still complete, resumable, and searchable.

## Shape

The server never speaks a vendor protocol. An **adapter** does, one per agent,
living where v1's runner lived:

```
server ──sylph.agent.v3 over stdio──► adapter ──vendor protocol──► agent CLI
         (normalized, this document)             (stream-json, JSON-RPC, rpc)
```

Keeping the adapter a separate process rather than a Rust module is deliberate,
and it is the one thing v1 got unambiguously right: adding an agent means
dropping in a directory, with no server build. It also contains a vendor's
protocol churn in a script the user can read and patch, and it keeps a crashing
adapter from taking the server down.

An agent is a directory containing an executable `adapter.sh`. Bundled adapters
ship in the repo, because an adapter is versioned with the protocol it implements
and a protocol change breaks it; third-party ones live in the configured agents
directory, which is what keeps "add an agent by adding a directory" true.

## Framing

NDJSON in both directions on the adapter's stdin and stdout. One JSON object per
line, UTF-8, no embedded newlines. stderr is diagnostics, captured but never
parsed.

Every message carries `t` (type). Requests carry `id`, unique per direction for
the life of the link; responses echo it. Correlation is explicit because both
sides originate requests concurrently, and an implicit ordering would deadlock
the first time both spoke at once.

```json
{"t":"send","id":"c7","text":"fix the failing test"}
```

Three classes, distinguished by who must answer:

| Class | Direction | Answer |
| --- | --- | --- |
| **Command** | server → adapter | An `ok` or `err` echoing `id` |
| **Request** | adapter → server | A `resolve` echoing `id` |
| **Event** | adapter → server | None; fire and forget |

An unknown `t` is ignored with a warning, never fatal. That is what lets a newer
adapter talk to an older server: the protocol grows by adding message types, and
a peer that does not know one behaves as though the sender had not sent it.

## Handshake

The server opens the link, then negotiates before anything can go wrong:

```json
{"t":"hello","id":"h1","protocol":"sylph.agent.v3","server":"sylph-web/0.2.0",
 "accepts":["approval","dialog","elicitation","choice"]}
```

`accepts` is what the *server* can handle. An adapter must not send a request
kind absent from it — a server that cannot render approvals must never be handed
one, because the agent would block forever on a reply that no UI will produce.
This is the one place a capability mismatch could hang a session, so it is
negotiated rather than discovered.

The adapter replies with what it and its CLI can actually do:

```json
{"t":"ok","id":"h1","adapter":"claude/2.0.0","agent":{"name":"claude","version":"2.1.238"},
 "supports":["interrupt","steer","approval","subagent","slash","set_config","context","fork"],
 "flags":{"model":{"choices":["opus-5","sonnet-5"],"description":"..."},
          "effort":{"choices":["low","medium","high","xhigh","max"]}}}
```

`supports` is a closed vocabulary; unknown entries are ignored. It drives the UI
directly, which is why nothing in the frontend hardcodes an agent's abilities —
a composer that offers interrupt to an adapter that cannot interrupt is a lie the
handshake prevents.

`flags` carries the coarse, spawn-time option schema: what can be chosen *before*
a session exists, which is all the create-a-session screen can know. Anything
adjustable on a live session belongs in `config` instead (§ Session config), and
an adapter that can adjust a knob at runtime should declare it in both — `flags`
so it can be chosen at creation, `config` so it can be changed afterwards.

An adapter that declares `approval` in `supports` must be able to raise one. `pi`
is the worked example of the other case: it has no sandbox and no tool-permission
callback, so its adapter declares no `approval`, and the UI must not offer an
approval affordance for a `pi` session. That is what makes `supports` load-bearing
rather than decorative.

## Opening a session

```json
{"t":"open","id":"o1","workdir":"/Users/x/p","sessionId":"<native-id|null>",
 "soul":"/path/to/SOUL.md","flags":{"model":"opus-5"},
 "permissionMode":"ask","env":{"SYLPH_SESSION":"<web-uuid>"}}
```

`sessionId` is the agent-native id to resume, or `null` for a fresh session.
`soul` is a path, or `""` to inject nothing — the empty string is meaningful,
exactly as in v1, and it is how the native-session invariant is enforced from the
server side.

The reply is the session's real capability surface, which is only knowable once
the CLI has loaded the project:

```json
{"t":"ok","id":"o1","sessionId":"abc-123",
 "commands":[{"name":"verify","description":"Verify that a change does what it should",
              "argHint":"[target]","source":"skill","kind":"prompt"},
             {"name":"model","description":"Set the AI model","aliases":[],
              "source":"builtin","kind":"control","configId":"model"}],
 "subagents":[{"name":"Explore","description":"..."}],
 "config":[{"id":"model","name":"Model","category":"model","type":"select",
            "currentValue":"opus[1m]",
            "options":[{"value":"opus[1m]","name":"Opus (1M context)"},
                       {"value":"sonnet","name":"Sonnet 5"}]}],
 "permissionMode":"ask","contextWindow":1000000}
```

`commands` is the agent's own answer to "what can I be asked to do". On a real
project it returns dozens of entries — skills, project commands, plugins — and the
server does not curate it.

## Two kinds of command

```
A prompt command is what the agent thinks about.   →  a turn
A control command is what the session *is*.        →  typed state
```

`kind` is `prompt` or `control`, and **absent means `prompt`** — so an adapter that
says nothing behaves exactly as it did in v2.

A `prompt` command is delivered as a turn (§ Slash commands are turns). Its output
is durable history, it can be interrupted and steered, and it can raise approvals.

A `control` command is **never** a turn. It names a `configId`, and a client
renders the matching config option as a picker; choosing an option sends
`set_config`. This is the difference between `/model` opening a list of models and
`/model` returning a paragraph about them.

`aliases` is carried because agents ship them and a terminal honours them: `claude`
gives `/usage` the aliases `cost` and `stats`, and `/clear` the aliases `reset` and
`new`. A menu that ignores them is a menu that behaves unlike the terminal it is
modelled on.

Which commands are control commands is the **adapter's** judgement. No vendor
supplies the discriminator, and the answer differs per agent: `pi` already excludes
its built-in TUI commands from `get_commands`, so almost everything it reports is a
prompt; `claude` reports its builtins inline, so its adapter must classify them.
Putting that list in the server or the UI would encode one vendor's taxonomy as
though it were universal.

## Session config

Everything adjustable on a live session, in one shape:

```json
{"id":"effort","name":"Reasoning effort","category":"effort","type":"select",
 "currentValue":"medium",
 "options":[{"value":"low","name":"Low"},{"value":"max","name":"Max"}]}

{"id":"autoCompaction","name":"Auto-compaction","category":"compaction",
 "type":"boolean","currentValue":true}
```

`type` is `select` or `boolean`. `category` is advisory grouping — `model`,
`effort`, `permission_mode`, or anything an agent invents — and a client groups by
it without needing to know the vocabulary. The shape is ACP's
`SessionConfigOption`, adopted rather than invented because it already covers
`claude`'s model / effort / permission mode / output style, `pi`'s model / thinking
level / queue modes, `codex`'s approval policy, and `gemini`'s approval mode.

```json
{"t":"set_config","id":"c9","configId":"model","value":"sonnet"}
```

The `ok` reply carries the **refreshed** `config` array, not an acknowledgement,
because setting one option can change another's choices. This is not defensive:
`pi`'s thinking levels depend on the selected model — the current one offers only
`off` and `high` — and `claude` carries `supportedEffortLevels` per model. A design
where effort is one session-global enumeration is wrong for both, which is what
v2's flat `flags.choices` was.

`select` and `boolean` are enough for every knob the four surveyed agents expose. A
numeric or free-text type is a later addition, not a placeholder now.

## Capability facets

A capability update names the facets it speaks about. **A facet that is not
mentioned is not changed.**

```json
{"t":"event","kind":"capabilities","facets":{"commands":[…]}}
{"t":"event","kind":"capabilities","facets":{"config":[…],"permissionMode":"plan"}}
```

Facets: `commands`, `subagents`, `config`, `permissionMode`, `contextWindow`.

This is the one shape v2 got wrong badly enough to need a version. There, a
capability event carried the whole surface and the server assigned it, so an
adapter reporting a permission-mode change also declared that the session had no
commands. Nothing in the wire format said that was happening, and the symptom
appeared two layers away as a menu that would not open.

Merging partials would have fixed the same bug at the same cost, and would have
left the hazard: a message about one part of the surface would still be *able* to
speak for the whole of it, and the next adapter would rediscover that. Naming the
facet makes silence mean silence.

Capability changes are pushed, not polled — a skill edited on disk or a plugin
loading changes what the composer should offer, and a list a client refreshes on
its own schedule is wrong for however long that schedule is.

## Commands the server sends

| `t` | Meaning |
| --- | --- |
| `send` | A user turn: `text`, optional `attachments[]`, optional `command`+`args`, optional `config` overrides |
| `interrupt` | Stop the current turn, keep the session |
| `steer` | Inject text mid-turn without ending it; carries `expectedTurnId` |
| `set_config` | Set one config option by id; `ok` carries the refreshed `config` |
| `resolve` | Answer an adapter request, echoing its `id` |
| `query` | Ask for `context`, `cost`, `diff`, or `plan` |
| `close` | End the session cleanly; adapter flushes and exits |

`send` returns `ok` as soon as the turn is accepted, not when it finishes. Turn
completion is an event, because a turn outlives any request that started it —
the same reason `POST /turns` returns `202`.

**Per-turn config travels with the turn.** `send` carries the overrides that turn
needs rather than the server issuing `set_config` calls before it. Those were three
round trips with a window in between where a turn could run under a mix of old and
new settings, and a failure partway left no single answer to "what did this turn
run as". codex models the same thing the same way, documenting a turn-scoped
override as applying "for this turn and subsequent turns".

**`steer` names the turn it means.** Without that precondition a steer racing a
turn boundary is delivered into the *next* turn — the message arrives, nothing
errors, and it lands in the wrong conversation. codex requires the same field and
fails the request "when it does not match the currently active turn". A mismatch is
an `err`, not a silent redirect.

## Requests the adapter sends

The inversion that v1 could not express: the agent asks, and a human answers.

```json
{"t":"request","id":"r4","kind":"approval","toolUseId":"toolu_01",
 "tool":"Bash","title":"Run a shell command",
 "detail":{"command":"rm -rf build","description":"Clean the build directory"},
 "options":[{"id":"allow","label":"Allow once"},
            {"id":"allow_always","label":"Allow Bash for this session","persistent":true},
            {"id":"deny","label":"Deny"}],
 "default":"deny","risk":"destructive",
 "allowPersistent":true,"expiresAtMs":null}
```

The server journals it, publishes it to every attached client, and blocks
nothing else. A client answers:

```json
{"t":"resolve","id":"r4","option":"allow","input":{"command":"rm -rf build"}}
```

`input` may carry an edited version of what the agent proposed, which is how
"allow, but change the command" works — a real terminal affordance that a
yes/no dialog would drop.

Request kinds: `approval`, `dialog` (free-form question, e.g. Claude's
`request_user_dialog`), `elicitation` (MCP-originated structured input),
`choice`. Each is gated by `accepts` in the handshake.

`allowPersistent` is `false` when a lasting grant must not be offered, and a client
must then omit the "always allow" affordance entirely rather than merely
discouraging it. This is not a style preference: `claude` sets it on asks where
accepting always would write a permission rule *broader than the ask itself*, so
offering it converts a narrow yes into a wide one the user did not read. Absent
means it is allowed, which keeps a minimal adapter correct.

`expiresAtMs` is advisory, and present when the agent will stop waiting. `pi`
auto-resolves a dialog when its own timeout lapses, so a client with no deadline to
show is racing a clock it cannot see — and the user experiences a dialog that
answers itself. The server never auto-applies a default when it lapses; that would
remove the user from the decision this channel exists to give them.

**A pending request is durable state, not UI state.** It is journaled, survives a
client disconnect, and is re-delivered on reconnect — otherwise closing a laptop
lid strands an agent forever. If the link dies with requests outstanding, they
are journaled as `abandoned` so the timeline explains why the turn stopped
instead of showing an unexplained gap.

### Request forms

An approval asks one thing about one proposed input, and the shape above is enough
for it. A *question* is not that: the agent authored the choices, there may be
several questions in one ask, and the answer can legitimately be something the agent
did not offer. A request therefore declares the input it wants collected, in an
optional `form`:

```json
{"t":"request","id":"r7","kind":"choice","title":"Two questions",
 "form":{"fields":[
   {"id":"q:0","type":"select","label":"Indentation?","mode":"one",
    "choices":[{"value":"Tabs","description":"Tab characters"},
               {"value":"Spaces","description":"Space characters"}],
    "allowOther":true,"notes":{"label":"Notes (optional)"}},
   {"id":"q:1","type":"select","label":"Linters?","mode":"many",
    "choices":[{"value":"ESLint"},{"value":"Prettier"},{"value":"Clippy"}]}]},
 "options":[{"id":"submit","label":"Submit"},
            {"id":"deny","label":"Don't answer"}]}
```

```json
{"t":"resolve","id":"r7","option":"submit",
 "input":{"values":{"q:0":"Tabs","q:1":["ESLint","Prettier"]},
          "notes":{"q:0":"only for generated files"}}}
```

Field types are `select` (with `mode` `one` or `many`, and `choices` carrying the
agent's own `description` — on a question the description is most of what makes the
options readable) and `text` (with `multiline`, `prefill`, `placeholder`). A field
may declare `required`, `allowOther` — permitting a value outside `choices`, which is
a real answer rather than a note — and `notes`, a free-text annotation attached to
the pick rather than replacing it.

**A form is declared, never inferred.** Before this existed, a client guessed what
to collect by sniffing `detail` for field names it recognized (`command`,
`file_path`, `url`, …). A question's detail carries none of those, so a `choice`
rendered as buttons with nowhere to type and an `elicitation` — whose entire purpose
is free text — rendered as a read-only dump of its own placeholder. Guessing cannot
be made to work: only the agent knows what it is asking for. **Absent means the
older behaviour**, which keeps a minimal adapter and every existing approval correct.

**`options` are verbs; fields carry data.** A `choice` previously overloaded
`options` as both the answer and the submit button, which is why picking one had to
submit immediately and why a note had nowhere to live. With a form present, an option
is what to *do* with the answer — submit it, or decline to give one.

The server validates `input` against the stored form before claiming the request:
an unknown field id, a `one` field given an array, a value outside `choices` where
`allowOther` is not set, or a missing `required` field is refused. This is the same
rule that governs `options` and `set_config` values, and for the same reason — the
client renders whatever an agent it has never seen reported, so the server is what
keeps a malformed answer from reaching a vendor CLI that will do something undefined
with it.

**Answering completely is not cosmetic.** Measured against `claude` 2.1.238: a reply
carrying every question's answer is delivered to the model as *"Your questions have
been answered … You can now continue with these answers in mind"*, while a reply
that answers one of two arrives as the weaker *"The user answered …"*. Forwarding
only the first question of a multi-question ask therefore degrades the answer the
model acts on, which is why one form carries N fields rather than one question.


## Events the adapter sends

The event kinds are `thinking`, `assistant`, `tool_call`, `tool_result`, `usage`,
`status`, with phases `started` / `delta` / `completed`. Beyond those:

| `kind` | Carries |
| --- | --- |
| `subagent` | Declares a delegated trajectory; its `streamId` is that trajectory's id. Any event's `span` may also promote its referent to one (§ Delegated trajectories) |
| `plan` | A proposed plan, so plan mode is reviewable rather than prose |
| `diff` | A structured patch, so a file change renders as a diff not a tool log |
| `compact` | A compaction boundary, so the timeline shows where context was condensed |
| `turn` | `started` / `completed` with usage, cost, duration, stop reason |
| `capabilities` | `facets`, applied per facet (§ Capability facets) |
| `notice` | Agent-originated notification worth surfacing |

`subagent` is the one that changes what the UI can be, and it needs its own
section because it is the only event kind that describes *structure* rather than
content.

## Delegated trajectories

A turn is not a sequence. An agent delegates, its delegates delegate, and a
dynamic workflow fans out to a dozen agents at once and then synthesizes. The
event stream has to carry that shape, because a client cannot recover it.

**One field carries all of it.** Every event may name the trajectory it belongs
to:

```
span   the trajectory this event happened in. Absent means the root turn.
```

A `subagent` event *is* the trajectory's own record: its `streamId` is the
trajectory id, and its `span` names the trajectory it was delegated *from*.
A `subagent` event *declares* a trajectory, but § Promotion below is what
*defines* one — declaration is the richer path, not the required one.
Everything else follows:

```json
{"t":"event","kind":"subagent","phase":"started","streamId":"s1","name":"Explore"}
{"t":"event","kind":"thinking","phase":"delta","streamId":"b3","span":"s1","text":"…"}
{"t":"event","kind":"subagent","phase":"started","streamId":"s2","span":"s1","name":"Verify"}
{"t":"event","kind":"tool_call","phase":"completed","streamId":"t9","span":"s2","name":"Bash"}
{"t":"event","kind":"subagent","phase":"completed","streamId":"s2","span":"s1"}
{"t":"event","kind":"subagent","phase":"completed","streamId":"s1"}
```

`s2` is a child of `s1` because it carries `span:"s1"`, by exactly the same rule
that puts the `thinking` block there. Depth is unbounded, siblings may run
concurrently, and a workflow phase is simply a trajectory whose children are
trajectories. No case in the protocol distinguishes any of these, which is what
makes it a generalization rather than three features.

**Why a pointer and not nesting.** The table above once said the child's events
arrived *nested inside* the parent event. That cannot be implemented: an envelope
has to be closed before it is sent, and a trajectory streams for minutes. It also
contradicts the journal, where an event is a row — a nested payload would make one
row hold a subtree, so a delta inside it could not be addressed, deduplicated, or
pruned. A flat stream of events carrying a parent pointer is what distributed
tracing settled on for the same reason, and it is already how the vendor speaks:
`claude` tags a subagent's output with `parent_tool_use_id` on the envelope.

**`span` is not `streamId`.** `streamId` identifies one block across its phases;
`span` identifies the trajectory a block ran in. A `subagent` event is the one
place they meet, because opening a trajectory is itself a block with a lifecycle.

### Promotion: a trajectory is defined by use, not by declaration

**Any `streamId` that another event names as its `span` is a trajectory**,
whatever kind of event opened it. A `tool_call` so named is *promoted*: it stays
a tool call, and it also becomes the container its children already claim it is.

This is a definition and not a fallback, because delegation is only knowable
*retroactively*. An adapter learns that a tool call delegated when a child event
arrives carrying its id — and by then the `tool_call` event has been sent and
cannot be recalled. Anything that decides at open time is therefore a guess
standing in for a fact that has not arrived. `claude`'s adapter guessed by tool
name (`Task`), which was correct until the vendor shipped `Agent` and `Workflow`;
an MCP server can add another at any time, so the whitelist could not have been
completed, only extended after each report. Promotion needs no list: an agent
nobody has written yet nests correctly by tagging its children, which is the same
`span` it must already set for its output to be attributed at all.

Declaration keeps a purpose it does not share with promotion. A `subagent` event
carries a *name* before any child exists, and states an *outcome* independent of
its host's — so an adapter that knows it is delegating should still declare, and
one that only discovers it in hindsight is no longer silently wrong. Both produce
the same tree.

**A promoted trajectory closes with its host.** A `tool_call`'s `completed` phase
means its input finished streaming, not that the delegated work finished — the
`tool_result` sharing its `streamId` is the close, and its `isError` is the
outcome. A client must not read a promoted node's lifecycle off its phase, or
every delegation reads as finished the moment its arguments are parsed.

**One host may run many delegates, so a trajectory id must be the delegate's.**
`claude`'s `Workflow` fans out to N agents under a single `tool_use_id`; keying a
trajectory by the host collapses all N into one row, where the last name to arrive
wins and every earlier delegate's identity is lost. Measured: a two-agent workflow
rendered as one card reading `Demo: agent-2`. An id must come from whatever the
vendor gives *per delegate* — `task_id` for `claude` — with the host named as its
`span`. This is the same rule as *What the adapter must keep separate* below, one
level up: never key a trajectory by something the vendor multiplexes.

**A vendor tag coarser than a trajectory is not refinable by guessing, and needs no
new protocol field.** The question a multiplexing host raises is where its *forwarded
interior* belongs, and it is tempting to answer it with a new envelope field. It does
not need one: `span` is already per-event and can name any trajectory at any depth.
The missing correlation is entirely vendor-side — `claude` tags forwarded output with
`parent_tool_use_id`, the *host*, which is precisely the id it multiplexes. So when a
host has N delegates the vendor's own tag cannot say which one produced a given event,
and an adapter must attribute that output to the **host** — the container, which is
true — rather than pick a delegate, which is corruption indistinguishable from correct
output. Only N = 1 is safe, because there the sole delegate is the only thing the
interior can be.

Two obligations follow, and both were real defects rather than hypotheses:

* **A re-label must be latched.** The table an adapter consults grows while the turn
  runs, so the same host answers "the host" before its `task_started` and "the sole
  delegate" after it, while the forwarded stream does not pause in between. Answered
  freshly per event, one delegate's output is split across the container and the
  delegate at whatever instant the notification landed — and both halves render,
  nested one inside the other, so nothing reports the split.
* **Vendor block state must be keyed by the vendor's own tag, never by the
  trajectory it resolves to.** The tag is the only part guaranteed identical across a
  block's `start`, `delta` and `stop`. Keyed by the resolution, a notification landing
  mid-block moves the key out from under an open block: the delta and the stop both
  miss, so the block's text is dropped and no `completed` is ever emitted.

Measured on `claude` 2.1.245: no host both forwards interior events and multiplexes.
`--forward-subagent-text` forwards only for the built-in `Task`, whose notifications
reuse the spawning id and so never make it multiplexing; `Agent` and `Workflow`
multiplex and forward nothing. The N = 1 re-label is therefore a guard against a
vendor change, not a path in use — which is exactly the shape the promotion rule above
exists to make safe.

**Progress is a snapshot, so it is not delta text.** A vendor's progress
notification re-sends its whole current summary rather than a suffix, so appending
each one repeats it — measured at six copies of one sentence. A `subagent` event
carries such a summary as `data.progress`, which replaces, leaving `text`
append-only as everywhere else in the protocol.

**A trajectory with no interior says so.** A vendor may report that delegated work
exists without forwarding any of it — `claude` forwards interior events for its
built-in delegation and only lifecycle notifications for the rest. A *closed*
trajectory with no children is therefore a real state and not a loading one, and a
client must distinguish it from a running trajectory that has not reported yet.
Rendering both as an empty expandable card invites the user to keep opening a
branch that will never fill.

**A `subagent` event is always stable, at every phase.** Ordinarily `started` and
`delta` are ephemeral projections that never reach the journal (§ `docs/storage.md`).
That rule is right for content and wrong for structure: the `started` phase is
where a trajectory's identity, name, and parent live, so dropping it would leave
every child event in the journal pointing at a parent that was never recorded.
After a reload the whole delegated subtree would flatten into the parent's own work
— which is precisely the pre-`span` bug, reintroduced by the storage tier instead
of by the protocol.

**An unknown `span` is not an error.** A client that receives an event naming a
trajectory it has not seen renders it at the root rather than dropping it. The
alternative loses work in exactly the case where structure is least reliable: a
link that died mid-delegation, or an adapter emitting out of order.

**A trajectory has its own outcome.** `phase:"completed"` with `isError` closes it,
so a delegation that failed three levels down is reportable at every level above
it without a client inferring failure from the absence of a result. An adapter that
loses a trajectory — its link died, its vendor stopped reporting — leaves it open,
and the server closes open trajectories when the turn terminates, for the same
reason a pending request becomes `abandoned`: an unexplained gap is worse than a
recorded failure.

**A trajectory id is session-scoped, not turn-scoped.** Two rules follow, and
without them a delegation that outlives its turn cannot be described at all.

*A turn closes only what it holds.* The turn-settle sweep resolves a trajectory
only when some event on that turn carries it as its `streamId` — a declaring
`subagent` row, or a `tool_call` its own children name (§ Promotion). A bare `span`
naming something this turn never emitted is a reference outward, most often to a
trajectory an earlier turn opened. Treating such a reference as an opening
fabricates a record: measured, a `Workflow` that had completed cleanly on the
dispatching turn acquired a second `abandoned` close on the agent's later turn, for
work it never ran.

*A late outcome is recorded on the turn that owns the trajectory.* Backgrounded
work reports back after its turn has settled (§ An agent may start a turn), so its
`completed` arrives on a different turn than its `started`. The event stays where it
arrived — that is when it happened, and a client watching that turn should see it —
and the server additionally corrects the owning turn's record, which is what keeps
the two from contradicting. Only a provisional record may be corrected: a trajectory
still open, or a close the *server* authored. An outcome the agent itself stated is
never rewritten, because a later mention is a description and not a re-decision.

This is what makes `abandoned` genuinely non-final rather than merely documented as
such. It is also what makes skipping the sweep for backgrounded work safe: on its own
that leaves no record on the dispatching turn at all, which trades a wrong answer for
no answer.

**What the adapter must keep separate.** Vendor block state has to be keyed by
trajectory, not by the vendor's own block index. `claude` numbers content blocks
per message, and two concurrent subagents both start at index 0 — an adapter with
one index-keyed map merges their deltas into one another's text. That is data
corruption, not a display fault, and it is invisible until two delegates run at
once.

## An agent may start a turn

A turn is not always a reply. An adapter may emit events with no turn in flight, and
the server opens a turn for them rather than dropping them — an **agent-initiated**
turn, recorded with `origin: "agent"` and an empty request text.

This is not hypothetical and not rare. Measured on `claude` 2.x, a session holding
backgrounded work emits, *after* the dispatching turn's `result`:

```
background_tasks_changed        the set of live background tasks changed
task_updated                    patch{status: completed}
task_notification               status, and the delegate's own summary
init                            …and then an entire exchange of its own
assistant                       "the background worker finished"
result
```

Its stdout stays open and the process does not exit. So the agent reports late work,
and reports it twice over: the delegate's outcome, then its own account of it.

**Why the events cannot simply be dropped.** They were, and the reasoning was sound
as far as it went: an event carries no turn id, the server files it under whichever
turn is running, and a *guessed* id misplaces it in the transcript permanently. The
error was concluding that no turn means nowhere to belong. The answer is to open the
turn that does exist rather than to guess at one that does not — otherwise the only
observable effect is that a delegation promised to report back never does, with
nothing anywhere saying why.

**Why a turn and not a lesser thing.** A turn is the unit that orders, consolidates,
and prunes (`docs/storage.md`): `turn_id` is what consolidation iterates and what
`events` is indexed by. Attaching these to no turn would leave rows the journal never
folds and never releases — a leak whose size is set by how much unprompted work an
agent does. Making them a turn costs one row and inherits every guarantee.

**When the turn opens.** On the *first* out-of-turn event of any kind, not on a
`turn`/`started`. The vendor's own bracket arrives partway through the sequence above
— `task_notification`, the payload this exists to deliver, precedes it — so waiting
for the bracket keeps the announcement and drops what it announces. A later
`turn`/`started` then lands inside the already-open turn instead of opening a second.

**How it settles.** By its own `turn`/`completed`, like any turn. Nothing is parked on
its outcome, since nobody asked for it, so the server records terminal state directly
rather than waking a waiter. It must settle: a session with a live turn refuses new
messages, so one unprompted turn left running would wedge the session.

**A client has to say who started it.** `requestText` is empty by construction, which
is indistinguishable from a user who sent nothing — so an unlabelled agent turn reads
as an answer to a message the user never wrote. `origin` is the only thing that
carries this.

## Slash commands are turns

A **prompt** command is delivered as `send` with `command` set:

```json
{"t":"send","id":"c9","command":"verify","args":{"target":"the auth change"},"text":""}
```

The adapter is free to implement this as literal prompt text (`/verify ...`),
because for most CLIs that is precisely what a slash command is. Modelling it as
a *kind of turn* rather than a separate operation is what keeps it in the
transcript: a command produces normal events, appears in history, and is
searchable. v1's `--command` mode returned an ephemeral status line that was
shown once and lost.

Two consequences worth stating, because both were wrong in v1:

* A command's output is durable history, not a toast.
* A command can be interrupted, steered, and can raise approvals, because it is
  a turn like any other.

A **control** command is not sent at all. It resolves to a config option, the
client renders a picker, and the chosen value goes back as `set_config`. v2
applied the paragraph above to every command uniformly, and that is precisely what
made `/model` answer with prose: the CLI was asked a question in a channel that has
no picker, and it did the only sensible thing.

## v1 is not supported

There is no degraded path. `runner.sh` is not discovered, `SYLPH_STREAM_FORMAT`
has no meaning, and an agent directory without `adapter.sh` is simply not an
agent.

This project has no external consumers, so a compatibility shim would buy
nothing and cost the thing that matters most: a single shape for what an agent
is. Keeping v1 would have meant every layer above the seam carrying two notions
of a session — one where a turn is a process and one where it is a message — and
that duality does not stay contained. It shows up in the supervisor, in the API's
capability reporting, in the UI's decision about whether to offer interrupt, and
in every test that has to cover both.

The Discord daemon does run on v1 today. It is migrated to this server rather
than kept on the old contract — see `docs/control-plane.md`. That is more work
than a shim and it is the work that leaves one control plane instead of two.

An agent whose CLI genuinely has no duplex mode is still supportable: its adapter
holds the session and spawns a one-shot CLI invocation per turn internally. The
one-shot-ness becomes a vendor detail inside one adapter, which is exactly where
vendor details belong, and the adapter declares a smaller `supports` list. The
protocol does not need to know.

## What the adapter owns, and what it must not

Unchanged from v1 in principle, and worth restating because v2 gives the adapter
more rope:

The adapter owns vendor translation: how its CLI accepts a system prompt, spells
a resume id, streams output, and represents a permission request. It owns
injecting `SOUL.md` through an **additive** channel, never a replacing one.

The adapter owns no policy. It does not decide whether to inject identity (the
server sends `soul` or `""`), when a session resets, what a permission answer
should be, or what is worth journaling. A pending request must never be
auto-answered on the server's behalf — an adapter that helpfully approves a tool
call has silently removed the user from a decision the protocol exists to give
them.

## Failure

| Failure | Handling |
| --- | --- |
| Adapter exits mid-turn | Turn `failed`, stderr tail attached, pending requests `abandoned`, session detached |
| Adapter stops reading stdin | Command times out (30s), link torn down |
| Malformed line | Warned and skipped; a bad line is never fatal |
| Handshake timeout (10s) | Adapter marked broken, excluded from listings, reason recorded |
| Server restart | Every link is gone; sessions detached, in-flight turns `interrupted` |
| Request unanswered | Stays pending; it is real state, and only the user or a link teardown resolves it |

A link's death is never silent. The session records why it detached, because
"the agent stopped responding" and "the agent finished" look identical in a
transcript that does not say.



