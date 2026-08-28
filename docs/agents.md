# Agents

One module per vendor, holding everything Sylph knows about that CLI: how to launch
it, how to read its native store, what it exposes to a second party, and where it
lies.

## Three agents, and why the list shrank

Supported: **`claude`**, **`codex`**, **`pi`**.

`gemini` and `opencode` are **not supported**. This is a deliberate narrowing, not a
gap waiting to be filled, and it deletes 867 lines of parser along with two open
questions that were blocking design work:

- **`opencode` was the only agent that broke every mechanism this architecture
  depends on.** *Measured:* no per-session file at all — one 541 MB SQLite store with
  a live WAL — so a file-mtime watcher sees nothing, "upload the bytes" has no
  referent, and the fingerprint an ingest path needs cannot be computed. An attempt to
  read it wholesale reached 4.6 GB still running at ~20 minutes and starved every other
  agent's pass. Its read strategy was recorded as *unresolved and must not be guessed*,
  and it was gating the ingestion timer.
- **`gemini`'s mid-turn write behaviour was never located** — unknown rather than
  measured absent — so it could not be promised as observable, and it exposes no
  second-party control channel beyond start-time flags.

Dropping both turns two unresolved questions into zero. What survives from them is
the *rule* they taught, which is stated in `docs/environments.md`: reading a native
store is one strategy per agent, and nothing constrains an agent to keep a
per-session file.

An agent is available on an environment when its CLI is on that machine's `PATH`.
That is the whole check. There is no discovery handshake, no adapter directory, and
no `adapter.sh` — see `docs/session-protocol.md` § What changed for the two total
failures that mechanism produced.

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

*Measured 2026-08-27 against `claude` 2.1.247, `codex`, `pi` 0.84.3.*

| | `claude` | `codex` | `pi` |
| --- | --- | --- | --- |
| dictate session id | `--session-id <uuid>` | assigns its own, reports it | `--session-id <id>`, creates if missing |
| resume | `-r <id>` | `resume <id>`, `exec resume` | `--session-id <id>` |
| fork | — | `exec fork <id>` | — |
| native store | JSONL per session | `rollout-*.jsonl` | JSONL per session |
| mid-turn flush | tool-call and content-block boundaries | item completion | every `message_end` |
| second-party control | authenticated per-process UDS — **delivers, cannot drive** | app-server daemon: `remote-control pair`, `queue --thread` | **none** — stdio only |
| slash commands | yes, project-dependent | no command concept | yes |
| typed config | model, effort, permission mode, output style | approval policy | model, thinking level, queue modes |

Two things to read off this table rather than from prose. The control column is why
`docs/architecture.md` makes control a facet that can report unavailable with a reason:
one of three agents is fully drivable by a second party, one is not drivable at all,
and one is in between. And the flush column is why `watch` is offerable at all — all
three write mid-turn, so an observer sees progress rather than only outcomes.

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
`Workflow` forward nothing, so their interior cannot be attributed at all and the
lifecycle notifications are the whole record.

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

**Archived titles cover ~73% of sessions.** Any surface showing one needs the
opening-ask fallback.

## `codex`

**The strongest second-party surface of the three, and the reason the control facet is
worth having.** A shared local app-server daemon, `remote-control start|stop|pair` with
short-lived pairing codes, and `queue --thread <uuid> --message <text>` accepting
`--remote ws://|wss://|unix://` with bearer auth. Sylph's own pairing flow
(`docs/environments.md`) is modelled on this, which is evidence the shape works rather
than a guess.

**It assigns its own session id and reports it**, so Sylph keys on what came back
rather than dictating. `exec fork <id>` exists, which is the vendor implementing the
fork relationship `docs/architecture.md` uses for moving work between environments.

**No command concept at all**, so the `commands` facet is empty and a client must not
present an empty slash menu as a failure. Its typed config is approval policy.

**Native store is `rollout-*.jsonl`, flushed at item completion**, with
`token_count` ticks proving mid-turn append.

**A subagent may record only a parent id with no id-suffix marker.** Measured from its
own store: a rollout with `parent_thread_id` set and `thread_source: subagent`. So
lineage cannot be recovered from an id-naming convention alone.

