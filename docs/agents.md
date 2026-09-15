# Agents

This document owns process setup, controls and dated vendor measurements. Replacement
trace interpretation is owned by the dedicated [Claude](unified-trace-claude.md),
[Codex](unified-trace-codex.md), and [Pi](unified-trace-pi.md) mappings. Older block/span,
input segmentation and synthetic native-event examples below describe the existing
implementation only; they cannot override those target contracts.

One module per vendor, holding everything Sylph knows about that CLI: how to launch
it, how to read its native store, what it exposes to a second party, and where it
lies.

## Three agents

Supported: **`claude`**, **`codex`**, **`pi`**. Two requirements decided that list, and
both govern any agent proposed later:

- **A source with stable, scoped identities and bounded acquisition.** Per-session
  JSONL is one suitable shape. A native database is also usable with consistent snapshots,
  session-scoped queries and explicit capture positions; a per-session file fingerprint
  is not an architectural requirement.
- **Known mid-turn write behaviour.** Observation is offerable only where it is known
  *when* a vendor flushes; unknown is not the same as absent.

Only these three vendors are in scope. Additional vendors require their own evidenced
acquisition and semantic mapping, rather than exclusion by a storage-format assumption.

An agent is available on an environment when its CLI is on that machine's `PATH` **at a
version the adapter accepts**, discovered by the envagent and re-discovered when the
machine's inventory changes (`docs/environments.md` § Agent availability).

## What every agent module must answer

| Question | Why it cannot be generic |
| --- | --- |
| launch argv, per mode | each CLI spells resume, session id, model, and effort differently |
| native store location and format | JSONL per session, or a store with its own row ids |
| does it accept a dictated session id | decides whether Sylph knows identity before the first event |
| mid-turn flush granularity | decides whether `watch` sees a turn in progress or only its end |
| second-party control surface | decides whether the control facet exists at all |
| which vendor keys carry what | the most-repeated defect class in this repo |

