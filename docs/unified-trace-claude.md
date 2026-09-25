# Claude Unified Trace Adapter

Status: **Normative vendor mapping contract.**

This specification maps Claude Code evidence into the objects and invariants in
[unified-trace-foundation.md](unified-trace-foundation.md). Acquisition runs in the
envagent; semantic decoding and reconciliation run as a deterministic server library
over retained observations. [unified-trace-storage.md](unified-trace-storage.md) owns
durability and publication; [unified-trace-conformance.md](unified-trace-conformance.md)
owns shared verification. Existing control and authentication rules in
[agents.md](agents.md#claude) continue to govern the process transport.

## Evidence baseline

The native observations are retained as a sanitized structural extract in
[claude-structure.json](measurements/unified-trace/fixtures/claude-structure.json).
[vendor-structure-inventory.json](measurements/unified-trace/fixtures/vendor-structure-inventory.json)
records source locators, source and extract SHA-256 digests, the extraction policy,
and limits. Source line numbers below refer to original fixtures, not extract rows.
The extract deliberately omits prose and tool bodies; it proves identity equality,
field placement, ordering, and topology, not exact content preservation.

The public live contract is
[`@anthropic-ai/claude-agent-sdk@0.2.126` declarations](https://unpkg.com/@anthropic-ai/claude-agent-sdk@0.2.126/sdk.d.ts),
which bundle CLI 2.1.126. The inventory includes the declaration digest. Native record
versions are retained separately: a historical fixture is not attributed to that SDK
version merely because the SDK is the type reference. Unsupported or changed required
fields produce visible mapping diagnostics while the complete observation is retained.

| Evidence | What it establishes |
| --- | --- |
| `fixtures/native/claude/ordinary-turn-session-start.jsonl`, lines 8–11 | Four distinct record UUIDs share one `message.id`; each contains a separate thinking, prose, or tool block |
| `fixtures/native/claude/injected-boilerplate-slash-command.jsonl`, lines 1–6 | Several user-role occurrences and `isMeta` metadata share a `promptId`; another input records `promptSource: sdk` |
| `fixtures/native/claude/delegation-task-fanout-nested.jsonl`, lines 1–21, and adjacent `.meta.json` sources in the inventory | `Agent` invocations return actual child IDs; child-owned calls and metadata establish nested agents independently of filesystem depth |
| `fixtures/native/claude/compaction-boundary.jsonl`, lines 4–7 | A compaction boundary, transcript-only summary, retained segment references, and local command records have distinct meanings |
| `fixtures/native/claude/fork-replay-messageuuid-mirrors-own-uuid.jsonl` | `forkedFrom.messageUuid` repeats each copied record's own UUID; it is not a universal fork cutoff |
| SDK declarations, lines 2334–2341, 3387–3460, 3481–3524 | Assistant/user UUIDs, task notifications, task updates, source provenance, and `shouldQuery` are separate fields |

Acquisition lives in `crates/adapters/src/claude/`; semantic interpretation lives in
`crates/trace/src/mapping/claude.rs`. Captured host identifiers and native records are
interpreted by the mappings below and cannot promote transport containers into agents.

## Capture inventory and source state

The capture inventory includes the root transcript, delegate transcripts, delegate
metadata sidecars, workflow manifests and journals, persisted output sidecars, live
stdout, control receipts, and stderr diagnostics. Their paths locate evidence; they
are neither agent identity nor semantic order.

Each JSONL source has a persisted source identity, generation, complete-line cursor,
and byte inventory. A live stdout source additionally records a producer execution
epoch and frame ordinal before delivery through the durable spool. Restart does not
reuse an old producer epoch. JSON snapshot sidecars have complete snapshot occurrences;
the digest detects changed bytes but does not collapse independent observations into
one occurrence.

Capture follows these transitions:

| Observed source state | Capture action | Semantic consequence |
| --- | --- | --- |
| Missing source | Retain inventory absence for this scan and retry on a later inventory change | Interior evidence is unavailable; the agent is not declared nonexistent or finished |
| Incomplete JSONL tail or partial sidecar | Preserve available source evidence and diagnostic; do not publish a successfully decoded record | Cursor stays before the incomplete record |
| Complete new line or snapshot | Publish payload before its durable reference, then send observation identity | The server may map after archive admission |
| Source replacement or rewritten consumed prefix | Start a new generation with retained relationship to the old generation | No file offset is reused as the identity of old content |
| Decoder cannot understand a complete record | Archive exact bytes and structured failure | Semantic coverage reports unsupported or malformed evidence |

A resume captures the native baseline before launching Claude, then hydrates its
correlation facts from that baseline. New delegate bytes arriving between launch and
`system/init` remain new observations. Adoption, native watch, and live attachment use
the same inventory definitions. Replay consumes captured files and manifests without
looking at the current filesystem or contacting a live Claude process.

Persisted output paths are resolved and read only by the envagent under its existing
containment policy. A source recording a path is not evidence that its file existed,
was complete, or held model-visible output. Capture reports each fact separately.

## Identity, content, and ordering

| Source field or relation | Unified contract | Forbidden inference |
| --- | --- | --- |
| Root `sessionId` / live `session_id` | Session AgentRef using the environment, `claude`, and native session ID | Minting another ID because the session was adopted or has children |
| Delegate native `agentId`, with captured root association | `embedded(root_session, "claude.agent", agentId)` AgentRef | Using a tool call ID, title, model, or sidecar depth as child identity |
| Native content-bearing record `uuid` | Message occurrence and corresponding HistoryEntry identity scoped by native conversation provenance | Treating an equal UUID copied into a fork as fresh child-authored work |
| `message.id` | Scoped vendor model-message grouping reference attached to each corresponding Message | Using it as canonical Message identity or merging distinct record occurrences |
| Native `uuid` plus source content-block index | Stable MessagePart occurrence address | Reusing index zero across several record UUIDs |
| Live stream epoch, actual message-start identity, and `content_block_* .index` | Stable live part reference and ordered update stream | A single global current message ID for multiplexed agents |
| `tool_use.id` and `tool_result.tool_use_id` | Operation reference within the correct agent namespace | Reading the pairing ID from top-level `toolUseResult` |
| Native `parentUuid` / explicitly documented logical ancestry | History predecessor evidence | Inferring agent spawn parentage from message predecessor links |
| `promptId` | Source correlation group, retaining its scope | A Message ID, unique human input ID, or globally unique activation ID |

Each native content-bearing record is a Message with its own ordered parts, keyed by
record UUID and block index. The four observed records sharing `message.id` therefore
produce four Message occurrences, each with one part, retaining the same scoped vendor
model-message grouping reference. That reference is metadata for correlation and
inspection; it neither creates a separate core MessageGroup object nor merges those
Messages. Native capture does
not synthesize a `message_start` and does not manufacture stream indices from the
position of a record in a file. A native completed block records its observed completion
time when present. It does not acquire a measured start time by replaying invented
start/stop events at the same instant.

Live content revisions are readable as soon as a content block starts. Each observed
revision has a history occurrence referencing the same live Message; those occurrences
let a reader choose an included prefix without inventing conversation admission. A live
Message remains scoped to its actual message-start occurrence, separately from SDK
assistant records whose UUIDs identify recorded messages.

The Agent SDK's `SDKPartialAssistantMessage.event` is the Anthropic SDK's
`BetaRawMessageStreamEvent`. Its pinned dependency's
[`BetaMessageStream` accumulator](https://unpkg.com/@anthropic-ai/sdk@0.81.0/src/lib/BetaMessageStream.ts)
establishes the field rules: block-start text/thinking bytes precede deltas; text and
thinking deltas append; `signature_delta.signature` replaces the signature; and
`input_json_delta.partial_json` accumulates into tool input. A signature is a separate
opaque part, not disclosed thinking. Stable live part identity includes the block index
and field, so an opaque signature cannot change a reasoning part's kind.

A signature update publishes a replacement snapshot of the observed message prefix.
Tool JSON fragments remain exact opaque bytes while incomplete; block stop parses the
complete accumulated JSON into the tool block's `input`. Invalid final JSON stays opaque
with a decode diagnostic instead of keeping a plausible empty input. These content
boundaries do not establish tool execution or delivery admission. A message-stop final
preserves part identities and all accumulated content without closing the SDK loop.

The SDK's explicit `session_id` and `parent_tool_use_id` partition stateful processing
within one captured source/generation/authorization epoch. A missing child certificate
holds that partition's ordered tail; unrelated parent and child routes continue. A later
certificate resumes from the last committed partition state, including after restart.
A routing gap stops subsequent frames but does not erase a valid earlier prefix whose
certificate arrives later. The host field remains a routing coordinate, never an agent
identity. Captured `control_request`, `control_response`, `control_cancel_request` and
`keep_alive` envelopes belong to the SDK transport-control union from the pinned
declarations. They need no session ID and do not alter content-lane state or create
execution facts; the control plane owns their effects. Frames keep their
original source positions, and equivalent host bindings do not merge unproved stream
continuity between different host routes.

Per-source sequence and native history ancestry remain separate from server admission
order. The same observed timestamp can label several blocks. Cross-source timestamps
do not settle which child or operation caused a notification.

## Input and delivery

The server-owned submitted request is an immutable Message revision and Delivery
attempt. The adapter retains exactly what it writes to stdin, including any sender
UUID, priority, origin, attachments, and `shouldQuery`. A write receipt, vendor acceptance,
native transcript occurrence, and inclusion in an inference request establish different
delivery stages. None rewrites the submitted human bytes.

Claude can expand a slash command, attach context, merge non-querying input, or produce
synthetic user messages. The native record and the submitted request therefore need
not have equal bytes, equal cardinality, or a one-to-one relationship.

| Evidence | Mapping |
| --- | --- |
| Server accepted human request | Author is the authenticated human; submission revision is fixed |
| Native `promptSource: typed` with no contradictory synthetic/context evidence | Human-origin evidence under that native schema; preserve the actual record separately |
| Native `promptSource: sdk` | SDK submission path; actual author remains unknown unless server or routing evidence identifies it |
| `isMeta`, `isCompactSummary`, `isVisibleInTranscriptOnly` | Context/transcript purpose and visibility evidence; these are not extra human requests |
| SDK `origin.kind: peer` plus `from` | Another agent/session authored or routed the content; explicit reference resolution is required |
| SDK `origin.kind: channel`, `task-notification`, or `coordinator` | External channel, runtime notification, or coordinator provenance respectively |
| SDK `isSynthetic` | Synthetic origin evidence; retain other routing and source fields |
| SDK `shouldQuery: false` | Transcript admission may occur without an activation; later merging needs its own observation |
| A `tool_result` inside `message.content` | Tool/caller result message; role `user` does not make the author human |
| `interruptedMessageId` | An interrupt relation to the identified message; the prose need not match an English sentinel |

Explicit SDK `origin.kind: human` has the same human-origin meaning as an absent origin
on the documented human input surface. A channel origin names its `server`, while a
peer origin names `from`. Forwarded child input without an origin does not become human
merely because it is a user-role SDK envelope; its routing provenance is kept separately.

The SDK's documented absent-origin default applies to its particular user-message
surface after synthetic/tool/context classification. It is not a rule for every native
`user` envelope. Unknown native provenance stays unknown. Native records sharing a
`promptId`, including descendant records, do not overwrite or merge human submissions.

An emitted tool result establishes the representation delivered into Claude's
conversation surface. It does not expose the entire next model request: hooks,
compaction, filtering, and context assembly can still intervene. Exact inference
inclusion is unavailable unless captured request evidence establishes it.

## Delegation is an invocation of another agent

An `Agent` invocation is a delegation Operation owned by the invoking agent. Its
arguments can identify a proposed task and requested settings before any child exists.
Sealing those arguments does not finish the invocation, start a child, or close an
activation. A failure before child creation remains a failed Operation with no child
catalogue entry.

The same distinction applies to a built-in `Task`, a resumed agent, and a workflow
invocation. Vendor-specific built-in recognition may classify an operation as a
delegation attempt. Only child identity evidence adds an AgentThread. A global list of
tool names cannot prove which operation executed or whether it succeeded.

| Evidence | Relationship established |
| --- | --- |
| Invocation owner's AgentRef and tool call ID | Owner and identity of the parent delegation Operation |
| `toolUseResult.agentId` paired by the enclosing content block's `tool_use_id` | The invocation returned or contacted that embedded AgentRef |
| `toolUseResult.status: async_launched`, `isAsync: true` | Launch caller result; child execution continues independently |
| Synchronous result with explicit child identity and terminal status | Caller result plus that particular child execution outcome, when the source establishes the run association |
| Delegate metadata `toolUseId` | Child-to-invocation relation; the invocation's owner establishes the parent |
| Delegate metadata `parentAgentId` | Explicit child-to-parent agent relation, independent of metadata directory depth |
| Workflow result `runId` joined to manifest `runId` and `workflowProgress[].agentId` | Workflow Resource/Operation to actual child references; one host can fan out to several agents |
| Task event `task_id`, `task_type`, optional `tool_use_id` | Task resource/activity identity and routing; the native child ID requires its own documented join |
| Forwarded `parent_tool_use_id` | Multiplexing/host correlation; it is not generally equal to native `agentId` |

`local_agent`, `local_bash`, and `local_workflow` task types are distinct. Unknown task
types remain unknown task resources with retained raw detail; they do not enter the
agent directory by default. A shell background job is a process Resource. A workflow
host is an Operation/Resource connecting agents, not an extra agent itself.

Late metadata resolves an existing reference. It never changes ownership of an existing
message to whichever parent currently seems plausible. A real child with unavailable
parentage remains readable under its own embedded reference. A bare unresolved routing
tag remains an unresolved reference, not a catalogue entry. When a task handle cannot
be proved equal to a native child ID, the two namespaces remain distinct.

Follow-up or resume creates a new delivery and, when observed, a new Activation on the
same child AgentRef. Waiting, status polling, result notification, and closing a child
each have their own operations/relations. None creates another child. The parent's
receipt of a summary is a new delivery occurrence related to the child's answer; the
child's answer retains its original owner.

### Reused task handles and forwarding

The controlled [delegation capture](../fixtures/unified-trace/claude/delegation/capture.json)
uses Claude 2.1.278 with a loopback provider, first calling `Agent` synchronously and
then `SendMessage` to the returned child. The follow-up result explicitly names
`resumedAgentId`; both native answers belong to the same `agentId`. The child's metadata
continues to name the original `toolUseId`, and resumed SDK assistant output can still
carry that original `parent_tool_use_id`. Neither field identifies the current task
episode. Every normalized claim and correlation produced from a forwarded frame retains
both that frame's evidence and a sufficient host-to-child certificate. Independent
certificates remain alternative immutable proofs; deleting one cannot withdraw another.
Late certificates refine indexed normalized templates in bounded passes, including after
restart, without advancing the source's stream state again. A routing hint alone never
supplies a child's ownership. The current provider schema has no `Agent.resume` parameter; that spelling
in an older vendor contract is not a substitute for the observed `SendMessage` route.

The two observed `task_started` records reuse `task_id` but have different
`tool_use_id` and record UUIDs. Each start establishes a distinct task activity resource;
the task handle remains a routing key. A later notification naming `tool_use_id` targets
that activity, not whichever same-handle activity arrived most recently. An update
without invocation identity needs a coherent source-local active episode; missing or
ambiguous starts cannot fabricate a creation or overwrite another episode's state.

Returned child identities acquire delegation meaning through their actual retained
invocation. An unrelated tool returning an `agentId` key cannot create a directory agent.
A failed `SendMessage` result creates no resume or accepted delivery merely because the
SDK itself completed normally. A successful resume identifies the existing child and
its interaction, not another spawn. Task outcome, invocation outcome and child execution
remain separate unless a retained run association proves the corresponding link.
Task progress/notification usage belongs to the identified activity resource. Reported
token totals, tool counts and duration milliseconds retain separate units; they do not
establish missing execution endpoints or disjointness from child/root accounting.
Completed activity history lives in the archive, not an ever-growing source checkpoint.

The recorder waits for the first child answer to reach the native transcript before
requesting resume: a synchronous hand-back can precede that file's flush, and a premature
resume can legitimately report that its transcript was not found. This is a capture
precondition, not an adapter sleep or a guarantee about every vendor run.

### Workflow bundles

The retained Claude 2.1.238 workflow bundle in
[`fixtures/unified-trace/claude/workflow/manifest.json`](../fixtures/unified-trace/claude/workflow/manifest.json)
records an asynchronous launch, a completed manifest, invocation journal and two native
child transcripts. The capture inventory records the selected original record numbers,
source digests and path substitutions; omitted predecessor records remain missing.
The paired Claude 2.1.245 failed probe retains a failed host manifest that knows one
child, plus a journal with two child starts and no results. A missing child ID in that
manifest cannot erase the second independently identified child or close either invocation.
The [capture inventory](../fixtures/unified-trace/claude/workflow/capture.json) records both
source versions and extraction boundaries.

The launch result's `runId` identifies the workflow resource, while the enclosing
`tool_result.tool_use_id` identifies its creator operation. `taskId` remains a task
handle, not a child ID. A manifest uses its retained companion-transcript association
and matching launch `runId`; neither the filename nor the capture's routing subject
establishes its owner. Its `status` governs the workflow resource alone. The manifest's
full result and progress remain runtime representations of the invocation, independent
of the earlier caller-facing launch result.

Only `workflowProgress` entries with explicit `agentId` identify children. Phase rows,
queued entries without an ID and `agentCount` do not create agents. A progress entry
can establish a child interaction and an activity resource scoped to its run, index and
attempt, with its own reported state and usage. Its index and attempt are not
assumed equal to the invocation journal's `key`. Reported workflow totals and child
counters remain distinct scopes.

The manifest's `startTime` and each activity's `startedAt` are reported wall-clock
milliseconds. `durationMs` is a reported duration; it does not manufacture a missing
terminal timestamp. Journal records without timestamps gain no elapsed-time claim.

The launch result's `transcriptDir` locates its journal. A retained source definition
and that exact captured locator bind journal records to the workflow. Each journal
`key` identifies a delegation operation; `started` and `result` describe that
invocation, with `agentId` naming its target. A result preserves its exact value and
finishes that invocation. It does not manufacture a child Activation boundary or a
delivery of the parent's summary. Missing journals and unmatched locators remain gaps,
and a failed workflow cannot finish an invocation whose result was not observed.

### Schedules

The controlled [schedule capture](../fixtures/unified-trace/claude/schedule/capture.json)
records `CronCreate`, `CronList` and `CronDelete` against the real CLI using a loopback
provider. Creation returns `id`, `humanSchedule`, `recurring` and `durable`; a following
list contains the job. Successful deletion returns that ID, the next list is empty,
and deleting an absent ID returns an error. The fixture job is non-durable and is
deleted during capture; no firing is requested or inferred.

These result shapes gain schedule meaning only through their retained built-in
invocation. A successful create declares a Schedule resource and registration state;
its creating operation retains the exact requested cron and prompt. A failed tool
result never registers or cancels a resource. Deletion must agree with the requested
ID and a known schedule reference. A list observes registration, not a new creation
operation; an empty list does not prove cancellation, firing or expiration.

Native IDs are resolved within their evidenced context. A durable registration can
be anchored by its creator, but an unrelated project-wide listing does not identify
that creator or establish a cross-session resource join. Such listings remain retained
with unresolved scope until a certificate supplies it. Resource host is absent when
the record does not establish who runs the scheduler.

The recorded `scheduled_task_fire` native notices have no schedule ID. Their content
remains a system notice with an explicit association gap. Prompt text, `/loop` wording,
recency and a preceding registration cannot identify which resource fired, establish
its delivery or start an Activation.

## Lifecycle and result authority

| Observation | State transition |
| --- | --- |
| Actual root execution event with matching live loop scope | Establish known activation start evidence; precise earlier execution start can remain unavailable |
| Accepted input without execution evidence | Delivery acceptance only; no Activation is opened |
| `content_block_stop` | Seal the part or arguments only |
| Assistant message `stop_reason: tool_use` | That model message yielded tool calls; the activation continues |
| SDK `result` with subtype, `is_error`, `terminal_reason`, and loop scope | Close the corresponding root Activation with its own recorded result and termination reason |
| `task_started` | Start the identified task execution/resource, with its recorded type and host association |
| `task_updated.patch.status` or `task_notification.status` | Lifecycle evidence about the identified task; classify its target before applying it |
| Source EOF, missing child file, or parent activation completion | Observation/source fact only; no vendor child outcome is invented |
| Process exit or server cancellation without vendor terminal confirmation | Supervisor-authored execution termination evidence, retaining ambiguity about vendor application |

An activation's final answer is attached only where the source gives that scope. A
native assistant `stop_reason` describes a model message. It does not by itself prove
Claude's complete outer loop settled: hooks and autonomous continuation may run again.
Native histories without explicit outer boundaries retain messages with unknown
activation association. A convenient human-message grouping is a read projection.

Terminal claims cite their target and the applicable field-specific resolution rule.
A known failure is not replaced
by a later generic completion notification about a different lifecycle. Genuine
corrections remain retained as corrections. Descendant error totals do not overwrite
an ancestor's successful own result.

## Context and payload representations

Native `parentUuid` relationships establish recorded history paths. They do not by
themselves select one path as the current conversation of a running process. The
examined SDK contract has no general history-head field, and the retained fixture
inventory contains no universal current-head selection witness. Import exposes known
candidate heads with unknown selection when no additional evidence supplies it.

A supported source-specific selection witness records the actual selected entry/head
or included membership, its source position, and the rule establishing its meaning.
Live execution correlation may establish a particular activation's branch membership
when it identifies the consumed predecessor; receiving another stdout message alone
does not. The latest file row, capture timestamp, `promptId`, apparent single leaf, or
`last-prompt` bookkeeping does not become a universal selected-head rule. A reader may
choose a candidate branch as view state without changing the captured selection facts.

Compaction maps to a ContextCheckpoint, with `logicalParentUuid`, preserved segment
references, preserved message IDs, and summary references when present. A transcript-only
summary is not evidence of the exact replacement prompt sent to a model. Disconnected
or missing predecessor references remain explicit coverage gaps. Neither compaction
nor source rotation removes earlier archived work.

Preserved compaction membership is scoped to that checkpoint. It does not establish
every later selected history head or inference input. A selection without the required
ancestor records remains partial membership rather than an invented complete branch.

Fork provenance names the source session and any proven copied-entry membership. The
observed `forkedFrom.messageUuid == uuid` shape cannot supply a universal fork point.
The native `forkedFrom.sessionId` stamp establishes context lineage independently of
a checkpoint or cutoff; repeated stamps corroborate the same origin without selecting
one from contradictory source sessions.
The child-side copy is a context reference or copied representation, with the original
authorship preserved. A missing cutoff is recorded as missing.

Missing invocation evidence does not erase a result's explicit `is_error` outcome or
its structured runtime bytes. A returned child identity whose meaning depends on a
built-in invocation stays unresolved and requests that invocation from retained
evidence; later arrival must resume the join. The call reference alone never supplies
invocation arguments or promotes an unrelated tool's `agentId` field into a child.

Operation results retain the native tool-result block, structured `toolUseResult`,
persisted sidecar body, and forwarded live representation independently. A sidecar may
contain more output than the formatted tool result; neither overwrites the other.
The SDK's `SDKUserMessage` and `SDKUserMessageReplay` carry structured data in
`tool_use_result`; native envelopes use `toolUseResult`. Mapping and sidecar discovery
select the field for the actual transport. A missing field is not an explicit null
result, and the other transport's spelling is not a fallback. Structured envelope
metadata binds to a tool result only when the content array supplies exactly one
tool-result pairing. Ambiguous metadata remains retained with a diagnostic instead of
being copied onto every operation in that envelope.
Outbound SDK frames remain exact write evidence. Their UUIDs, content and optional tool
metadata do not establish vendor admission, a returned result or an independently
observed child; server control facts own the known dispatch stages.

The controlled Claude 2.1.278 [read capture](../fixtures/unified-trace/claude/tool-read/capture.json)
and [sidecar capture](../fixtures/unified-trace/claude/tool-sidecar/capture.json) use the real
CLI with a deterministic loopback provider, isolated configuration and one enabled tool.
They retain both transport spellings with matching record UUIDs. The read's caller
representation has line numbers while its structured data preserves the plain file
bytes. The Bash capture retains a bounded caller preview and the complete produced
sidecar as separate representations. Provider requests, response text and usage are
controlled fixtures; these captures do not claim remote-model behavior.

Images, documents, thinking signatures, unknown block types, truncated output markers,
and vendor fields remain intact in retained evidence. Unknown content is available as
an opaque payload part, not a successful empty string.

Reported usage is scoped to its actual message, SDK result, task activity, or cumulative
series. Repeated usage on records sharing `message.id` requires the vendor's documented
aggregation rule before summation. Root `modelUsage` and child task usage are not assumed
disjoint. `duration_ms`, `duration_api_ms`, tool elapsed seconds, and arrival timestamps
keep distinct units and clock provenance.

SDK `usage` is local to the completed query. The controlled two-query capture reports
input usage 10 then 20, while `modelUsage` reports 10 then 30 and `total_cost_usd`
also accumulates. Model usage and total cost are therefore agent-scoped cumulative
samples in the observed SDK source, not separate costs charged to each Activation.
Each model sample retains its reported name, optional canonical name and provider.
Model context/output limits are gauges, and reported cost keeps its pricing basis.
These samples supply no unobserved zero baseline or disjointness certificate.

## Cross-source reconciliation and coverage

Every equivalence cites a retained join: identical native occurrence identity and
provenance, shared invocation ID in its real namespace, an SDK UUID preservation
contract, sidecar host attribution, or an explicit workflow manifest link. Equal prose,
equal timestamps, block index zero, and similar titles are not join keys.

Where live final SDK messages expose the same persisted record UUID, their corresponding
native blocks can confirm the same content occurrence. Where only a model message ID
is shared, per-block correspondence requires a retained ordering/index mapping with
unambiguous cardinality; `message.id` alone cannot collapse records. A live-only fragment
remains its own observed representation until such evidence arrives. Native absence
cannot erase retained live evidence.

| Dimension | Managed live capture | Native-only import |
| --- | --- | --- |
| Root identity and retained native entries | Known when headers/records are captured | Known from native IDs |
| History predecessors and selected membership | Captured predecessor paths; selected membership needs a specific witness | Candidate paths; selection unknown without a supported witness |
| Stream parts and precise stream order | Observed where stdout exposes them | Unavailable; completed blocks only |
| Human submission identity | Known for server-dispatched requests | Proven only by source provenance, otherwise unknown |
| Activation lifecycle | SDK loop result plus live correlation | Partial unless native lifecycle evidence exists |
| Child identity/interior | Depends on explicit task/native joins and captured bundle | Depends on available delegate files and sidecars |
| Exact inference input | Usually unavailable | Usually unavailable |
| Process runtime output | Only captured result/sidecar representations | Only retained representations |

Coverage is per object, semantic dimension, and observed interval. The table states
possible evidence, not a blanket promise for every record in that mode.

## Replacement and falsifiable acceptance

Implementation removes synthetic native block start/stop replay, the global current
message identifier, host-tool promotion into agents, input-as-bare-turn-text,
name-based catalogue creation, native/live suppression by host-name rules, and
parent-turn sweeps that determine a child's terminal outcome. Existing capture cursors,
spool guarantees, filesystem containment, and process controls are retained behind the
new evidence interface. There is no migration, dual internal event vocabulary, or
compatibility alias for the removed model.

Acceptance includes these faults and observable promises:

| Deliberate fault | Required failing assertion |
| --- | --- |
| Key native parts by `message.id` and index alone | All four observed ordinary-message record parts survive as distinct addresses |
| Key delegated agents by invocation/host | Three root children and their three distinct nested children survive the observed fanout extract |
| Interpret `promptId` as human input identity | Meta, SDK, tool-result, and shared descendant occurrences cannot replace a dispatched human revision |
| Finish an Agent call at argument seal | The child remains unstarted or independently running until actual lifecycle evidence arrives |
| Treat `async_launched` as child completion | Launch result and child completion remain different occurrences and states |
| Process a notification before its type/host evidence | Later joining produces the same graph as ordered replay without an invented agent |
| Replace the task failure with generic parent completion | The child failure and parent's own result remain independently retrievable |
| Treat the compaction summary as human input | Checkpoint, transcript-only content, and original authorship remain distinct |
| Treat `forkedFrom.messageUuid` as a single cutoff | The fixture's varying self-UUID stamps cannot produce a fabricated shared prefix boundary |
| Reload after live/native overlap | Canonical identities and content remain stable; unsupported equivalences remain visible |

New tests demonstrate their intended failure by mutation before becoming a gate. The
structural extract is one source of expected topology and ownership; full sanitized
live/native payload pairs are additionally required to establish streaming/body
reconciliation, timing, and byte preservation. Synthetic scenarios are labeled as such
and cannot be cited as observations of a vendor guarantee.