**`thread_history_*.sqlite` is a projection of the rollouts, not a replacement — read
the JSONL.** *Measured 2026-08-28 against codex-cli 0.150.1.* The store also holds
`~/.codex/thread_history_1.sqlite` (`thread_items`, `thread_turns`) and the CLI offers
`migrate-rollouts` to publish legacy sessions into it, which together read as a store
migrating away from files. It is not: `thread_history_projection_state` tracks a
per-thread `next_rollout_byte_offset` and `next_rollout_ordinal`, and `thread_turns`
keeps `rollout_byte_offset`/`rollout_end_byte_offset` pointing back into the file — the
table is an incrementally-built pagination index *over* the rollouts. The census settles
it: 1258 `rollout-*.jsonl` against 73 projected threads on the same install, so a
parser reading only SQLite would silently miss 94% of the sessions on this machine.

Two consequences for an adapter. The JSONL is the authoritative source for `adopt` and
`watch`, as the row above says. But `enumerate` may legitimately prefer the projection
where a thread is present, because it is the only place carrying turn-level `status`,
`duration_ms`, `started_at`, and `completed_at` — facts a rollout scan would have to
reconstruct — and it must fall back to walking the files for every thread the
projection does not hold.

**Model and thinking are read from `--describe`**, which is why `codex` correctly
shows effort with no model selector — nothing in the UI hardcodes a model list.

## `pi`

**No second-party control surface.** `RpcClientOptions` is
`{cliPath, cwd, env, provider, model, args}` — no address field, no `createServer`, no
socket. It is stdio-only, reachable only by whoever spawned it. So a terminal-born `pi`
session is observable and undrivable, which is the case that rules out a third link
state.

**`set_model` takes `provider` + `modelId`, not `model`.** Sending `model=` is answered
`Model not found: undefined/undefined` — pi interpolating the two fields it looked for
and found absent — and `provider=` plus `model=` narrows it to `anthropic/undefined`,
which is what identifies *which* field is misspelled. The provider is not part of the
value a client sends, so it must be looked up from the model list (372 entries on a real
install, so cache it). Do not parse it out of the id: 350 of those ids contain a slash
that is not a provider separator.

This was the most expensive vendor-key instance in the repo's history: the refusal came
back as a failed RPC that nothing surfaced, so a correct picker looked broken and the
investigation went to the UI twice before reaching the adapter.

**A model's identity is `(provider, id)`.** Of 372 offered models, two ids are each
offered by two providers and three display names are duplicated. The bare id made one
of each pair unreachable.

**Thinking levels depend on the selected model** — the current one offers only `off`
and `high` — which is why config is a refreshed array rather than a session-global
enumeration.

**A turn ends on `agent_settled`, not `agent_end`.** `pi` can emit `agent_end` and then
continue. Both events genuinely exist and are emitted (`agent_start` at
`pi-agent-core/dist/agent-loop.js:49,67`, `agent_end` at `:110,157,172`, both reaching
the wire through `dist/modes/rpc/rpc-mode.js:266`); an earlier claim here that they had
zero emit sites was **wrong**, measured against a minified bundle. An unbundled
source-mapped install at `~/.local/lib/node_modules/@earendil-works/pi-coding-agent` is
far better evidence and should be preferred.

**Two hazards a watcher must be built around.** `_rewriteFile()` truncate-rewrites with
mode `"w"` and no lock, so a reader can observe an empty or half-written file. And
`_persist` buffers until the first assistant message, so the file **does not exist** early
in a turn — an absent file is not an absent session.

**"Only at `message_end`" is true-but-trap.** A turn emits one per assistant message
*and* per tool result, so a 5-tool turn writes ~11 times. That is message granularity,
not turn granularity.

**It records no archived title**, so every row needs the opening-ask fallback.

**Its native store has two field-spelling eras that landed field by field, plus two
timestamp eras, and one unit bug that reads as a spelling bug.** *Measured 2026-08-28
over all 527 files in `~/.pi/agent/sessions/`, while scoping the parser port.*