[unified-trace-storage.md](unified-trace-storage.md#source-identity-and-generations)
owns capture/source identity, generations and complete-record cursors. Native and live
observations both contribute semantic facts under explicit equivalence rules; missing
joins remain visible. No mode discards richer native semantics merely because a live
connection already exists.

## Capability matrix

Against `claude` 2.1.247 and `pi` 0.84.3 on 2026-08-27, and `codex-cli` 0.150.1 on
2026-08-28. These are the minimum versions currently verified by the adapters; a CLI below
its adapter's floor is reported unavailable with the version it reported. Versions and dates
belong with the table: a capability claim without one goes stale invisibly.

| | `claude` | `codex` | `pi` |
| --- | --- | --- | --- |
| dictate session id | `--session-id <uuid>` | assigns its own, reports it | `--session-id <id>`, creates if missing |
| resume | `-r <id>` | `resume <id>`, `exec resume`, `thread/resume` | `--session-id <id>` |
| fork | — | `exec fork <id>`, `thread/fork` | — |
| native store | JSONL per session | `rollout-*.jsonl` | JSONL per session |
| mid-turn flush | tool-call and content-block boundaries | item completion | every `message_end` |
| second-party control | stdio while Sylph holds it; **none** once adopted from a terminal | app-server: **fully drivable**, incl. interrupt and steer | **none** — stdio only |
| second-party observe | — | explicit `thread/resume`; independent of control | — |
| forwards delegate interior | only for built-in `Task` | **yes**, with the child's `threadId` | no delegation on the wire at all |
| slash commands | yes, project-dependent | no command concept | yes |
| typed config | model, effort, permission mode, output style | model, effort, approval policy, personality, sandbox, cwd | model, thinking level, queue modes |

Three columns carry the design. **Control** is why `docs/architecture.md` makes control a
facet that reports unavailable with a reason: `codex` is drivable by a second party over its
app-server, `pi` not at all, and `claude` depends on who holds its stdio — drivable when
Sylph started it, `not_ours_to_drive` once adopted from a terminal. **Flush** is why `watch`
is offerable: all three write mid-turn, so an observer sees progress rather than outcomes.
**Observe** is why the facets are separate rather than one "reachable" flag: `codex` splits
them natively, since a second client must call `thread/resume` to receive events and can
drive a turn without ever subscribing.

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

**Model catalogue entries are `{value, displayName, …}`, not `{id, …}`.**

**`system/init` arrives once per turn, not once per process.** A whole-surface
capability payload answering it erases everything it does not mention; facets exist
for this.

**Delegation notifications.** The subtypes are `task_started`, `task_progress`,
`task_updated`, `task_notification`; there is no `task_completed` or `task_failed`. A close
arrives either as `task_updated` with `patch.status`, or as `task_notification` with
`status` plus the delegate's summary. `task_updated` carries **no `tool_use_id`**, so the
host is remembered from `task_started`, or the close lands at the root while the trajectory
it closes sits inside a container.

**`task_type` is the only thing distinguishing a delegation from a backgrounded shell
job.** `local_agent` is a real subagent; `local_bash` is `run_in_background` on Bash
reporting through the same channel. Neither `task_updated` nor `task_notification` carries
the field, so it too is remembered from `task_started`. **Filter narrowly and let an unknown
type through**: that direction costs a missing subtree, while the other publishes a shell
command as a subagent and escalates its host tool's status.

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

**The inbound approval gate is not a security boundary, and it does not decide the
facet.** Every sender is hardcoded to `peer`, and the gate compares only a self-declared
`mode` tag carried in the message body — which the sender supplies, so any local process
bypasses it. Approved messages are queued rather than delivered synchronously
(`Routed user message to queue (priority=`).

So the control facet is decided by transport, not by the gate. A terminal-adopted
`claude` session reports `control: unavailable (not_ours_to_drive)` because interrupt and
steer have **structurally no channel** — no socket carries them and there is no pipe to
the process. A session Sylph started runs over stdio and reports `control: available`.
The discriminant is whether Sylph holds the process's stdio, which is a property of the
link (`docs/architecture.md` § One link, two typed capability facets).

**Never connect or write to a socket in the user's real `cc-socks` directory.** That
injects a turn into a live session of theirs. Probe only a throwaway child's own pid.

**It records its own title for most sessions but not all**, so any surface showing one
needs the opening-ask fallback.

**In the native store, a tool result's id and its error flag are both on the content
block — nowhere else.** Stated as shapes rather than as counts, since a developer's store
grows while it is being measured while "this key is on that block and on no other" stays
true.

`tool_use_id` sits on the `tool_result` block inside `message.content[]`, on every such
block. `toolUseResult` under either spelling, and the top level of the record, hold it
nowhere — so a parser reading those pairs nothing, and the pairing
`docs/session-protocol.md` § Keying rules depends on does not exist.

`is_error` is on that same block and **only** on records whose `message.role` is `user`,
never on an assistant message. Read from the assistant branch it classifies every tool
failure as a success, since `false` and absent-defaulted-to-`false` are indistinguishable
downstream.

Neither misread raises an error: the parser completes over the whole store and produces
plausible records. So assert on *values a parser should have found* — that a failure parses
as a failure — rather than on the absence of a crash.

### A host-managed auth switch inherited from another agent session

**A vendor process inherits the envagent's environment, so who launched the envagent can
decide whether a session can authenticate at all.** This is a distinct failure family from
misreading a payload: nothing is parsed wrong. The CLI does exactly what the environment
told it, and the transcript records a real refusal.

**One environmental cause, two observable symptoms.** Which one a session shows depends on
how much of the inherited configuration is still in place: `Not logged in · Please run
/login` while the keychain switches remain, and `Failed to authenticate. API Error: 401
Invalid token` once they are gone but the endpoint is still redirected. A reader who
arrived here from a 401 is in the right place;
`docs/development.md` § A session fails its first turn with an authentication error is the
diagnostic path.

Two `claude` variables disable the CLI's own keychain login and declare that the **host**
supplies the credential instead. *Measured on 2.1.267:*

| Environment | `claude auth status` |
| --- | --- |
| clean | `loggedIn: true, authMethod: "oauth_token"` |
| `CLAUDE_CODE_HOST_AUTH_ENV_VAR=ANTHROPIC_AUTH_TOKEN` | `loggedIn: false, authMethod: "none"` |
| `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST=1` | `loggedIn: false, authMethod: "none"` |
| either switch **+** non-empty `ANTHROPIC_AUTH_TOKEN` | `loggedIn: true, authMethod: "oauth_token"` |
| either switch **+** non-empty `ANTHROPIC_API_KEY` | `loggedIn: true, authMethod: "api_key"` |
| `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST=1` **+** `CLAUDE_CODE_USE_BEDROCK=1` | `loggedIn: true, authMethod: "third_party"` |

So the last three rows are a **working** host-managed deployment and stripping them would
break an operator's configuration. Only a switch with *nothing to answer for it* is broken,
and it is broken in the expensive way: the session starts, accepts a turn, and answers
`Not logged in · Please run /login`, while `claude auth status` in the developer's own
terminal reports `loggedIn: true`. Nothing between the two says the environment was the
cause.

**A third variable decides whether removing the switches is enough.**
`ANTHROPIC_BASE_URL` redirects the API endpoint, and it is set by exactly the same
inherited environment. Removing the switches restores the keychain token, which is then
sent to whatever host the relay names — and a relay issued its own tokens, so it rejects
that one. *Measured on 2.1.267, running `claude -p "say ok"` inside that environment:*

| Inherited environment | Result |
| --- | --- |
| both switches, no credential, `ANTHROPIC_BASE_URL` set | exit 1, `Not logged in · Please run /login` |
| switches removed, `ANTHROPIC_BASE_URL` set | exit 1, `Failed to authenticate. API Error: 401 Invalid token` |
| switches removed, `ANTHROPIC_BASE_URL` also removed | exit 0 |

The messaging pair `CLAUDE_CODE_MESSAGING_SOCKET` and `CLAUDE_CODE_MESSAGING_TOKEN` is
inherited too and was ruled out by removing it and re-measuring: the 401 is unchanged, so
the endpoint is the single remaining cause.

**The endpoint is only knowably wrong in combination, so it is judged and never filtered.**
A redirected endpoint *with* a credential is the working relay deployment above, and a
redirected endpoint with **no switch at all** is an operator pointing the CLI at a gateway
of their own — neither is Sylph's business. What is unambiguous is the conjunction the
inherited environment produces: the keychain disabled, no credential to replace it, and the
endpoint moved. That combination serves no configuration, which is the same standard the
switches themselves are held to.

**An envagent started from inside another coding-agent session is how this arrives.** That
session exports its own `CLAUDE_CODE_*` variables, the envagent inherits them, and every
vendor process it spawns inherits them again.

Four measurements decide what the code may assume, and each rules out a repair that would
otherwise look obvious:

- **The pointer's value is not read as a credential.** Pointing it at `MY_HOST_TOKEN` *and*
  setting `MY_HOST_TOKEN` still reports `loggedIn: false`, while an unrelated pointer name
  plus a real `ANTHROPIC_API_KEY` reports `loggedIn: true`. Its only measured effect is to
  disable the keychain, so a repair that read the named variable would be a check that
  cannot fail.
- **Nothing inside the session repairs the broken state.** Neither a populated
  `CLAUDE_CODE_HOST_CREDS_FILE` nor an `apiKeyHelper` passed via `--settings` rescues it;
  `-p` still answers `Not logged in`. This is what makes removal defensible rather than
  merely convenient — the state serves no configuration.
- **The switch is truthy-valued, not presence-valued.** `1`, `true`, `yes`, and `on` (any
  case) disable the keychain; `0`, `false`, `no`, `off`, the empty string, `2`, and `xyz`
  leave it working. Reading presence would report a fault on `=0`, which is an operator
  turning the switch *off* — a diagnostic that cries wolf is how a real warning stops being
  read.
- **Removing the switches alone does not restore a session.** It restores the keychain
  token and leaves it aimed at the relay, turning a refusal into a 401. So the broken state
  is the whole conjunction, and a repair that addresses part of it produces a *different*
  failure rather than a working session — which reads as a regression in the fix.

`crates/adapters/src/claude/hostauth.rs` owns the classification and the report, and
`AGENTS.md` § Rules That Keep Costing Time governs the reporting shape: this is one fact
about the run, so it is reported once at `error` and thereafter at `debug`, never once per
spawn. The removal is never silent — the report names the variables it removed and says to
restart the envagent from a clean terminal.

**The class does not generalize to the other two agents, and that was checked rather than
assumed.** `codex` has no host-managed-auth switch: its `CODEX_MANAGED_BY_*` variables name
the *package manager* that installed it, and `login status` reported the same account with
bogus `OPENAI_API_KEY` and `CODEX_API_KEY` values in the environment. `pi` reads two
environment variables in its whole bundle, `AI_AGENT` and `PI_CODING_AGENT`, neither of
which is a credential. So the check lives in the `claude` adapter, not in
`crates/adapters/src/process.rs`, which only gains the general ability to remove a named
variable from a spawn.

## `codex`

Against `codex-cli` 0.150.1. Capture scripts and raw frames live in
`docs/measurements/codex/`, where each numbered `sNN` probe is re-runnable.

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
null` with `forkedFromId` set. Where both exist the parent wins
(`docs/database-schema.md` § Identity in the schema).

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
  (`agent_path: "/root"`). The record's own side therefore decides the edge's direction;
  mapping it to `span` unconditionally inverts every child-side edge, and nothing
  downstream can detect that.
- **`collabAgentToolCall` is not lineage.** It carries empty `receiver_thread_ids` and
  `agents_states` with `tool: "wait"`, and its `status` is the *wait's* rather than any
  delegate's. It marks a barrier.

**The `spawn_agent` brief is ciphertext in current versions**, so a delegation's name is
unrecoverable from the parent's own record. `docs/session-protocol.md` § "`name` is absent
when unknown" governs: omit the name, because the journal makes every guess permanent and a
row written with a placeholder is afterwards indistinguishable from one that carried a real
name.

**A childless trajectory means the opposite of what it means under `claude`**: there it is
a terminal state the card must state, here a loading state, since codex does forward the
interior. The two agents need opposite defaults, which is why `fidelity` and the capability
facets are per-record rather than global.

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

The Codex watcher follows complete JSONL lines with a generation-aware byte
cursor, rediscovers child and fork rollouts as they appear, and preserves
cursors when a rollout moves between date directories. Reload reconstructs
delegation/fork edges from headers and measured team paths. Forwarded child
items in the parent rollout are de-duplicated when the child's own rollout is
present, so one normalized event has one owner.

**Identity is the vendor's, structurally.** `thread/start` has no `threadId` parameter at
all — the id comes back at `result.thread.id` — so "assigns its own" follows from the shape
of the API rather than from observation. Resume keeps the id and appends to the same
rollout file; fork mints a new id with `forkedFromId` set.

**`thread_history_*.sqlite` is a pagination index over the rollouts, not a replacement —
read the JSONL.** `thread_history_projection_state` tracks a per-thread
`next_rollout_byte_offset` and `thread_turns` keeps byte offsets pointing back into the
file, so the tables are built incrementally *from* the rollouts and cover only a fraction
of the threads a real install holds.

So the JSONL is authoritative for `adopt` and `watch`. `enumerate` may prefer the
projection where a thread is present, since it is the only carrier of turn-level `status`,
`duration_ms`, `started_at`, and `completed_at`, and falls back to walking the files for
every thread it does not hold.

**Model and effort come from `model/list`, and a bad effort value is confirmed rather
than refused.**

The vendor's refusals are inconsistent, which decides where validation lives. Enum-typed
fields are refused with their valid variants listed and map to `INVALID_VALUE`:
`approvalPolicy: "sometimes"`, `personality: "grumpy"`. But the schema types
`ReasoningEffort` as a bare non-empty string, so `effort: "banana"` is **accepted**, echoed
back by `thread/settings/updated` as the current value, and fails only when a turn runs
(`level "banana" not supported, valid levels: low, medium, high, xhigh, max`). A
nonexistent model behaves the same way, and an unknown option name is accepted silently.

**Validation against `model/list`'s `supportedReasoningEfforts` is therefore the adapter's
job**, and codex offers no `UNSUPPORTED_FIELD` signal at all, so an unknown field is caught
adapter-side or lost.

`config/value/write` is worth naming separately: it writes to `config.toml` with **no
value validation whatsoever**, accepting both `model_reasoning_effort = "banana"` and an
entirely invented key with `status: "ok"`. It is a file writer, not a settings API.

## `pi`

The target timestamp interpretation is
[unified-trace-pi.md](unified-trace-pi.md#context-payloads-and-measurements): offsetless
historical timestamps remain ambiguous unless actual timezone/epoch evidence resolves
them. The current parser's machine timezone is not such evidence.

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

**Pi does not offer turn-scoped Steer.** Its RPC `steer` handler calls
`AgentSession.steer`, whose `_queueSteer` unconditionally inserts into the steering queue.
Neither the command nor its receipt carries a turn identity or checks that a turn remains
open. Observing `agent_start` locally cannot close the race with the vendor settling before
it reads stdin. Sylph exposes this primitive as QueueSteering and follow-up as Queue, alongside its
editable server-owned queue. Turn-scoped Steer is refused without writing to the vendor. Codex's `expectedTurnId` supplies the required atomic
precondition; the envagent binds that vendor id to Sylph's unique turn generation.

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

The Pi watcher therefore keeps a byte cursor only at complete-line boundaries, verifies the
consumed prefix before each changed-file pass, and treats an inode replacement or prefix change
as a new file generation. Unchanged record ids are de-duplicated across a rewrite; a record whose
payload changed under the same id requires the supported field-revision rule before
changing its semantic projection. Changed owner or predecessor claims remain conflicts. A missing or
partial header remains a pending source and is rediscovered on the next pass. This is a
source-specific rule: Claude's append-only bundle cursor and Codex's rollout cursor must not
inherit Pi's rewrite assumptions.

Pi's persistence writer terminates each `JSON.stringify(entry)` with `\n`; a valid JSON value
without that terminator is consequently treated as an in-flight final line and retried rather
than published early.

Pi has no second-party control channel. A terminal-owned native watch is therefore
`observe: available` with `control: unavailable (not_ours_to_drive)`; the live RPC session
still advertises its own controls when Sylph launched the process. The adapter exposes this
as a native-link facet for the environment bridge; the bridge, rather than the file parser,
owns publication of the link state.

**"Only at `message_end`" is true-but-trap.** A turn emits one per assistant message
*and* per tool result, so a 5-tool turn writes ~11 times. That is message granularity,
not turn granularity.

**It records no title of its own**, so every row needs the opening-ask fallback.

**Its native store has two field-spelling eras, and the rename landed field by field
rather than atomically.** Every multi-word key moved from snake_case to camelCase —
`parent_id`→`parentId`, `model_id`→`modelId`, `stop_reason`→`stopReason`,
`error_message`→`errorMessage` — at different times, so one file can hold snake `parent_id`
beside camel `stopReason`. No file carries both spellings of the *same* field.

So **resolve the spelling per field, never as one era per file.** For a mixed file no single
era exists, so a per-file flag gets one field right and another wrong while passing on the
large majority of files. Two facts bound the detection:

- **The vendor's version field cannot discriminate.** Every file in every era carries
  `"version": 3`, so detection comes from field presence.
- **`toolCallId`, `isError`, and `toolName` occur only in camelCase**, so a snake_case-era
  file cannot hold a tool result and the spelling check is not independent of the
  tool-result path.

Read field presence, and fail loudly on a record whose combination no known era emits. A
dual-spelling alias is what would make the *next* rename unobservable.

**Take a census by walking the parsed structure, never by grepping the serialized line.**
These transcripts discuss field names, so a substring test matches words inside message
*text* and reports keys a record does not have, inflating exactly the count a port branches
on.

**`message.timestamp` is epoch milliseconds.** Read it as milliseconds rather than sniffing
a value's magnitude to choose a divisor: session-level start and end come from the entry's
own ISO string and stay correct whatever the divisor, so a wrong unit corrupts only the
per-event times. A handful of integers in the store are the literal `1` written by test
sessions, which resemble a second unit era and are not.

**There are also two timestamp eras, and a date heuristic cannot separate them.** Some
outer ISO stamps carry no UTC offset (`2026-04-08T03:20:12.387333`), so an RFC3339 parse
fails outright — and because the value is a string, an integer fallback never fires either,
so those yield *absent* rather than a wrong number. A file is one era or the other, never
both, but the offsetless window sits in the **middle** of the offset-aware range, so
"before/after this date" gets it backwards.

**Offsetless historical timestamps are ambiguous without source evidence.** A sibling
epoch value can establish that message's time; the parser's current timezone cannot
establish the timezone of an earlier environment. The replacement preserves the lexical
stamp and explicit uncertainty under
[unified-trace-pi.md](unified-trace-pi.md#context-payloads-and-measurements).

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
