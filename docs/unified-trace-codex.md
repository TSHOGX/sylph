# Codex Unified Trace Adapter

Status: **Implementation contract; replacement adapter is not implemented.**

This specification maps Codex evidence into
[unified-trace-foundation.md](unified-trace-foundation.md). The foundation owns semantic
objects, identities, correction, and ordering. This document owns vendor interpretation;
[unified-trace-storage.md](unified-trace-storage.md) owns retention and publication, and
[unified-trace-conformance.md](unified-trace-conformance.md) owns the common acceptance
gate. Existing control behavior continues to follow [agents.md](agents.md) until its
replacement is implemented. The existing renderer does not constrain these mappings.

## Evidence baseline

The examined source is
[Codex revision 968835997714baaff199cfed5f89a2c65d8ca77d](https://github.com/openai/codex/tree/968835997714baaff199cfed5f89a2c65d8ca77d).
Each capture also records its actual CLI version and history mode. An upstream checkout
is evidence of a contract, not proof that an installed binary produces every field.

[The structural inventory](measurements/unified-trace/codex-structure.json) pins the
three long roots, the direct child files used to inspect their inheritance boundaries,
and eleven physical rollouts for one additional logical thread. Its samples keep
identities, field names, counts, and content digests; prompts, executable text, output,
and credentials are absent. [The audit script](measurements/unified-trace/audit-codex.py)
reads the local files without running anything from them:

```sh
python3 docs/measurements/unified-trace/audit-codex.py --check docs/measurements/unified-trace/codex-structure.json
```

This checks the structural measurement against the source inventory. It is not an
adapter conformance result, and it requires those local sources to remain present and
unchanged. The retained structural samples remain reviewable when the source is absent;
they cannot reconstruct the omitted payload. The older
[app-server captures](measurements/codex/README.md) independently establish live envelope
shapes, including approvals, errors, subscription, and delegation.

The long roots establish the following distinctions:

| Root | Evidence | Required interpretation |
| --- | --- | --- |
| `01a067f8-de0e-77a0-ac73-e484ccc9959a` | 1,918 model `exec` invocations, four replacements, one call without an observed output | Preserve incomplete invocations; an exec call is not a terminal process or a complete runtime trace |
| `01a083c2-1389-77c2-a16c-302aa2bbf884` | Seventeen direct child identities, twenty-two completed activities, six root activations, one `UserMessage` display item | Notifications, activations, children, and human inputs are different cardinalities |
| `01a09de6-c5c3-7060-875a-44cbd4a91a60` | Forty-nine `clock.sleep` items; both root `task_complete` records carry errors | Sleep is an operation; a completed boundary does not mean successful execution |

These counts are observations of the pinned files, not live catalogue totals. The
inventory also shows exact per-response usage alongside repeated cumulative counters.
The schema interpretation below is established by source plus these payload shapes,
not by counting event names alone.

## Capture boundary

The envagent owns app-server I/O, native-file discovery, selected-rollout metadata
reads, sidecar discovery, and lexical capture. The server's pure Codex mapper interprets
retained observations. Reprojection uses no vendor process and no envagent filesystem;
it works while the environment is offline.

Every received JSON-RPC frame and native record is retained before interpretation,
including unknown notifications and fields. Transport receipt identifies one observed
frame; reconnect delivery and a new native observation have different occurrence
identities unless the source supplies replay identity. A native position includes its
source generation, byte extent, and ordinal. A physical filename is a source locator,
not proof of logical thread identity or the selected branch.

The capture retains metadata needed to resolve physical history references and records
the selected native rollout as a separate observation. If selection cannot be read, the
archive exposes available histories with selection unknown. It does not select the
largest, newest, or lexically last file.

Ordinary rollout persistence is selective. The pinned
[persistence policy](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/rollout/src/policy.rs)
omits many runtime notifications, including exec deltas and several collaboration
begin/end events. The opt-in
[rollout trace](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/rollout-trace/README.md)
has richer runtime evidence but uses best-effort recording. Sylph may ingest it as an
additional source when present; it is neither required nor silently synthesized.

## Identity and ownership

| Native evidence | Normalized meaning | Forbidden substitution |
| --- | --- | --- |
| `session_meta.payload.id` | Native AgentThread ID in the paired environment's Codex namespace | The filename UUID or header `session_id` |
| Header `session_id` | Root thread identity used by the vendor's multi-agent accounting | Owner of every child message |
| Header `parent_thread_id`, or typed spawn source parent | Spawn parent evidence | A reply recipient or current viewer |
| Header `forked_from_id` and `forked_from_ordinal_exclusive` | Logical context ancestry and its exclusive boundary | Spawn parent or physical file identity |
| Header `history_base.thread_id` | Physical rollout ID of a referenced prefix | Logical AgentThread ID after revert |
| Live `threadId` | Emitting/owning thread for that frame | Parent thread whose connection carried the frame |
| `turn_id`, or live `turn.id` scoped to `threadId` | Activation identity | Call ID, response ID, or currently open parent activation |
| `SubAgentActivity.agent_thread_id` | Affected agent reference | Activity `id` or `event_id` |
| `response_item.call_id` | Model invocation identity in its vendor scope | A runtime `exec-*` item ID or process handle |
| Usage `response_id` | Completed model response accounting identity | Activation, root activation, or delivery identity |

These fields are defined by
[SessionMeta and protocol events](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/protocol/src/protocol.rs).
The public identity remains the native session triple. An independently discovered
child already has the same AgentRef as its explicitly identified embedded appearance;
no parent-local synthetic agent is created first. An unresolved path/name remains an
unresolved reference, outside real-agent counts.

An agent path is scoped to its owning multi-agent root and supported topology evidence.
Two roots can both contain `/root/worker`. A nickname, role, or task name is metadata;
it is not a merge key. When child headers and parent observations disagree about
parentage, preserve the conflict rather than replacing a parent edge.

## Content and delivery

The mapper retains both native content channels. `history_mode` selects the vendor's
documented display-item format; it does not authorize dropping `response_item` records.
Live camel-case app-server items and native Pascal-case `TurnItem` variants decode into
the same facts. Vendor-supported legacy records remain supported as vendor contracts;
there is no compatibility path for Sylph's replaced internal event model.

| Observation | Mapping |
| --- | --- |
| `response_item/message` | Immutable message content with role/channel and source occurrence; role alone does not prove a human author |
| Native `UserMessage.id`, optional `client_id`, and live user-message equivalent | Delivery/content evidence with their distinct vendor/client namespaces; link an accepted Sylph input only through the supported client/request correlation |
| `AgentMessage` or response assistant message | Authored assistant content; preserve analysis/commentary/final distinction where present |
| `response_item/agent_message` | Routed communication with `id`, author, recipient, and exact recorded content |
| `inter_agent_communication_metadata` immediately preceding that agent message | Associated routing/trigger evidence; preserve the pair's source adjacency |
| Reasoning summary, raw reasoning, encrypted reasoning | Distinct payload representations with readable/opaque coverage; absent plaintext is not absent evidence |
| A model function/custom call | Invocation message content linked to an Operation |
| Function/custom output | Result occurrence and recorded conversation admission; it is not proof of inclusion in a later inference |

The pinned
[UserMessageItem](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/protocol/src/items.rs)
keeps `id` and `client_id` separate. Source IDs that exist on both live and native
observations prove correspondence only within their actual namespace and owner.
Snapshots without such proof remain separate observations with a possible unresolved
correspondence; matching text, timestamps, or adjacent arrival alone cannot erase an
occurrence.

[Communication persistence](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/core/src/session/mod.rs)
records the prepared model item in history before persisting its metadata/item pair.
This proves conversation admission at that boundary. Request inclusion requires an
observed request or another explicit vendor guarantee. Sending, accepted mailbox
delivery, history admission, and inclusion remain independent delivery stages.

An accepted, dispatched Sylph input revision is immutable. Vendor wrapping, injected
instructions, goal text, or native edits never overwrite it. A mapped native input may
refer to the same delivery and an observed vendor representation. Human authorship
comes from accepted control provenance or explicit vendor input semantics; a
`user`-role context block remains context when that is what the source establishes.

## Activations and lifecycle

Native `task_started.turn_id` and live `turn/started` open an Activation. Native
`task_complete` and `turn_aborted`, and live `turn/completed`, provide terminal facts.
Content observed without a native activation boundary keeps an unknown activation
association. A later boundary or explicit source ID can resolve it; opening a guessed
activation at the next text record cannot supply its missing start.

Native `task_complete.error` is terminal failure evidence. Live `turn.status` and
`turn.error` determine its reported result. A missing or unknown status remains
unclassified. A `final` assistant message is final content, not an independent closure
of the AgentThread. A tool failure remains that operation's diagnostic even when the
enclosing activation succeeds.

`SubAgentActivity.kind` describes activity involving `agent_thread_id`. It is not a
child activation ID. Repeated `completed` activities do not create children, and their
generic completion cannot replace an identified child's terminal error. An activity
without a child turn ID remains an activity/result observation until correlation is
proved. A child `turn/completed` closes the child's identified Activation, irrespective
of which parent's transport carried it or whether its parent is still running.

Thread closure and parent-edge closure are independent. The pinned
[AgentGraphStore](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/agent-graph-store/src/store.rs)
persists topology and edge status separately from child turn results. `close_agent`
requests, interruption requests, and successful request acknowledgments are operations;
their effects require outcome evidence. Missing child history means coverage is missing,
not that the child succeeded, failed, or is currently running.

Native `started_at` and `completed_at` are Unix seconds; `duration_ms` is milliseconds.
Live `emittedAtMs` is a notification timestamp, distinct from its execution endpoints.
Keep all three meanings. The pinned
[turn timing implementation](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/core/src/turn_timing.rs)
uses monotonic `Instant` for reported duration. A retained root activation has
6,538,000 ms between second-resolution wall endpoints and reports 3,624,174 ms of
duration. Store the discrepancy without guessing its cause or relabeling the reported
quantity as active time.

## Operations, results, and resources

`function_call.arguments` is the exact model-supplied JSON string; `custom_tool_call.input`
is the exact custom-tool input, which can be code. Preserve the lexical input before
any typed parsing. Both forms occur in the same rollout. Malformed arguments retain an
invocation with undecodable-input coverage; they do not become an empty object.
Tool routing retains both `namespace` and `name` when the source exposes them. The long
roots distinguish `collaboration.spawn_agent` from `clock.sleep`; an unqualified name
alone cannot select a built-in semantic mapping over a different registered tool.

For a direct call, matching scoped `call_id` relates its input and each returned
result occurrence. A tool output without its call remains an orphan result. Multiple
observations of one result require identity evidence before reconciliation. A returned
resource handle is a result, not proof that resource execution ended.

For code-mode `exec`, the model call and its initial response describe the invocation.
An explicitly identified runtime cell is a Resource; yielded and terminal results are
separate occurrences. A later `wait` operates on the cell. Known nested tool calls have
the cell as requester and the initiating agent/activation as owner. Completion of a
wait or initial exec response does not close unrelated tools or the terminal resource.

The richer
[runtime trace model](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/rollout-trace/src/model/runtime.rs)
exposes model call IDs, internal runtime invocation IDs, code-cell handles, terminal
operations, and immediate caller results separately. Ordinary long rollouts contain
runtime `CommandExecution` and `FileChange` items whose IDs are `exec-*`, alongside
model `exec` calls whose IDs are `call_*`. They do not expose every enclosing-cell
relationship. The mapper retains these as runtime operations with unknown nesting
when necessary. Parsing JavaScript source cannot prove that a branch ran, identify
every dynamically constructed call, or establish returned bytes.

Command runtime stdout/stderr, aggregated/formatted output, and the content selected
for model delivery are separate representations. A command status/exit code belongs
to the runtime operation or resource it describes. It must not replace a separately
captured caller-facing custom output. Polls and stdin writes target a scoped process
Resource and keep their own request, result, and timing.

Process and cell handles include their runtime incarnation in identity. Reopening a
thread can reuse a handle value. Where native history exposes creation without an
incarnation ID, the resource can use that creation occurrence as its stable address;
later observations attach only through an evidenced handle scope or explicit relation.
An unresolved poll remains inspectable instead of merging two processes with equal
handle text.

File-change evidence preserves the exact requested edit and the observed application
result. A requested patch is not an observed filesystem diff. Native web search,
image-generation, and tool-search records also remain operations/results even when
they have no ordinary `function_call` opener. Their payload types are preserved rather
than flattened through a string-only tool result.

## Runtime protocol observations

Codex rollout traces wrap existing protocol observations in a `protocol_event_observed`
record. The wrapper's `event_type` and captured EventMsg payload are both required:
turn identity comes from the payload's own `turn_id`, and seconds-resolution timestamps
remain distinct from receipt time. A coarse trace runtime boundary never overrides a
payload's explicit failure or interruption evidence. Missing payloads block semantic
mapping; identity mismatches produce diagnostics and never create a second operation.
The wrapper's `codex_turn_ended` status is an observation of the raw trace writer; the
ordinary protocol-event adapter emits `completed` for `TurnComplete` and `cancelled` for
abort reasons. Those coarse values do not establish a vendor outcome or execution endpoint.
An explicit `failed`/`aborted` runtime status is accepted only where the pinned trace
`ExecutionStatus` contract supplies it, and endpoint times remain the retained EventMsg
fields rather than wrapper receipt time.

The raw trace runtime records code-cell start, yielded/initial response, terminal response,
nested tool requester and runtime incarnation. A code-cell handle can be reused after a
thread reopen, so resource identity includes the trace rollout incarnation. A nested
wait or terminal poll is linked only when the corresponding cell/process creation is
retained; equal handle text alone is insufficient. Caller-facing tool results and raw
runtime output remain separate representations.

The raw `agent_result_observed` producer names `child_thread_id`,
`child_codex_turn_id`, `parent_thread_id`, `edge_id` and the exact carried `message`.
The optional `AgentResultTracePayload` retains `child_agent_path`, `message`, `status`
and any additional fields. The [pinned producer](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/core/src/session/mod.rs)
records it only after successful harness submission to the parent.

Sylph keeps the formatted notification as a harness-authored runtime Notice and its
structured carried metadata as a separate part. Its Delivery names the explicit child
as sender and parent as recipient, with an accepted stage. The child turn remains route
metadata: this observation creates no Activation, spawn edge, execution outcome, history
admission or inference inclusion. It does not identify the child's original answer or
the parent's later transcript occurrence by text matching. Repeated notifications remain
distinct source occurrences even when edge identifiers and bodies repeat. An explicit
envelope identity or carried-message mismatch produces a diagnostic instead of a guessed
delivery.

`collabAgentToolCall`
items use camelCase names in the live app-server and snake_case in native rollout records;
that transport distinction is part of the vendor contract.

## Delegation and long-running work

Spawn, follow-up, message, wait, interruption, and close are different operation kinds:

- Spawn success with an explicit child ID establishes the child and topology. Failure
  remains a failed operation. A header can establish a child when its spawn result was
  not captured, without inventing the missing operation.
- Follow-up delivers another task to the existing child. It starts a new Activation
  only when the vendor reports a new execution boundary.
- Send-message can deliver while a peer is active without starting another activation.
  A reply can target a parent or sibling; it does not reverse spawn ownership.
- Agent wait records the requested targets and the returned observation. A timeout is
  its own result, not a child outcome. Completion notifications and answer delivery are
  related to the child's content only where the source establishes that relation.
- `clock.sleep` is a wait operation whose requested and observed elapsed durations
  remain distinct. It does not establish a recurring schedule.

`thread_goal_updated` provides goal-state evidence. A tool-created goal and an
app-created slash goal use the same Resource model when their identity is known. If a
native snapshot lacks goal identity, it remains a scoped state observation until an
explicit operation or lifecycle establishes the instance. Objective text is not a key.
The pinned
[goal runtime](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/ext/goal/src/runtime.rs)
submits ordinary input with `turn_trigger: "goal"` when idle. That trigger is meaningful
when captured; an ordinary history lacking it does not gain inferred goal provenance
because the preceding turn completed or because a text prefix looks familiar.

## History and model context

The header's `subagent_history_start_ordinal` marks the first child-owned native
record in the vendor ordinal coordinate. In a fresh file without `history_base`,
ordinal zero is the header. For fresh copied-context creation, the pinned
[child creation path](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/thread-store/src/live_thread.rs)
sets the boundary to one plus the number of inherited records the vendor actually
persists. Count native persisted records, including records that produce no semantic
message; counting rendered items gives a different cutoff. This formula is not the
ordinal base of a reverted file.

Physical line/byte position and top-level native `ordinal` remain separate. A reverted
file's first header starts at `history_base.end_ordinal_exclusive`, not zero; subsequent
records carry their continuing ordinals. The pinned
[ordinal implementation](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/rollout/src/ordinal.rs)
defines this coordinate. A source range records physical byte extents plus observed
vendor ordinal bounds and their conversion evidence. An absent native ordinal does
not become a physical line number under an unverified assumption.

The retained revert file whose physical ID is
`01a079c3-fe4b-7850-bc8f-76868dda0e71` has its first physical record at line one and
vendor ordinal 214, equal to its recorded base's exclusive ordinal. This is a required
structural regression: replacing recorded ordinals with `line_number - 1` must fail.
Regressed, missing, or skipped ordinals remain explicit evidence/diagnostics; filtering
records cannot silently renumber the native coordinate.

The first sampled child of each long root has a cutoff of nine. Its prefix contains
two `session_meta` records, together with configuration, world-state, and content
records; its first local record is `thread_settings_applied`. The governing first
header identifies the captured child source. A copied parent header or settings record
inside the inherited prefix cannot replace the child's identity or current settings.
Raw range membership does not promote telemetry or headers into executed history.

Inherited records are context occurrences under the child with origin ownership
preserved when known. They are not child-local activations, operations, or usage.
Missing origin identity does not authorize merging equal bytes across different
histories. Physical source references and explicit item identity may resolve the
origin later. A boundary beyond the captured tail means the child-local portion has
not been observed; it does not make inherited content local.

`compacted.replacement_history` establishes the recorded replacement content, including
ordered items and opaque payload parts. The `ContextCompaction` display item is a
boundary announcement, not the full replacement. Each checkpoint records known input
scope and replacement coverage; it does not erase the earlier historical reading
entries. Repeated logical content in a replacement is a new context occurrence without
being another execution of the original work.

`history_base` freezes a prefix using `thread_id` as the physical rollout ID,
`end_ordinal_exclusive` as the exclusive native ordinal, and `end_byte_offset` as the
exclusive physical byte offset. Its lineage can span several files. Resolve those explicit
segments before interpreting child-local boundaries; verify byte/ordinal extents and
diagnose missing, cyclic, or contradictory references. A copied-prefix source and a
referenced-prefix source preserve the same ownership distinction without sharing a
made-up physical identity. A referenced parent entry becomes a child-scoped context
occurrence retaining the original revision and owner; a same-agent reverted prefix
keeps its original entries. Large inherited prefixes expand in bounded, checkpointed
pages, and selection is declared only after the complete prefix has been verified.
A restart resumes unfinished pages without requiring another admitted capture.

Revert changes selected history under the same AgentThread. The native
[rollout resolver](https://github.com/openai/codex/blob/968835997714baaff199cfed5f89a2c65d8ca77d/codex-rs/thread-store/src/local/thread_rollout_resolver.rs)
uses the live writer or selected SQLite metadata because scanning can return an older
immutable rollout. Sylph captures that selection as HistorySelection evidence. Old
suffixes stay available as retained history; they are excluded from the selected
conversation unless an explicit selection includes them. Fork instead establishes a
new thread and context ancestry. Neither operation transfers historical authorship,
and revert is not a claim that filesystem changes were undone.

## Measurements and coverage

`token_usage_record` contains `response_id`, owning `thread_id`/`turn_id`, causal
`session_id`/`root_turn_id`, one response's `usage`, and turn/thread cumulative values.
Normalize these as separate scoped measurements. Response usage is identified by its
vendor response and owner; turn/thread snapshots identify counter series and boundaries.
The three long roots contain 166, 1,347, and 334 distinct response usage records,
respectively. Their presence is explicit evidence; their absence cannot be filled with
synthetic inference operations.

`token_count.info` can be absent. When present, `total_token_usage` and
`last_token_usage` keep their declared cumulative/latest semantics. Neither repeated
latest values nor cumulative snapshots are summed as new usage. Inherited checkpoints
do not increment local accounting, resets establish a new compatible series boundary
only when evidenced, and an unknown baseline leaves derived deltas unavailable.
Cached-input and reasoning values remain vendor-defined subcounts, not extra tokens
added to totals. Goal/root accounting and child usage are not presumed disjoint.

Coverage distinguishes conversation content, exact model request inclusion, runtime
nesting, child internals, timing, opaque reasoning, result representation, and selected
history. Native final items can give complete text and incomplete execution timing at
once. A live observer can miss early frames while native history supplies the content.
Combining them improves only the facts actually proved by their joint evidence.

## Replacement and acceptance

The rewrite replaces the following semantic mechanisms in place:

| Existing location | Removed responsibility | New owner |
| --- | --- | --- |
| `crates/adapters/src/codex/live.rs` `Translator`, `thread_span`, and delegation-host maps | Inferring a child trajectory by rewriting `span` | Pure fact mapping with explicit AgentRef and activation ownership |
| `crates/adapters/src/codex/native.rs` trajectory declarations/forwarded closes | Promoting tools and activity items into lifecycle containers | Agent, operation, topology, and lifecycle facts reduced independently |
| Native event fingerprints | Deduplicating semantic occurrences by normalized content | Source occurrence identity and explicit cross-source equivalence |
| Native compaction announcement handling | Retaining only a display sentence as context history | Immutable checkpoint/replacement facts and retained source payloads |
| `crates/adapters/src/codex/mod.rs` per-format semantic assembly | Separate live/native interpretations of the same fact | Shared server mapper; envagent retains acquisition/control responsibilities |

The process transport, configuration validation, approvals, control request correlation,
and native byte-reading mechanisms can be reused where they satisfy the new capture
contract. Their authority and bounds remain explicit. No old block/span projection is
kept as a fallback consumer.

Before replacement publication, adversarial cases demonstrate failures for wrong
identity scope, equal message bodies, duplicate/reordered live and native evidence,
inherited calls and usage, orphan results, failed spawn, repeated child activations,
late child errors, a parent ending before a child/process, exec yield plus later waits,
runtime/caller output disagreement, missing goal trigger, opaque reasoning, incomplete
compaction, and an unavailable selected revert head. The same cases run incrementally,
from cold evidence, and from mixed sources under the common conformance contract.
The structural audit above supplies grounded shapes and source digests; full fixtures
and the replacement mapper still have to establish those semantic promises.