The store renamed every multi-word key from snake_case to camelCase — `parent_id`→
`parentId`, `model_id`→`modelId`, `stop_reason`→`stopReason`, `error_message`→
`errorMessage`. **The rename was not atomic, and that is the fact a parser must be built
around.** No file carries both spellings of the *same* field, but different fields flipped
at different times, so 65 files hold snake `parent_id` beside camel `stopReason`. The
switch points really are per field: `parent_id` is 111 files snake against 120 camel,
`stop_reason` 25 against 185, `model_id` 46 against 120.

So **resolve the spelling per field, never as one era per file.** A per-file era flag is
unsound rather than merely coarse: for those 65 files no single era exists, and a reader
choosing one spelling for the whole file gets `parent_id` right and `stopReason` wrong, or
the reverse. Worth stating because the per-file shortcut is the obvious design and it
passes on 462 of 527 files.

Two traps around this same measurement. **The vendor's version field cannot
discriminate** — `"version": 3` is on all 527 files in every era — so detection has to come
from field presence, not from reading the header. And **take a key census by walking the
parsed structure, never by grepping the serialized line**: these transcripts discuss field
names, so a substring test matches words inside message *text* and reports keys a record
does not have. That inflates exactly the count a port branches on, and it manufactured a
phantom set of mixed files during this work.

Do not answer any of it with dual-spelling lookup. § Compatibility forbids the alias, and
an alias is what makes the *next* rename unobservable. Read field presence, and fail
loudly on a record whose combination no known era emits.

One shape fact falls out of the counts: `toolCallId`, `isError`, and `toolName` occur only
in camelCase, on 63 files, with **no** snake_case counterpart — so a snake_case-era file
cannot hold a tool result, and the spelling check is not independent of the tool-result
path.

The unit bug is the more consequential half and belongs to the *reader*, not the vendor.
`message.timestamp` is **epoch milliseconds**, verified by `1776416849330` on an entry
whose sibling ISO stamp is `2026-04-17T17:07:29.330418` — equal to the millisecond, on
3880 records. trace-db's parser divided an integer timestamp by 1000 while its string
branch returned milliseconds, so every per-event time it produced decoded to 1970. Two
things made it survivable: the session-level start and end come from the *entry's* ISO
string and were therefore right, so only the finer-grained per-event times were wrong;
and the 67 remaining integers are the literal `1` written by test sessions
(`"model":"claude-test"`), which look like a second unit era and are not. Do not sniff
magnitude to choose a divisor — the vendor writes milliseconds, so read milliseconds.

The second timestamp era compounds it. **746 of 4881 outer ISO stamps carry no UTC
offset** (`2026-04-08T03:20:12.387333`), so an RFC3339 parse fails outright; and because
the value is a string, an integer fallback never fires either, so those yield absent
rather than a wrong number. This split *is* clean — 407 files naive-only, 120
offset-aware-only, none mixing — but the naive window (2026-04-07 to 04-20) sits in the
**middle** of the offset-aware range (2026-02-27 to 08-26), so a "before/after this date"
heuristic gets it backwards. Parse both forms, and treat a missing offset as UTC.

**A fork pointer is a file path, not a session id.** The session header carries
`parentSession`/`parent_session` naming another `.jsonl` on disk. Under an identity of
`(environment, agent, native_id)` it cannot be stored as given: it has to be resolved by
reading the target file's header for its id, or recorded as unresolvable. trace-db's
parser hardcoded `parent_session_id: None` and never read the field under either
spelling, so pi fork lineage was dropped entirely rather than partially.

The unit bug is the more consequential half and belongs to the *reader*, not the vendor.
`message.timestamp` is **epoch milliseconds**, verified by `1776416849330` on an entry
whose sibling ISO stamp is `2026-04-17T17:07:29.330418` — equal to the millisecond, on
3880 records. trace-db's parser divided an integer timestamp by 1000 while its string
branch returned milliseconds, so every per-event time it produced decoded to 1970. Two
things made it survivable: the session-level start and end come from the *entry's* ISO
string and were therefore right, so only the finer-grained per-event times were wrong;
and the 67 remaining integers are the literal `1` written by test sessions
(`"model":"claude-test"`), which look like a second unit era and are not. Do not sniff
magnitude to choose a divisor — the vendor writes milliseconds, so read milliseconds.

