# Pi Unified Trace Adapter

Status: **Target implementation contract.**

This specification maps Pi's session tree and runtime events into
[unified-trace-foundation.md](unified-trace-foundation.md). Acquisition and process
control remain in the envagent. Deterministic semantic mapping runs on the server over
retained evidence, under [unified-trace-storage.md](unified-trace-storage.md).
[unified-trace-conformance.md](unified-trace-conformance.md) owns the common acceptance
protocol. Existing control and vendor-era facts remain in [agents.md](agents.md#pi).

## Evidence baseline

The source reference is
[earendil-works/pi at bbb61e34aaf231639fdaaad1adbd757947034eac](https://github.com/earendil-works/pi/tree/bbb61e34aaf231639fdaaad1adbd757947034eac),
whose coding-agent package is 0.85.1. These links are pinned:

| Source | Load-bearing behavior |
| --- | --- |
| [SessionManager](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/session-manager.ts#L32) | Header session ID; entries with `id` and `parentId`; custom entries/messages; compaction and branch summaries |
| [Persistence and append](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/session-manager.ts#L993) | Truncate-rewrite, delayed first flush, and new entry ID allocation inside `appendMessage` |
| [Branch selection](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/session-manager.ts#L1369) | In-session branch changes move the leaf; historical entries remain |
| [AgentSession event handler](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/agent-session.ts#L625) | `agent_settled`, message-end extension processing, event forwarding before persistence |
| [Outer run and continuation](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/agent-session.ts#L1101) | Retry, compaction, and queued continuation can follow an inner agent end |
| [Context conversion](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/messages.ts#L148) | Custom, bash, branch-summary, and compaction-summary content convert independently of their UI roles |
| [Inference request construction](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/agent/src/agent-loop.ts#L279) | `transformContext`, then `convertToLlm`, then model request; native entries are not exact request evidence |
| [RPC protocol](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/modes/rpc/rpc-types.ts#L20) | Request IDs, queue controls, and entry/tree snapshots with explicit selected `leafId` |

[pi-branch-structure.json](measurements/unified-trace/fixtures/pi-branch-structure.json)
retains a sanitized structural extract of a real 73-entry native file.
[vendor-structure-inventory.json](measurements/unified-trace/fixtures/vendor-structure-inventory.json)
records source and artifact digests, locators, and extraction limits. The source contains
two sibling pairs of user messages, following a length-limited response and a completed
response respectively. The historical source's producing binary version is unknown;
its header `version: 3` identifies a file format, not the current source revision.

Existing native fixtures under `fixtures/native/pi/` also cover tool results, failure,
missing stop reasons, mixed vendor field spellings, and cross-file fork pointers.
Historical vendor spellings remain legitimate external contracts. This does not permit
compatibility aliases for Sylph's replaced internal model.

The controlled Pi 0.85.1 success pair in
[`fixtures/unified-trace/manifest.json`](../fixtures/unified-trace/manifest.json) was
captured from the installed CLI by `scripts/capture-pi-trace.mjs`. A loopback provider
emits deterministic marker chunks, including two separate equal text deltas; Pi emits
its own RPC events and writes its own native session. Tools, extensions, context files,
personal configuration and startup network operations are disabled. This proves the
observed Pi framing, streaming and persistence slice, not remote provider behavior or
a live/native message identity certificate. The provider's content is synthetic and
the fixture metadata labels it accordingly.

## Acquisition and source generations

Managed live execution captures raw stdout frames, exact RPC writes and responses,
native JSONL generations, entry snapshots when available, and process-lifecycle
evidence. Native-only observation reads the same JSONL sources without acquiring a
control channel. Reprojection never executes a Pi command or reads the current store.

The capture cursor advances past complete newline-terminated records only. The pinned
writer appends `JSON.stringify(entry) + "\n"`; a syntactically complete JSON object without
the terminator is still an incomplete final record. On a changed-file pass, capture
validates the already consumed prefix, not just inode and length.

| Source transition | Archive action | Semantic action |
| --- | --- | --- |
| File absent before the first assistant message | Keep pending inventory and retry | No absent-session or completed-activation claim |
| Empty or incomplete header during truncate-rewrite | Retain generation/source-state evidence | Publish no synthetic empty replacement history |
| New complete appended line with verified prefix | Retain occurrence at its generation/position | Apply native entry identity and predecessor facts |
| Inode replacement or consumed-prefix change | Start a new source generation | Retain old evidence; reconcile entries by native IDs and explicit revision evidence |
| Same entry ID with changed bytes in a later generation | Retain both occurrences and their relationship | A documented mutable-field/rewrite rule may establish a new representation revision; otherwise preserve conflicting claims |
| Same entry ID and bytes across a proved rewrite lineage | Retain the new source occurrence | Confirm the same semantic entry; do not create another conversation item |
| Duplicate ID with conflicting ownership or incompatible predecessor in one coherent source view | Preserve both and report conflict | Do not choose whichever record arrived last |

The fingerprint is a rewrite detector and payload address, never an occurrence identity.
Two equal messages with distinct entry IDs remain two messages.

A generation change alone does not authorize semantic correction. Incompatible owner
or predecessor claims remain conflicts unless explicit source lineage establishes a
different copied-history membership; they are not repaired by last-arrival wins. A
mutable content field changes only under the recorded vendor rule for that field.

Pi's read APIs can repair legacy or malformed files on load. The envagent's capture
reader does not call those APIs on user-owned native history; it reads bytes without
mutating the vendor's store. A managed process may perform its own documented rewrite,
which becomes captured source-generation evidence.

## Identity and the history tree

| Evidence | Unified contract |
| --- | --- |
| Session header `id` | Session AgentRef `(environment, pi, native_id)` |
| Entry `id`, scoped to the session and proven source lineage | HistoryEntry occurrence and canonical message/config/checkpoint address |
| Entry `parentId` or the measured historical `parent_id` field | History predecessor within this session, including non-message entries |
| Tool-call content `id`, with owning agent and message-part scope | Operation identity |
| Tool-result `toolCallId` | Reference to that Operation; missing invocation remains an orphan result |
| RPC producer epoch and message-start ordinal | Provisional live message/part identity where native entry identity is not yet available |
| `contentIndex` inside the current assistant message stream | Part index, never a session-wide identity |
| Header `parentSession` or historical `parent_session` | A source path requiring capture of the referenced header to resolve actual fork Session identity |

The entry tree includes model settings, thinking settings, custom entries, labels,
session metadata, messages, and checkpoints. Omitting a non-message node from a reading
projection does not remove its predecessor relationship or arbitrarily reparent its
children. The server can walk through non-visible entries while keeping the actual
history edges available.

Physical append order answers when an occurrence was recorded. `parentId` answers which
history it extends. They are different orders. In the observed branch extract, source
lines 12 and 14 both extend the entry at line 11; lines 42 and 44 both extend line 41.
A renderer joining consecutive physical lines into one context would include abandoned
branch messages in the later model context.

Branch navigation within one session creates no new AgentThread. `branch()` changes
only the live leaf pointer; navigating without another append can leave no native-file
evidence of the selected head. The latest appended entry is therefore a last-recorded
head, not a claim about a still-running process's current selected context.

Managed capture uses supported RPC `get_entries` / `get_tree` responses to observe
`leafId` explicitly. Each response is a snapshot with its own observation sequence and
coverage. A later head selection replaces the selected-head projection at that evidence
cut; it does not delete the old branch. Native-only capture exposes selected head as
unknown where no source establishes it. A chosen historical reader branch is view state.

Cross-session fork and clone create a new native session header. Copied entry IDs do
not become globally unique, and copied input remains inherited context. The source
[branched-session writer](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/session-manager.ts#L1424)
can remove label nodes and reconnect the copied path. Therefore equal entry IDs alone
do not prove identical predecessor membership across sessions. Retain the fork source,
captured membership, and any rewritten edge relationships explicitly.

## Messages, provenance, and delivery

| Source shape | Message or payload meaning |
| --- | --- |
| `message.role: user` | User protocol role; actual author needs server submission or source evidence |
| `message.role: assistant` | Agent-authored content with ordered text, thinking, and tool-call parts |
| `message.role: toolResult` | Tool/caller result; `toolCallId`, `isError`, `content`, and `details` remain separate |
| `custom_message`, `customType`, `display`, `content`, `details` | Extension/context Message; `display: false` is a TUI visibility choice, not model exclusion |
| `custom`, `customType`, `data` | Durable extension state; not automatically conversation content or model context |
| `bashExecution`, `excludeFromContext`, `output`, exit data | Human/extension shell execution and its recorded representation; no model-origin tool call is invented |
| Unknown entry or content-part type | Opaque retained evidence with a visible coverage diagnostic |

The exact server submission, RPC payload, accepted command response, native admitted
message, and model request are separate representations and delivery stages. RPC
`success: true` may mean an input handler accepted or handled a command without any
model run. It does not prove consumption of the original bytes.

The source [input pipeline](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/agent-session.ts#L1180)
runs extension commands and input transforms before skill/template expansion and queue
admission. Its user Message construction does not preserve the command request ID or
the input-source enum in the native message. Extension `message_end` handlers can also
replace content before persistence. The immutable submitted human revision consequently
cannot be overwritten with native text merely because it arrived next.

RPC `steer` and `follow_up` are queueing operations. Their IDs correlate request and
response, not the later consumed user message. A returned queue string and a matching
user text are not an identity join: equal submissions can coexist and extensions can
transform content. Sylph keeps known dispatch/queue state and marks consumption association
unknown unless a retained certificate or explicit source binding establishes it.

Native custom messages can become user-role model messages while remaining authored by
an extension/harness. `bashExecution.excludeFromContext` excludes that shell output from
the ordinary conversion. Exact inference inclusion still requires request evidence:
`transformContext` and `convertToLlm` can alter the final set after native context building.
The same rule applies to system prompts assembled from resources and extension overrides.

## Activation and operation lifecycle

Pi exposes nested loop boundaries. The unified Activation follows the outer active
interval; an inner `turn_end` is not a human exchange and an inner `agent_end` is not
proof that the outer invocation settled.

| Live observation | Mapping |
| --- | --- |
| First `agent_start` while no outer run is known active | Open one Activation with source-local start identity |
| Another `agent_start` before settlement | Inner retry/continuation evidence inside the same Activation |
| `turn_start` / `turn_end` | Model-loop step boundaries; a step can contain an assistant response and several tools |
| Assistant `message_start` / updates / `message_end` | Message/part lifecycle and immutable terminal representation |
| `tool_execution_start` | Operation begins with `toolCallId`, name, and actual arguments |
| `tool_execution_update` | Result-progress snapshot or segment according to the concrete event shape |
| `tool_execution_end` | Operation's execution result and error flag, separate from the later admitted tool-result message |
| `agent_end` | End of the inner run, retaining `willRetry` when exposed; outer Activation remains open |
| `auto_retry_*`, `auto_compaction_*` | Retry/context work, with its own observed lifecycle and diagnostics |
| `agent_settled` | End of the outer active interval; correlate the terminal outcome with the final actual work |
| Process exit, aborted transport, or native EOF | Source/supervisor fact; EOF does not create a vendor outcome |

The [outer run loop](https://github.com/earendil-works/pi/blob/bbb61e34aaf231639fdaaad1adbd757947034eac/packages/coding-agent/src/core/agent-session.ts#L1101)
can retry after an assistant error, compact, or continue messages queued by an
`agent_end` extension. `agent_settled` has no outcome payload. Its outcome is derived
only from associated terminal assistant/error/cancellation evidence and the observed
outer run scope. A prior failed attempt followed by a successful terminal attempt does
not leave the entire Activation failed; the failed attempt remains visible as a
diagnostic/inference result. No terminal result means unknown outcome.

An assistant `stopReason` describes its model response: `toolUse`, `stop`, `length`,
`aborted`, and `error` are not interchangeable. Message end with `toolUse` cannot finish
the enclosing Activation. An aborted response preserves its partial content and
termination reason. A successful activation may contain failed tools.

Native JSONL persists completed messages, not the full outer lifecycle. User-message
boundaries, `stopReason: stop`, file EOF, and wall-clock gaps do not prove a settled
outer interval. Native import keeps unknown Activation association where evidence is
missing rather than synthesizing a turn for each user-role record.

Pi's built-in tool set supplies no universal subagent identity protocol. An extension
named `subagent` remains an Operation until versioned extension evidence supplies an
actual child ID, parent relation, delivery, and child trace source. A textual result
claiming that several agents ran is not a directory of independently observed agents.
Such extension mappings use the foundation's embedded/native AgentRefs without adding
a second trajectory model.

## Live/native reconciliation

`AgentSession` emits `message_end` to listeners before `SessionManager.appendMessage`
allocates the native entry ID. Standard live messages do not carry that entry ID. The
adapter must therefore not promise universal equality between a live message reference
and a native entry by comparing body, timestamp, or array position alone.

Native entry IDs anchor persisted conversation addresses. Live fragments
use stable provisional references and provide stream representation evidence. An
explicit vendor entry ID, or a retained acquisition certificate proving ordered
one-to-one correspondence, can bind the two references without changing either owner.
Unproven equivalence remains visible as unresolved coverage.

A sequence certificate identifies the producer epoch, initial entry snapshot,
complete live frame interval, later native-entry snapshot, the source mapping rule,
and every selected pair. It is valid only when that interval has no capture gap and
the source contract proves a unique order-preserving correspondence, including custom
messages and messages inserted without normal live events. A reader lagging behind
`message_end` waits for a later snapshot; absence at one poll is not failure.

Direct extension writes, independent process writes, repeated indistinguishable
candidates, source rewrites, and an unknown baseline can invalidate such a certificate.
The adapter retains the streams separately in that case. The UI can show a provisional
stream continuation separately from persisted entries and expose the unresolved mapping;
it cannot silently merge on likely text similarity. Full body preservation does not
depend on whether a canonical join was possible.

`get_entries(since)` is an entry append query, not a branch query: the source filters by
physical array index and returns `leafId` separately. Managed capture records both the
append window and the head. Reconnect requests a new verified baseline; it does not use
an old live message ordinal in a new process epoch. Sources that lack the supported
snapshot commands retain native/live coverage independently.

The vendor's snapshot response has no page-size parameter. Capture prefers incremental
`get_entries(since)` with a verified known entry, and does not poll the entire `get_tree`
surface to refresh a head. An initial or lagging response can still be large; bounded
capture chunks and the storage resource-limit policy govern its acquisition. The
adapter reports a capture gap if it cannot retain the response, rather than silently
truncating it and issuing a complete sequence certificate.

## Context, payloads, and measurements

Compaction entries preserve `summary`, `firstKeptEntryId`, `tokensBefore`, `details`,
`fromHook`, and usage. They establish a ContextCheckpoint with kept/replaced membership
where the referenced branch and entries are available. Branch summaries additionally
preserve `fromId`, distinguishing the abandoned branch whose summary was produced from
the branch receiving it. Summary text alone cannot supply an exact omitted membership
set or exact inference request.

Live update objects can contain a full partial-message snapshot as well as the delta
that led to it. The mapping classifies append versus replacement at the concrete field
level; it never appends the full snapshot repeatedly. Partial tool arguments remain
structured only when parsing is complete; sealing arguments does not signal execution
completion. Native final content can differ from streamed content because extension
hooks rewrite the final message. Both observed representations survive.

Tool `content` and `details` are distinct payloads. Binary/image content keeps media
type and encoded bytes. Bash full-output sidecar paths are captured under environment
containment; missing sidecar bytes remain unavailable. Exact provider requests,
uncaptured stderr, and hidden reasoning are not reconstructed from summaries.

Native `message.timestamp` is milliseconds; outer entry timestamps retain their own
reported clock interpretation. An offsetless historical wall-clock timestamp does not
become UTC or acquire a guessed timezone. Its interpretation and uncertainty remain
recorded, particularly when no capture-time timezone evidence survives. A sibling
epoch timestamp can establish a time for that message, not a universal timezone for
every older entry.

Message usage, compaction usage, branch-summary usage, and session aggregate snapshots
have different scopes. Usage on a failed attempt is still reported usage. Cost and
token counters retain their units and reset/cumulative domains. A session total is not
summed with its component inference totals. Stream arrival intervals are receipt-time
measurements; native timestamps do not reveal token duration.

## Coverage and replacement

| Dimension | Managed live | Native-only |
| --- | --- | --- |
| Session/entry identity and history predecessor | Native or entry-snapshot evidence | Native evidence |
| Current selected head | Explicit RPC snapshot when available | Unknown beyond recorded head evidence |
| Stream text and operation runtime boundaries | Captured event coverage | Unavailable beyond recorded messages |
| Outer Activation settlement | `agent_settled` plus associated outcome | Generally unavailable |
| Submitted human request | Exact server submission | Unknown author unless explicit source evidence |
| Exact live/native message equivalence | Explicit IDs/certified mapping only | Not applicable to absent live stream |
| Model request inclusion | Requires additional captured request evidence | Unavailable |
| Extension subagent internals | Requires a supported extension mapping | Requires identifiable retained child sources |

Implementation removes native-to-live event fabrication, user-message-as-turn
segmentation, canonical part IDs derived only from a mutable message counter, text-based
input correlation, and loss of non-message predecessor nodes. It replaces the current
live-native policy that retains native rows while discarding all their semantic facts.
Native rows now contribute identity, context, corrections, and additional content
through the same mapping as live evidence. Process controls, source-generation detection,
vendor field-era recognition, and durable raw retention are preserved. Removed internal
schemas have no migration or compatibility layer.

## Falsifiable acceptance

| Deliberate fault | Required failing assertion |
| --- | --- |
| Flatten entries by file order | Observed sibling user entries remain separate branches; neither branch consumes its sibling's content |
| Drop model-change/label nodes before building predecessors | Every known predecessor edge resolves through retained history entries |
| Use last file row as a live selected-head claim | Branch navigation without append leaves native-only selected head unknown; an RPC `leafId` snapshot selects the observed head |
| End Activation on `agent_end` | A retry/compaction/queued continuation before `agent_settled` remains within the same outer Activation |
| Treat message `stopReason: stop` as native settlement | Native-only content survives without fabricated outer lifecycle |
| Replace dispatched input with the next user message | Transformed or repeated messages cannot overwrite the immutable human submission |
| Merge repeated equal messages by content hash | Distinct native entry IDs retain separate occurrences and uncertain live joins remain unresolved |
| Treat `custom.display: false` as model exclusion | Custom context conversion is preserved independently of transcript visibility |
| Read an entry snapshot as a context branch | Returned append window and selected `leafId` remain independent facts |
| Use mtime/length only for native cursors | Equal-length consumed-prefix rewrites create a new generation and preserve corrections |
| Append each complete partial snapshot | Final streamed content contains each actual segment once and keeps terminal replacements separately |
| Add session/child/inference usage indiscriminately | Disjoint scopes and cumulative resets prevent fabricated totals |

Tests replay source occurrences incrementally, after restart, in mixed source order,
and as a cold rebuild. New tests are proven by making the intended semantic fault fail.
Structural extracts prove the observed tree shape; separate sanitized live recordings
must prove stream/native certificates and retry settlement. Synthetic extension scenarios
remain labeled synthetic and cannot establish universal Pi behavior.
