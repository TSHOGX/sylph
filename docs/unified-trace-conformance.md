# Unified Trace Conformance and Evidence

Status: **Normative acceptance contract. Retained corpus replay and production storage
tests cover specific promises; the complete semantic, failure and workload matrices remain open.**

[unified-trace-foundation.md](unified-trace-foundation.md) defines meanings;
[unified-trace-storage.md](unified-trace-storage.md) defines durability and reads.
This document defines how to disprove an incorrect implementation. Implementation
progress and remaining capture work are tracked in
[roadmap.md](roadmap.md#unified-trace-foundation-review), not in another report.

## Evidence inventory and boundaries

The evidence layers answer different questions. None substitutes for the next.

| Evidence | Available material | What it can establish |
| --- | --- | --- |
| Pinned source | Codex, DeepSeek Harness, Pi source revisions; Claude SDK declaration version/digest in the adapter specs | Field meaning, persistence order, exposure limits, lifecycle producer behavior |
| Observed structural corpus | Long-history inventory and extracted records below | Actual identities, repeated records, namespaces, branch boundaries, topology and field placement |
| Executable reference exercise | Disposable SQLite model and deliberate faults below | Transaction/order ideas fail when their defining safeguards are removed |
| Production replay | Retained bytes through admission, mapping, reduction and persisted reader projections | The particular identities, contents, lifecycles and schedules asserted by each production test |

Full sanitized live/native byte pairs and production mapper replay are a separate
implementation gate. Structural extracts omit content and cannot prove text, binary,
lexical, stream reconstruction, or complete vendor coverage. A reference exercise is
not proof of fsync behavior under power failure or correct Rust integration.

The retained material is:

- [Codex structural inventory](measurements/unified-trace/codex-structure.json): the
  three named long roots, their inspected direct-child sources, and an eleven-rollout
  lineage for one logical thread, with source digests and exact structural samples.
- [Claude structural extract](measurements/unified-trace/fixtures/claude-structure.json):
  selected native records/sidecars with pseudonymized identities, source positions and
  content shapes, including shared model-message IDs and nested delegation.
- [Pi branch extract](measurements/unified-trace/fixtures/pi-branch-structure.json):
  a real native history tree whose sibling user entries must remain alternative paths.
- [Vendor source inventory](measurements/unified-trace/fixtures/vendor-structure-inventory.json):
  artifact/source SHA-256 digests, retained fields, redaction policy and source baselines.
- Existing [native fixtures](../fixtures/native/) and
  [Codex app-server captures](measurements/codex/README.md): additional malformed-input,
  outcome, transport, control, and native payload evidence, each subject to its own
  documented capture limits.
- [Controlled live/native captures](../fixtures/unified-trace/manifest.json): Claude
  2.1.272 successful marker input/output and Pi 0.84.3 failed outer-loop settlement,
  with source/retained digests and explicit path substitution/JSON reserialization.
  These preserve semantic bodies and identity relationships; they do not claim the
  sanitized bytes reproduce original lexical encoding or establish unobserved Pi joins.
- The Pi 0.85.1 success, retry and compaction pairs in that same manifest use the real CLI
  with a deterministic loopback provider. `scripts/capture-pi-trace.mjs` preserves independent stdout, RPC
  direction records and native output. Its repeated equal text deltas and settled outer
  run establish Pi's emitted shapes; the synthetic provider response establishes neither
  a remote model's behavior nor a missing live/native identity join.
  The retry capture includes two failed inference responses followed by recovery;
  manual compaction occurs after outer settlement. Production replay in
  `crates/server/src/trace/worker/tests/corpus/pi_maintenance.rs` checks independent
  operation outcomes, exact result payloads, native history and checkpoint identity,
  the open outer interval at retry completion, and same-cut provenance across cold,
  live-first, alternating, duplicate-delivery and restarted schedules. Synthetic
  malformed-result and missing-end cases separately test conservative outcomes and
  episode isolation; they are not vendor captures.
- The retained Codex 0.150.1 [delegation capture](measurements/codex/captures/s10_delegation_fork.jsonl)
  supplies all inbound app-server frames to the production replay in
  `crates/server/src/trace/worker/tests/delegation.rs`. A pinned digest guards the complete
  recorder artifact; `RawValue` extracts each whole frame without JSON reserialization,
  and its original recorder line is retained as the source coordinate. This establishes
  the observed child and command/process lifetimes, parent completion before child
  completion, distinct answer bodies, and activity notifications without invented agents.
  It supplies no child follow-up run, delayed sidecar, native/live join, proved spawn
  result or caller-truncation pair. Its independent native replay source is a separate
  session, explicitly not a counterpart to this live recording. Historical tool text
  is retained data and is never executed by the harness.

Sanitization preserves equality relationships, key scope, missing-versus-empty values,
ordering and branch structure. Replacing two distinct IDs with one placeholder would
destroy the very property under test. Full payload fixtures use controlled substitute
content with provenance that labels substitution; they are never presented as original
private bytes. Historical prompts and tool bodies are data and are never executed.

## Reproducible checks

The vendor audit works entirely from retained sanitized artifacts by default:

```sh
python3 docs/measurements/unified-trace/audit-vendor.py
python3 docs/measurements/unified-trace/storage-spike.py
```

Original-source verification is an explicit read-only check, requiring the local files
named in the inventories. It must fail or report absence if evidence moved; it cannot
quietly regenerate the expected values from changed input:

```sh
python3 docs/measurements/unified-trace/audit-vendor.py --sources
python3 docs/measurements/unified-trace/audit-codex.py --check docs/measurements/unified-trace/codex-structure.json
```

The Codex audit reports digests and structure, never prompts or executable tool text.
It distinguishes physical file line/byte location from recorded vendor ordinals.
Changing an expected count deliberately makes its check fail. The vendor audit checks
topology and part identity after verifying artifact digests; a deliberately collapsed
Claude UUID with an updated artifact digest still fails the semantic assertion.

[storage-spike.py](measurements/unified-trace/storage-spike.py) creates temporary WAL
databases and checks independent equal occurrences, producer redelivery, control outbox
retries, immutable snapshot bodies, late reorder during pagination, private generation
publication, old-generation readability and post-publication arrivals. Its
[recorded result](measurements/unified-trace/storage-spike-results.json) includes these
negative controls:

```sh
python3 docs/measurements/unified-trace/storage-spike.py --fault content_dedup
python3 docs/measurements/unified-trace/storage-spike.py --fault export_redispatch
python3 docs/measurements/unified-trace/storage-spike.py --fault mutable_page
python3 docs/measurements/unified-trace/storage-spike.py --fault early_publish
python3 docs/measurements/unified-trace/storage-spike.py --fault skip_tail
```

Each fault must exit nonzero at its named promise; the unmodified run must pass. Those
commands intentionally fail and are not ordinary successful CI checks. The experiment
uses connection close/reopen at explicit committed boundaries, not a claim that it
injects real power loss or asynchronous writer races.

## Normal forms the corpus must assert

Expected results are authored independently from the mapper. A test may not compute its
expected ID, scope or outcome by calling the production helper it is meant to verify.
The following compact examples use invented identifiers and content to illustrate
expected normalized shapes. They are not additional vendor captures.

### Reusable child, independent execution, and delayed delivery

Given an invocation `spawn-1` by agent A, explicit returned native child B, child
activation B1, parent completion A1, a later follow-up delivery D2, and child activation
B2, the graph contains two agents, three activations, and two task deliveries if both
initial and follow-up admission are observed. It does not contain an agent named
`spawn-1` or one agent for each completed activity notification.

```text
Agent(A), Agent(B), Spawn(A -> B, proof=spawn-1-result)
Activation(A1, owner=A, outcome=succeeded)
Activation(B1, owner=B, outcome=failed)
Activation(B2, owner=B, outcome=succeeded)
Operation(spawn-1, owner=A, activation=A1, category=delegation)
Delivery(D1, message_revision=task-v1, recipient=B, admitted=true)
Delivery(D2, message_revision=followup-v1, recipient=B, admitted=true)
TriggeredBy(B1,D1) and TriggeredBy(B2,D2) only when boundary correlation proves them
```

Repeated generic notifications do not add agents or overwrite B1's failure. A message
from B to A has a new Delivery with recipient A; its content's production owner remains B.
Parent completion does not close B2. If an observed terminal process lives beyond B1,
its Resource state is independent of B1 as well.

### Submitted bytes, native wrapping, and shared model message IDs

A human submits revision H1. The server dispatches H1, and the vendor later records a
wrapper W1 plus injected context C1 under the same correlation group. H1 remains byte
identical; W1 can be related as a transformed representation when proved; C1 retains
harness authorship. Shared prompt correlation does not make three occurrences one input.

Four Claude record UUIDs can share the same vendor model-message ID. Each record/block
part has a distinct address and retains its source ownership. The model grouping does
not authorize replacing one record with the next. Two independently submitted identical
human strings likewise remain separate messages/deliveries even if every byte matches.

### Branched history and inherited source records

For entries `p -> a -> a2` and `p -> b -> b2`, selecting b2 includes p, b and b2;
it does not include a or a2 merely because they appeared earlier in the file. All five
entries remain in the retained archive. With no selection observation, the source's
current head is unknown; a reader can choose an inspection path without claiming it
is the vendor's active context.

A copied Codex prefix can contain a second `session_meta` record belonging to the
parent. The governing child header remains the child's identity; copied metadata cannot
reset it. A replacement rollout can begin at vendor ordinal 214 even though its header
is physical line one. A history-base exclusive ordinal and its byte offset refer to
their stated physical source and coordinate domain, not to line 214 of any arbitrary
file. Telemetry inside that prefix stays evidence, not child-local execution.

### Result representations and streaming replacement

An operation records full runtime bytes R, returns truncated bytes C to its caller,
and later admits bytes M into a conversation. Each representation remains retrievable.
Admission of M proves no particular next inference included it. If an inference I
explicitly contains M, that is a separate inclusion fact naming I.

Stream revisions `append("A")`, `append("A")`, `replace("B")`, and `final("BC")`
retain two identical append occurrences and the replacement. Reconstructing every
revision as a prefix of `BC` is invalid. A late ordinary append is archived without
reopening the final content; an explicit corrected final creates a new revision.

### Accounting and terminal evidence

A cumulative counter with samples 100, 100, 130 yields a delta of 30 only when the
100 baseline and common counter/reset domain are known. A following sample 5 starts a
new or uncertain reset domain, not a negative delta silently clamped to zero. Exact
response usage is counted once by its response scope. Inclusive root totals and child
totals are not added. Reported monotonic duration and endpoint wall elapsed time retain
their different units/bases even when numerically inconsistent.

A source `task_complete` with structured error ends an execution unsuccessfully. A
termination without result evidence gets an unknown outcome. Neither missing content
nor a generic completed item supplies a successful activation.

## Replay comparison protocol

For a pinned bundle, run each vendor's same mapper and the shared reducer under:

1. Cold source replay into an empty generation.
2. Incremental input at deliberately varied batch boundaries and chunk sizes.
3. Redelivery of every producer batch, native reread, and overlapping follower/reread.
4. Independent source interleavings that preserve each source's own order.
5. Late sidecars, delayed identity joins, delayed outcomes, and resolved source gaps.
6. A new projection generation from exactly the same evidence cut.

Compare canonical object references/equivalence resolution, owner, immutable content
revisions, delivery stages, entry membership/order basis, outcomes, resources, metrics,
coverage and provenance. Exclude scheduling-dependent publication revision numbers and
receipt timestamps from semantic equivalence where the experiments intentionally used
different admission histories. Compare them exactly when testing receipt playback itself.

A lower-resolution source need only establish its known subset. Expected missing live
deltas, exact model inputs, native outer boundaries, or unresolved Pi live/native joins
are asserted as missing. It is not acceptable to erase the richer source to force equal
output, or to invent values for the poorer source. Unknown does not mean empty or false.

## Required semantic mutations

| Fault introduced | Observable broken promise |
| --- | --- |
| Content-hash deduplication | Distinct repeated messages/deltas disappear |
| Native namespace stripped from keys | Child/root or runtime/model identities collide |
| Global block index | Concurrent message parts overwrite each other |
| Header `session_id` treated as child owner | Codex child work moves to root |
| Copied header treated as governing header | Child identity/config changes inside inherited context |
| Activity/task/tool ID used as native child ID | Phantom directory agents appear |
| Accepted delivery opens an activation | Claude non-querying input or Pi queueing fabricates execution |
| Queue success treated as consumption | Unconsumed/rewritten input appears included |
| Native wrapper overwrites submitted revision | Human request bytes change after dispatch |
| Arguments sealed closes operation/child | Delegation appears complete before execution evidence |
| Final tool return closes a resource | Background command/cell disappears while still running |
| Parent outcome sweeps descendants | Independent child/process lifetime is truncated |
| Generic completion overwrites detailed failure | Own terminal result becomes incorrectly successful |
| Inner Pi agent end closes outer activation | Retry/compaction continuation becomes extra or missing execution |
| File EOF or missing source declares success | Observation loss masquerades as completion |
| JavaScript text establishes nested execution | Unexecuted branch/code creates invented runtime work |
| Current file/order chooses native history | Revert/branch selects discarded work |
| Ordinal interpreted as line index | Nonzero-baseline Codex history prefix is truncated or duplicated |
| Inherited context gains local ownership | Child authored count/usage duplicates parent work |
| Compaction summary replaces the archive | Earlier evidence and retained operation results disappear |
| Runtime bytes replace caller bytes | Model/caller-visible truncation is lost |
| Snapshot appended as delta | Stream body duplicates prefixes or misses replacement |
| Cumulative/root totals summed as intervals | Usage and cost double count |
| Receipt clock fills native endpoint | Plausible but unmeasured duration appears |
| Coverage collapsed to one strength rank | Missing intervals disappear behind a complete message surface |

Mutations target a specific mechanism and test. At least one assertion checks the actual
value that would be wrong, not merely "parsing succeeded", a row count, or a status
string shared by the implementation. When a timing window matters, a fixture deliberately
holds that window open before completing the competing operation.

## Storage, query, and failure acceptance

| Area | Required production experiment |
| --- | --- |
| Admission | Crash/kill before and after payload sync, evidence commit and acknowledgment; compare accepted prefixes and exact bytes |
| Operational outbox | Crash around dispatch, result receipt, export and export marking; assert unknown at-most-once outcome is not retried |
| Source acquisition | Partial JSONL, superseded staged tails, CRLF/duplicate JSON keys, truncate/rewrite, inode reuse, same-ID changed payload, concurrent reread/follower, large record across chunks |
| Sidecars | Missing then available; mutable locator revisions; interrupted chunk publication; corrupt digest; vendor removal after complete retention |
| Reducer | Late parents, cycles, conflict withdrawal, equivalence merge then split, absent native owner, unknown required enum, excessive semantic expansion |
| Rebuild | Fail each validation/publication step; race admitted arrivals and an in-flight old writer; cut never regresses and no half-generation is served |
| Snapshot views | Read page one, move/merge/delete a member, read page two and old body; no omissions/duplicates or body revision changes |
| Reusable lease | Read header, history, child and operation details through one base view token; each uses the same cut/revision with its own cursor scope |
| Payload access | At R1 a readable anchor X references P1; X/R1 cannot read newer P2; an authorized copied entry can read its retained inherited bytes without origin-environment access |
| Action target | Focusing an embedded child without a control route never sends Stop or input to its anchor/root Session |
| Search | Lag index behind trace and temporal GC; pin matching view/cut, or invalidate explicitly; never current text with old membership |
| Retention | Declaring a complete native prefix cannot remove a unique live delta, clock or raw field |
| Explicit deletion | Cancel old reads/jobs; reject known old receipts AND never-admitted pre-deletion spool/outbox items by their fixed ingestion epoch; re-adoption cannot revive old snapshots or count erased bytes as recaptured; terminal deletion receipts release unrelated producer work without claiming admission |
| CAS collection | Shared payload across evidence, active/private/leased generations, search and export jobs survives until its last live root is released |
| Resource pressure | Projection stall does not hold evidence/control writer; bounded queues report actual admission/capture limits and recover from durable checkpoints |

### Performance fixtures

Performance fixtures vary independent dimensions: history length, concurrent sources,
child fan-out, activation count, payload bytes, source rewrite frequency, and the number
of superseded versions of hot objects. A LIMIT on returned rows is not a scan bound.
Report SQL VM steps, rows examined, lock occupancy, batch bytes, worker queue age,
first-page bytes, memory, active leases and evidence/projection/search cuts.

For fixed visible page size, tenfold history growth and tenfold invalid-version density
must not cause an unbounded foreground scan. If plain temporal tables fail, use a proven
membership/index design or resumable work-budget response before accepting the read
contract. A budget-exhausted scan cannot return a complete empty result. Rebuild and full
export are measured separately because they legitimately process the archive.

## Acceptance order

The foundation's object and persistence contracts are the basis for the next work.
Acceptance proceeds in dependency order:

1. Core types, evidence/source identity, immutable payloads, durable receipts and control
   outbox satisfy fault tests. No UI interpretation is needed to pass them.
2. Each pure vendor mapper supplies sanitized native/live pairs with independent expected
   identities, content and gaps. Native-only support does not claim exact live joins.
3. Shared reduction passes cold/incremental/mixed-source equivalence, history/lifecycle
   mutations, correction and coverage tests over the corpus.
4. Generation publication, temporal pages, search, authorization, action routing and
   workload bounds pass the production storage experiments.
5. Adapter runtime integration and frontend implementation proceed against that contract;
   the [UI acceptance scenarios](unified-trace-ui.md#performance-and-acceptance) validate
   the actual reader, accessibility and performance without reinterpreting vendor JSON.

The old runtime/UI tests remain useful regression observations but cannot veto the new
data model merely because the old expectations encode removed semantics. Replace those
expectations deliberately and prove the replacement tests fail for the new promise.
Run final affected suites after the last edit and retain actual output, including any
remaining failure; no implementation gate closes on a spec or reference-model pass alone.
