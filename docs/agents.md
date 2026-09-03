# Agents

One module per vendor, holding everything Sylph knows about that CLI: how to launch
it, how to read its native store, what it exposes to a second party, and where it
lies.

## Three agents

Supported: **`claude`**, **`codex`**, **`pi`**. `gemini` and `opencode` are **not
supported**, and that is a deliberate narrowing rather than a gap waiting to be filled.

Two requirements decided it, and both are worth stating because they apply to any agent
proposed later:

- **A readable per-session native store.** An agent that keeps every session in one
  database with a live WAL cannot be watched by file mtime, has no per-session bytes to
  point at, and cannot produce a per-session fingerprint; reading such a store wholesale
  is unbounded work that starves every other agent's pass. Nothing constrains an agent to
  keep a per-session file, so this has to be checked rather than assumed.
- **Known mid-turn write behaviour.** Observation is only offerable if it is known *when*
  a vendor flushes. Unknown is not the same as absent, and neither can be promised to a
  user as "observable".

An agent is available on an environment when its CLI is on that machine's `PATH`. That is
the whole check — there is no discovery handshake and no adapter directory.

## What every agent module must answer

| Question | Why it cannot be generic |
| --- | --- |
| launch argv, per mode | each CLI spells resume, session id, model, and effort differently |
| native store location and format | JSONL per session, or a store with its own row ids |
| does it accept a dictated session id | decides whether Sylph knows identity before the first event |
| mid-turn flush granularity | decides whether `watch` sees a turn in progress or only its end |
| second-party control surface | decides whether the control facet exists at all |
| which vendor keys carry what | the most-repeated defect class in this repo |

## Capability matrix

*Measured against `claude` 2.1.247 and `pi` 0.84.3 on 2026-08-27, and `codex-cli`
0.150.1 on 2026-08-28. Every row names a version, because a measurement without one
goes stale invisibly.*

| | `claude` | `codex` | `pi` |
| --- | --- | --- | --- |
| dictate session id | `--session-id <uuid>` | assigns its own, reports it | `--session-id <id>`, creates if missing |
| resume | `-r <id>` | `resume <id>`, `exec resume`, `thread/resume` | `--session-id <id>` |
| fork | — | `exec fork <id>`, `thread/fork` | — |
| native store | JSONL per session | `rollout-*.jsonl` | JSONL per session |
| mid-turn flush | tool-call and content-block boundaries | item completion | every `message_end` |
| second-party control | authenticated per-process UDS — **delivers, cannot drive** | app-server: **fully drivable**, incl. interrupt and steer | **none** — stdio only |
| second-party observe | — | explicit `thread/resume`; independent of control | — |
| forwards delegate interior | only for built-in `Task` | **yes**, with the child's `threadId` | no delegation on the wire at all |
| slash commands | yes, project-dependent | no command concept | yes |
| typed config | model, effort, permission mode, output style | model, effort, approval policy, personality, sandbox, cwd | model, thinking level, queue modes |

Three things to read off this table rather than from prose. The control column is why
`docs/architecture.md` makes control a facet that can report unavailable with a reason:
`codex` is fully drivable by a second party, `pi` is not drivable at all, and `claude`
is in between — its socket delivers a message but cannot steer a turn. The flush column
is why `watch` is offerable at all: all three write mid-turn, so an observer sees
progress rather than only outcomes.

The third is the observe row, and it is why the two facets are **separate** rather than
one "reachable" flag. `codex` splits them natively — a second client must call
`thread/resume` to receive events, and can drive a turn without ever subscribing. So
observation and control are independently available in a real vendor, not just in
Sylph's model of one.

## `claude`

