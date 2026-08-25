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
| `subagent` | `parentToolUseId`, the child's own events nested |
| `plan` | A proposed plan, so plan mode is reviewable rather than prose |
| `diff` | A structured patch, so a file change renders as a diff not a tool log |
| `compact` | A compaction boundary, so the timeline shows where context was condensed |
| `turn` | `started` / `completed` with usage, cost, duration, stop reason |
| `capabilities` | `facets`, applied per facet (§ Capability facets) |
| `notice` | Agent-originated notification worth surfacing |

`subagent` is the one that changes what the UI can be. A subagent's trajectory
arrives tagged with its parent tool call, so the timeline can nest it — which is
what the terminal does, and what TraceDB already records as lineage. v1 saw only
an opaque `Task` tool call with a wall of text.

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