**A fork pointer is a file path, not a session id.** The session header carries
`parentSession`/`parent_session` naming another `.jsonl` on disk. Under an identity of
`(environment, agent, native_id)` it cannot be stored as given: it has to be resolved by
reading the target file's header for its id, or recorded as unresolvable. trace-db's
parser hardcoded `parent_session_id: None` and never read the field under either
spelling, so pi fork lineage was dropped entirely rather than partially.

## Reading a vendor payload

This is the most-repeated defect class in the repo — eight instances, each one
compiling, each surfacing one or two layers from the cause. The instances are above,
under their agents, because the *specific* keys are what a reader needs. The general
rules:

**Print one real payload rather than inferring field names.** Two of three guesses
about `claude`'s shapes were wrong, and each one compiled.

**The right key in the wrong unit fails the same way, and is harder to see.** The eighth
instance was not a misspelling: `pi`'s `message.timestamp` was read from the correct key
and divided as though it were microseconds, so it produced a plausible integer that
decoded to 1970. A wrong key at least yields absent; a wrong unit yields a *value*, which
passes any check for presence. So a payload's units are part of what "print one real
payload" has to establish — compare the number against a sibling human-readable stamp
rather than eyeballing its magnitude.

**Ask the binary.** Grepping the 321 MB `claude` binary for a key is cheap and
decisive: `selectedOptions` occurs nowhere in it, which would have caught that instance
before it shipped.

**An SDK object's attribute names and its JSON wire names are different namespaces.**
The MCP spec says `requestedSchema`; the Python SDK's field is `requested_schema` with
the camelCase form as an alias, and `claude` passes `__dict__` without resolving
aliases. So the spec-correct key reads absent against a spec-compliant SDK. When the
payload came from an SDK rather than off a socket, read the SDK side.

**A type in a vendor's event union is not a promise anything emits it.** Grep the
implementation for emit sites, not the types for declarations. And note that "I grepped
and found nothing" is a claim about your grep as much as about the vendor — prefer an
unbundled source-mapped install over a minified bundle.

**A success and a refusal are different shapes.** `{}` must never be reachable from two
opposite outcomes.

**The first `user` event is usually not what the user typed.** Agents inject their own
context as leading pseudo user turns (`<permissions instructions>`,
`<environment_context>`, `<skills_instructions>`, memory files as
`# AGENTS.md instructions`, `claude`'s `Caveat:` preamble). Taking the first user event
verbatim labels most sessions with boilerplate byte-identical across every session of
that agent. Skipping injected envelopes raised recognizable labels from ~10% to 98% over
311 measured sessions. Keep the rule narrow: an earlier version also treated a leading
`You are ` as boilerplate and discarded 44 genuine labels, because subagent task briefs
legitimately open that way.

## Testing an agent module

Two kinds of test, and neither substitutes for the other.

**Recorded-payload tests** pin classification against real vendor JSON checked into the
repo. Every classification bug in this repo's adapter history was invisible to the live
verification script, because that script spends real tokens and only sees whatever
shapes one live turn happens to produce. The `local_bash` versus `local_agent` case is
the canonical one.

**A live verification path** runs a real turn against the real CLI and refreshes the
recorded payloads. Recorded fixtures go stale as vendors change, so this cannot be
deleted along with the adapter subprocess — it is what keeps the fixtures honest.

**A fixture's size and timing are both part of the contract.** A two-item model list
cannot observe an unscrollable, unsearchable catalogue — real agents report a dozen to
twenty-one, and every symptom was invisible to a green suite. A scenario emitting its
whole tree inside one millisecond gives every bar equal length, so a client computing
durations from the wrong field passes everything. And a delegate that forwards nothing
cannot be timed from its interior at all, so "each delegate forwards at least one event"
is a fixture requirement rather than a nod to realism.