**Launch.** `--session-id` to dictate, `-r` to resume,
`--append-system-prompt-file` for identity injection (additive — the agent's own rules
and the project's `CLAUDE.md` must survive), `--model`, `--effort`.

**`--permission-prompt-tool` must be registered unconditionally.** It can only be
asked for on the command line while the mode it serves changes at runtime, so gating
it on the launch mode loses the approval channel permanently for anyone who switches.
`docs/session-protocol.md` has the measurement.

**`bypassPermissions` is refused** unless launched with
`--dangerously-skip-permissions` or `--allow-dangerously-skip-permissions`. The latter
marks the mode *available* without starting in it, which is what a user selecting it
later actually asked for. And `manual` answers **success while confirming `default`** —
the vendor normalizes silently, so a picker offering it lands on a different row.

**Model catalogue entries are `{value, displayName, …}`, not `{id, …}`.** Reading `id`
produced an empty model list on every session ever opened, presenting as a composer
bug.

**`system/init` arrives once per turn, not once per process.** A whole-surface
capability payload answering it erases everything it does not mention; facets exist
for this.

**Delegation notifications.** The real subtypes are `task_started`, `task_progress`,
`task_updated`, `task_notification`. `task_completed` and `task_failed` **do not
exist** — branching on them compiled, never fired, and left every delegate spinning
forever. A close arrives either as `task_updated` with `patch.status`, or as
`task_notification` with `status` plus the delegate's summary. `task_updated` carries
**no `tool_use_id`**, so the host must be remembered from `task_started` or the close
lands at the root while the trajectory it closes sits inside a container.

**`task_type` is the only thing distinguishing a delegation from a backgrounded shell
job.** `local_agent` is a real subagent; `local_bash` is `run_in_background` on Bash
reporting through the same channel. Neither `task_updated` nor `task_notification`
carries the field, so it must be remembered from `task_started`. Classifying by the
notification alone published a *shell command* as a subagent — a card named `find /
-maxdepth 8 …` whose error flag escalated the Bash call hosting it, so a turn that had
done nothing wrong read `failed` from its first moment. **Filter narrowly and let an
unknown type through**: guessing wrong that way costs a missing subtree, while the
other way mislabels a tool call as an agent.

**`--forward-subagent-text` forwards only for the built-in `Task`.** `Agent` and
`Workflow` forward nothing on stdout, so their interior must come from the native bundle;
the `Task` delegate file is the overlapping copy and is suppressed only after its host tool
is known.

**Claude's native bundle is read by one cursor-driven reader in every mode.** `adopt`,
`watch`, and live attachment share root/delegate/journal/manifest traversal and one
projection. JSONL cursors advance only past complete lines; manifests are
content-fingerprinted snapshots; sidecars have three source states: `missing`, `partial`,
and `ready`. A partial sidecar is never treated as valid metadata, and a missing sidecar
is not an eternal `pending` state: native launch/result records or task notifications are
used when present, otherwise live close releases the delegate at the root. No timeout
decides source meaning.

**A resumed live reader captures its baseline before process launch.** Capturing after
`system/init` can skip a delegate created between spawn and identity discovery. Existing
bytes are hydrated into correlation state without re-emission; only bytes after the
pre-launch boundary are projected live.

**Workflow manifests may arrive after delegate files.** Delegate content is retained under
its trajectory id while the parent-bearing declaration waits for the manifest's `runId`
join. When the manifest arrives, the declaration is emitted once with the measured parent,
so watch, live, and a later reload converge on the same tree without a retraction API.

**`is_backgrounded` is reported on `task_started` and nowhere later**, so it must be
re-attached to every subsequent phase.

**A refusal has no `response` member** — `subtype: "error"` is the discriminant. Read
it before the payload.

**The socket delivers messages; it cannot drive.** A per-process authenticated UDS at
`$XDG_RUNTIME_DIR/cc-socks/<pid>.sock`, falling back to `/tmp/cc-socks-<uid>/`, with a
messaging token; live/dead probe in 250 ms. Its accepted frames are all
conversation-ownership coordination (`auth`, `user`, `peer_message_status`,
`notify_when_idle`, …). `set_model`, `set_permission_mode`, `stop_task`, and interrupt
live ~2 MB away in the SDK/WebSocket control table and travel over a websocket.
Byte-offset proximity in a minified bundle is strong evidence, not proof.

**Inbound `user` frames are gated on the sitting human's approval** — the binary
carries "held for the recipient user's approval", declined, and expired strings, plus
`Routed user message to queue (priority=`. So even an approved message is queued rather
than synchronous. **Whether the gate applies to every inbound frame or only to
peer-session senders is unresolved, and it is the highest-value unknown here**: if
every frame is held, remote continuation of a terminal `claude` session is worthless
precisely when AFK matters, because nobody is there to approve.

**Never connect or write to a socket in the user's real `cc-socks` directory.** That
injects a turn into a live session of theirs. Probe only a throwaway child's own pid.

**It records its own title for most sessions but not all**, so any surface showing one
needs the opening-ask fallback.

**In the native store, a tool result's id and its error flag are both on the content
block — nowhere else.** These are the two shapes a parser for this store must be built
around, and both are stated as shapes rather than counts on purpose: a developer's session
store grows while you measure it, so a ratio taken today is not re-derivable, while "this
key is on that block and on no other" stays true.

`tool_use_id` sits on the `tool_result` block inside `message.content[]`, on every such
block. It is *not* on `toolUseResult` under either spelling and not at the top level of
the record — those keys occur nowhere in the store. A parser reading them pairs nothing:
every tool result gets a null call id, and the pairing `docs/session-protocol.md`
§ Keying rules depends on silently does not exist.

`is_error` is on that same block and **only** on records whose `message.role` is `user`;
it never appears on an assistant message. A parser reading it from the assistant branch
therefore classifies **every real tool failure as a success**, because `false` and
"absent, defaulted to false" are indistinguishable downstream.

Note what both failures have in common, since it is the whole risk in reading a vendor
store: neither raises an error. The parser completes over the entire store and produces
plausible records. So assert on *values a parser should have found* — that a failure parses
as a failure — never on the absence of a crash.

## `codex`

*Measured against `codex-cli` 0.150.1. Capture scripts and raw frames for this section
live in the parser-port measurement set, and each numbered `sNN` probe is re-runnable.*

**The strongest second-party surface of the three, and the reason the control facet is
worth having.** Its app-server exposes a large generated schema of client request
methods — among them `turn/interrupt`, `turn/steer`, `thread/settings/update`,
`thread/fork`, `thread/resume`, `thread/unsubscribe`, and `config/value/write`. Interrupt
and steer were both measured driving a live turn, so **a codex `control` facet declares
itself full**, not queue-only.

Count such a union from its own `oneOf` rather than by grepping for method-name patterns:
names that do not match the pattern are missed silently, which is the same class of error
as reading a vendor key by pattern rather than by structure.

`turn/steer` requires an `expectedTurnId` and refuses a stale one (`expected active turn
id X but found Y`), which is the mid-turn precondition Sylph's own protocol asks for.
`thread/queue/add` additionally requires an undocumented `clientUserMessageId`.

**`remote-control` is not the door an envagent uses.** It is gated twice — `remote control
requires ChatGPT authentication; API key auth is not supported`, plus a managed standalone
install — while `app-server --listen unix://PATH` has neither gate. Its pairing flow is
still the shape `docs/environments.md` models itself on; it is simply not the transport.

**Observation requires an explicit subscribe, and control does not.** A second client
attached to the same daemon sees **zero** `item/*` events until it calls `thread/resume`
— including for a turn it started itself. After resuming, its event stream is identical
to the first client's; `thread/unsubscribe` returns it to silence **while leaving control
working**, and an unsubscribed client was measured interrupting the other's turn. That
asymmetry is `docs/architecture.md`'s two facets implemented natively. A thread with no
completed turn is unresumable (`no rollout found for thread id`), so observation depends
on a persisted rollout.

**Approvals fan out to every subscriber, arrival *and* departure.** A
`item/commandExecution/requestApproval` reaches every attached client, and
`serverRequest/resolved` is published to all of them when one answers. That is the shape
`docs/session-protocol.md` § Announce every departure requires of Sylph, implemented here
by a vendor.

**An out-of-enum approval decision is accepted and silently denies the command.** The
enum is `accept` | `acceptForSession` | `decline` | `cancel`. A plausible guess outside it
— `"approved"` — returns **no error at any layer**: the JSON-RPC reply is accepted, the
turn reports `completed`, and only the command's own `status` two levels down distinguishes
it from a real approval, having never run. So an adapter that guesses this token ships an
approval channel that refuses *everything* while every observable layer reports success.
**Read such an enum from the vendor's schema; never infer the word from what it obviously
ought to be.**

**It assigns its own session id and reports it**, so Sylph keys on what came back
rather than dictating. `exec fork <id>` exists, which is the vendor implementing the
fork relationship `docs/architecture.md` uses for moving work between environments.

**No command concept at all**, so the `commands` facet is empty and a client must not
present an empty slash menu as a failure. Its typed config is approval policy.

**Native store is `rollout-*.jsonl`, flushed at item completion**, with
`token_count` ticks proving mid-turn append.

**The record vocabulary has two eras, and the discriminant is `history_mode`.** In the
newer, paginated era `event_msg/item_completed` carries the turn's content keyed by
`item.type` — Reasoning, CommandExecution, AgentMessage, FileChange, UserMessage,
SubAgentActivity, McpToolCall, ContextCompaction, ImageView, CollabAgentToolCall, roughly
in that order of frequency. A parser that drops `item_completed` reads almost nothing from
a paginated rollout.

**Do not discriminate on `cli_version`.** One CLI version appears in both eras, and the
store holds files written by the *newest* version present in the legacy shape — so a
version comparison picks the wrong parser on a file the vendor is still producing today.
The discriminant is `session_meta.payload.history_mode`. It is absent on the majority of
files, all of which predate the field and are legacy, so **absence reads as legacy rather
than as an error**.

**The two content streams are mutually exclusive per file.** "Both must be read" is true
of the *store* and false of a *record*: nearly every file holds `item_completed` or
`event_msg/agent_message`, not both, and reading both in a paginated file
**double-counts every assistant message**.

A small set of the oldest files does hold both, and the obvious dedup fails on them: the
pair carries the same text with matching `msg_…` ids but differs by a millisecond or two
*and* by block type (`input_text` against `text`), so a key of `sha(text):timestamp`
cannot match them. **Choose one stream per file from the discriminant** rather than
discovering the overlap per record.

**`CommandExecution` is not redundant with the tool call, and cannot be dropped as a
duplicate.** It is the only carrier of `exit_code`, `status`, and `duration`, it uses a
disjoint `exec-` id space, and it carries **no** `call_id` — so it cannot be paired to
the `function_call` it completes by id at all. Neither stream can simply be preferred
over the other.

**A subagent may record only a parent id with no id-suffix marker.** Measured from its
own store: a rollout with `parent_thread_id` set and `thread_source: subagent`. So
lineage cannot be recovered from an id-naming convention alone.

**Lineage is nested, and reading only the flat key finds almost none of it.** The
authoritative spelling is the nested
`session_meta.source.subagent.thread_spawn.parent_thread_id`; a top-level
`parent_thread_id` exists but is rare. A parser written only to the flat key finds almost
nothing while erroring on nothing — read the nested path, and treat the flat one as a
supplement.

**`forked_from_id` alone does not mean "fork".** On 0.150.1 a subagent carries it too,
equal to its `parent_thread_id`. The discriminant for a real fork is `parentThreadId:
null` with `forkedFromId` set. Where both a parent and a fork origin exist, the parent
wins (`docs/architecture.md` § Provenance is not identity).

**`SubAgentActivity` names a host call id and a child thread id in one record**, which
makes codex the one agent of the three whose native store carries real delegation
structure. That is what `span` can honestly be built from here; see § Reading a vendor
payload for why a parser-synthesized pairing is not.

**codex forwards a delegate's interior, and this is the opposite of `claude`.** A real
`spawn_agent` delegation emitted `subAgentActivity` and `collabAgentToolCall`, **and the
delegate's own `commandExecution` and `agentMessage` items arrived unprompted, carrying
the child's `threadId`** — so a codex delegation card may claim a real, fillable interior.

Three properties of that structure decide how it may be read, and each is a branch an
adapter author would otherwise get wrong:

- **`kind` is `started` or `interacted` only.** `interrupted` and `completed` are not
  emitted, so **a codex delegation has no close record in the store** and a parsed
  delegation's end has to come from the turn containing it. This is the declared-only-event
  trap in its usual form (§ Reading a vendor payload).
- **`agent_thread_id` is the *other party*, not the child.** In a parent's rollout it names
  the child; in the child's own rollout an `interacted` record names the **parent**
  (`agent_path: "/root"`). Mapping it to `span` unconditionally inverts the edge for every
  child-side record, producing a tree that is backwards rather than merely incomplete — and
  nothing downstream can detect that.
- **`collabAgentToolCall` is not lineage.** It carries empty `receiver_thread_ids` and
  `agents_states` with `tool: "wait"`, and its `status` is the *wait's* rather than any
  delegate's. It marks a barrier.

**The `spawn_agent` brief is ciphertext in current versions**, so a delegation's name is
unrecoverable from the parent's own record. `docs/session-protocol.md` § "`name` is absent
when unknown" governs: omit the name, because the journal makes every guess permanent and a
row written with a placeholder is afterwards indistinguishable from one that carried a real
name.

**Do not carry `claude`'s conclusion across.** There, a childless trajectory is a real
terminal state and the card must say so; here it is a loading state, because codex does
forward a delegate's interior. The two agents need opposite defaults, which is exactly why
`fidelity` and the capability facets are per-record rather than global.

**There are three codex event vocabularies for the same concepts, and they are not
interchangeable.** An adapter reading one while mapping names learned from another gets
nothing, silently.

| Source | Spelling | Example |
| --- | --- | --- |
| app-server | slash + camelCase | `item/completed`, `commandExecution` |
| `exec --json` | dot + snake_case | `item.completed`, `command_execution` |
| rollout JSONL | `type` / `payload` | `response_item` / `custom_tool_call` |

Mapping the app-server's names onto Sylph's vocabulary: `reasoning`→`thinking`,
`agentMessage`→`assistant`, `commandExecution`→ **both** `tool_call` and `tool_result`
(one item carries the call and its outcome), `turn/started|completed`→`turn`,
`thread/tokenUsage/updated`→`usage`, `thread/status/changed`→`status`,
`contextCompaction`→`compact`.

With no Sylph equivalent: `webSearch`, `imageGeneration`, `imageView`, `sleep`,
`enteredReviewMode`/`exitedReviewMode`, `hookPrompt`, and `plan` — which Sylph reserves but
does not implement (`docs/session-protocol.md` § Events). Only a minority of the declared
`ThreadItem` variants and server-request kinds appear in real turns, which is § Reading a
vendor payload's declared-only trap again: **write branches for what was observed, and let
an unknown variant through rather than dropping it.**

**`custom_tool_call` does not supersede `function_call`.** Both appear in the same files,
and `function_call` is still how every collaboration tool is issued, so a parser that
switches shapes on a version stops reading the calls that matter.

**Mid-turn flush is confirmed, so `watch` is genuinely offerable**: a single turn appends
to its rollout many times over its life, the first append within a fraction of a second,
interleaving reasoning, tool calls, and token counts.

**Identity is the vendor's, structurally.** `thread/start` has no `threadId` parameter at
all — the id comes back at `result.thread.id` — so "assigns its own" follows from the shape
of the API rather than from observation. Resume keeps the id and appends to the same
rollout file; fork mints a new id with `forkedFromId` set.

**`thread_history_*.sqlite` is a projection of the rollouts, not a replacement — read
the JSONL.** The store also holds `thread_history_*.sqlite` (`thread_items`,
`thread_turns`) and the CLI offers `migrate-rollouts` to publish legacy sessions into it,
which together read as a store migrating away from files. It is not:
`thread_history_projection_state` tracks a per-thread `next_rollout_byte_offset`, and
`thread_turns` keeps byte offsets pointing back into the file — the table is an
incrementally-built pagination index *over* the rollouts. It also covers only a small
fraction of the threads a real install holds, so a parser reading only SQLite silently
misses most sessions on the machine.

Two consequences for an adapter. The JSONL is the authoritative source for `adopt` and
`watch`, as the row above says. But `enumerate` may legitimately prefer the projection
where a thread is present, because it is the only place carrying turn-level `status`,
`duration_ms`, `started_at`, and `completed_at` — facts a rollout scan would have to
reconstruct — and it must fall back to walking the files for every thread the
projection does not hold.

**Model and effort come from `model/list`, and a bad effort value is confirmed rather
than refused.**

The vendor's refusals are inconsistent, which decides where validation lives.
`approvalPolicy: "sometimes"` and `personality: "grumpy"` are both refused with their
valid variants listed — those are enum-typed in the schema, so they map to
`INVALID_VALUE`. But `effort: "banana"` is **accepted**, echoed back by
`thread/settings/updated` as the current value, and only fails when a turn runs (`level
"banana" not supported, valid levels: low, medium, high, xhigh, max`); a nonexistent
model behaves the same way, and an unknown option name is accepted silently. The schema
types `ReasoningEffort` as a bare non-empty string, so **validation against
`model/list`'s `supportedReasoningEfforts` is the adapter's job.** This is the
"a vendor's `success` is not a promise it did what you asked" rule with a second vendor
behind it — and note it also means codex offers no `UNSUPPORTED_FIELD` signal at all, so
an unknown field must be caught adapter-side or it is lost.

`config/value/write` is worth naming separately: it writes to `config.toml` with **no
value validation whatsoever**, accepting both `model_reasoning_effort = "banana"` and an
entirely invented key with `status: "ok"`. It is a file writer, not a settings API.

## `pi`

**No second-party control surface.** `RpcClientOptions` is
`{cliPath, cwd, env, provider, model, args}` — no address field, no `createServer`, no
socket. It is stdio-only, reachable only by whoever spawned it. So a terminal-born `pi`
session is observable and undrivable, which is the case that rules out a third link
state.

**`set_model` takes `provider` + `modelId`, not `model`.** Sending `model=` is answered
`Model not found: undefined/undefined` — pi interpolating the two fields it looked for and
found absent — and `provider=` plus `model=` narrows it to `anthropic/undefined`, which is
what identifies *which* field is misspelled. The provider is not part of the value a client
sends, so look it up from the model list and cache it: a real install offers hundreds of
entries. **Do not parse the provider out of the id** — most ids contain a slash that is not
a provider separator.

Note how this one presents: the refusal arrives as a failed RPC, so with no error path in
the client a correct picker simply looks broken and the investigation goes to the UI rather
than to the adapter.

**A model's identity is `(provider, id)`.** Some ids are offered by more than one provider
and some display names are duplicated, so a bare id makes one of each pair unreachable and
puts a checkmark on both (`docs/session-protocol.md` § A config value must be unique).

**Thinking levels depend on the selected model**, which is why config is a refreshed array
rather than a session-global enumeration.

**A turn ends on `agent_settled`, not `agent_end`**, because `pi` can emit `agent_end` and
then continue. Both events exist and both reach the wire; the distinction is about which
one is terminal, not about which one is real.

**Prefer an unbundled source-mapped install to a minified bundle** when checking this
vendor's behaviour — one is available at
`~/.local/lib/node_modules/@earendil-works/pi-coding-agent`. Conclusions drawn from the
minified bundle have been wrong about which events exist at all.

**Two hazards a watcher must be built around.** `_rewriteFile()` truncate-rewrites with
mode `"w"` and no lock, so a reader can observe an empty or half-written file. And
`_persist` buffers until the first assistant message, so the file **does not exist** early
in a turn — an absent file is not an absent session.

**"Only at `message_end`" is true-but-trap.** A turn emits one per assistant message
*and* per tool result, so a 5-tool turn writes ~11 times. That is message granularity,
not turn granularity.

**It records no title of its own**, so every row needs the opening-ask fallback.

**Its native store has two field-spelling eras, and the rename landed field by field
rather than atomically.** That is the fact a parser must be built around. The store renamed
every multi-word key from snake_case to camelCase — `parent_id`→`parentId`,
`model_id`→`modelId`, `stop_reason`→`stopReason`, `error_message`→`errorMessage` — but
different fields flipped at different times, so a single file can hold snake `parent_id`
beside camel `stopReason`. No file carries both spellings of the *same* field.

So **resolve the spelling per field, never as one era per file.** A per-file era flag is
unsound rather than merely coarse: for the mixed files no single era exists, and a reader
choosing one spelling for the whole file gets one field right and another wrong. This is
worth stating plainly because the per-file shortcut is the obvious design and it passes on
the large majority of files — long enough to be believed.

Two traps around the same question:

- **The vendor's version field cannot discriminate.** Every file in every era carries the
  same `"version": 3`, so detection comes from field presence, never from the header.
- **Census keys by walking the parsed structure, never by grepping the serialized line.**
  These transcripts discuss field names, so a substring test matches words inside message
  *text* and reports keys a record does not have — inflating exactly the count a port
  branches on. The defect lands in the *measurement*, which is worse than in the code,
  because it produces a number and a number reads as evidence.

**Do not answer any of it with dual-spelling lookup.** `AGENTS.md` § Compatibility forbids
the alias, and an alias is what makes the *next* rename unobservable. Read field presence,
and fail loudly on a record whose combination no known era emits.

One shape fact falls out: `toolCallId`, `isError`, and `toolName` occur only in camelCase,
with no snake_case counterpart — so a snake_case-era file cannot hold a tool result, and the
spelling check is not independent of the tool-result path.

**`message.timestamp` is epoch milliseconds.** Read it as milliseconds; do not sniff a
value's magnitude to choose a divisor. Dividing as though it were microseconds yields a
plausible integer that decodes to 1970 — and note what makes that survivable and therefore
long-lived: session-level start and end come from the entry's own ISO string and stay
correct, so only the finer-grained per-event times are wrong. A handful of integers in the
store are the literal `1` written by test sessions, which look like a second unit era and
are not.

**There are also two timestamp eras, and a date heuristic cannot separate them.** Some
outer ISO stamps carry no UTC offset (`2026-04-08T03:20:12.387333`), so an RFC3339 parse
fails outright — and because the value is a string, an integer fallback never fires either,
so those yield *absent* rather than a wrong number. A file is one era or the other, never
both, but the offsetless window sits in the **middle** of the offset-aware range, so
"before/after this date" gets it backwards.

**Read an offsetless stamp as local wall-clock time, not as UTC.** Comparing such stamps
against their sibling integers — whose units are known — shows the offsetless era written in
whatever zone the machine was in, while the offset-bearing era agrees with UTC exactly.
Reading them locally therefore makes the result depend on where the parser runs, which is
normally a hazard and is correct here: the stamps *are* local wall-clock time, so every
fixed zone is wrong by construction, and the offsetless era is a closed historical window
rather than something new records join.

This is the vendor-payload family in its subtlest form, and the rule generalizes: **a wrong
time zone yields a plausible timestamp**, so no presence check and no eyeball test can catch
it, and the error is invisible on a machine whose own offset is zero. What settles a units
or zone question is comparing a value against a sibling whose units are already known —
which means censusing *pairs* rather than stamps. Draw such a pair from the wrong era and it
disagrees by construction, so the comparison confirms whatever you already believed.

**A fork pointer is a file path, not a session id.** The session header carries
`parentSession`/`parent_session` naming another `.jsonl` on disk. Under an identity of
`(environment, agent, native_id)` it cannot be stored as given: resolve it by reading the
target file's header for its id, or record it as unresolvable. Both spellings apply here,
per the per-field rule above.

## Reading a vendor payload

**This is the most-repeated defect class in the repo, and every instance compiles.** Each
surfaces one or two layers from its cause, because a misread key produces a plausible
record rather than an error. The specific keys live above under their agents, since that is
what a reader actually needs; these are the general rules.

**Print one real payload rather than inferring field names.** Guesses about a vendor's
shapes are wrong more often than not, and every wrong guess compiles.

**The right key in the wrong unit fails the same way and is harder to see.** A wrong key
yields *absent*, which a presence check finds; a wrong unit yields a *value*, which passes
every such check. So a payload's units are part of what printing one real payload has to
establish — compare a number against a sibling human-readable stamp rather than eyeballing
its magnitude.

**Ask the binary.** Grepping a vendor's shipped binary for a key you intend to write is
cheap and decisive, and it catches the case where the key exists nowhere at all.

**An SDK object's attribute names and its JSON wire names are different namespaces.**
Where a spec says `requestedSchema` and an SDK's own field is `requested_schema` with the
camelCase form as an alias, a client passing the object's `__dict__` without resolving
aliases sends the snake_case form — so the *spec-correct* key reads absent against a
spec-compliant SDK. When a payload came from an SDK rather than off a socket, read the SDK
side.

**A type in a vendor's event union is not a promise anything emits it.** Grep the
implementation for emit sites, not the types for declarations. And note the symmetric
error: "I grepped and found nothing" is a claim about your grep as much as about the
vendor, so prefer an unbundled source-mapped install over a minified bundle.

**A success and a refusal are different shapes.** `{}` must never be reachable from two
opposite outcomes.

**The first `user` event is usually not what the user typed.** Agents inject their own
context as leading pseudo user turns (`<permissions instructions>`,
`<environment_context>`, `<skills_instructions>`, memory files as `# AGENTS.md
instructions`, `claude`'s `Caveat:` preamble). Taking the first user event verbatim labels
most sessions with boilerplate that is byte-identical across every session of that agent,
so skipping injected envelopes is what makes a label recognizable at all.

**Keep that rule narrow.** Treating a leading `You are ` as boilerplate discards genuine
labels, because a subagent task brief legitimately opens that way. An over-broad filter
here costs real data and looks like correct behaviour.

## Testing an agent module

Two kinds of test, and neither substitutes for the other.

**Recorded-payload tests** pin classification against real vendor JSON checked into the
repo. This is where classification is actually covered: a live run spends real tokens and
sees only whatever shapes one turn happens to produce, so a classification defect — a
backgrounded shell job read as a delegation, say — is invisible to it.

**A live verification path** runs a real turn against the real CLI and refreshes the
recorded payloads. Recorded fixtures go stale as vendors change, so this cannot be
deleted along with the adapter subprocess — it is what keeps the fixtures honest.

**A fixture's size and timing are both part of the contract**, not details of it. A
two-item model list cannot observe an unscrollable, unsearchable catalogue, because real
agents report a dozen or more. A scenario emitting its whole tree inside one millisecond
gives every bar equal length, so a client computing durations from the wrong field passes
everything. And a delegate that forwards nothing cannot be timed from its interior at all,
so "each delegate forwards at least one event" is a fixture requirement rather than a nod
to realism. **When a defect is a function of how much a control holds or how fast events
arrive, the fixture's shape is the test.**
