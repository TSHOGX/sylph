# Unified Trace Conformance and Evidence

Status: **Normative acceptance contract.**

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
- The [Pi Bash sidecar corpus](../fixtures/unified-trace/pi/tool-sidecar/capture.json)
  uses the same loopback recorder with one enabled tool. Two deliberately repeated
  model tool-call IDs produce separate observed invocations and output files. The
  complete 139,264-byte file contents are equal and share one retained body artifact;
  the two file locators, native entry IDs and source observations remain distinct.
  `crates/server/src/trace/worker/tests/corpus/pi_tools.rs` feeds acquisition's actual
  path selectors through production admission and normalization, including progress,
  message, echo and entry-window references, early attachments, restart and rebuild.
  Separate synthetic tests reject incomplete echo certificates and preserve bounded
  runtime checkpoints. No native/live message equivalence is inferred from equal bytes.
- The [Claude workflow bundles](../fixtures/unified-trace/claude/workflow/capture.json)
  retain a 2.1.238 successful two-child demo and a 2.1.245 failed host with two journal
  starts and no returns. Each bundle keeps launch records, manifest and journal with
  original record numbers and digests; the successful bundle also keeps native child
  input/answer records. Production replay in
  `crates/server/src/trace/worker/tests/corpus/claude_workflow.rs` verifies exact results,
  native answer ownership, independent host/activity/invocation outcomes, reported clocks
  and scoped usage, late source/launch joins, restart, duplicate delivery and same-cut
  rebuilding. These historical artifacts do not establish absent child Activation
  boundaries or equality between progress indices and journal keys.
- Controlled Claude 2.1.278 [read](../fixtures/unified-trace/claude/tool-read/capture.json)
  and [Bash sidecar](../fixtures/unified-trace/claude/tool-sidecar/capture.json) bundles
  retain real SDK/native frames and tool-produced bytes from a loopback provider.
  The provider's tool calls, final marker and usage are synthetic; the CLI supplies
  record identities, field spellings, caller formatting and sidecar persistence.
  `crates/server/src/trace/worker/tests/corpus/claude_tools.rs` verifies live runtime
  metadata before native arrival, separate caller/runtime bytes, early sidecars,
  source interleaving, restart and rebuild. An explicitly synthetic second call
  referencing the same physical snapshot proves result identity remains qualified
  by its invocation. Acquisition tests separately verify transport-specific selectors,
  immutable parent bytes and retained file revisions after removal.
- The controlled [Claude delegation bundle](../fixtures/unified-trace/claude/delegation/capture.json)
  retains root SDK/native records, the child transcript and its companion metadata.
  `scripts/capture-claude-trace.mjs delegation` uses an isolated loopback provider to
  observe a synchronous Agent hand-back and a successful SendMessage resume. The same
  child/task handle has two task-start UUIDs and invocation IDs, while forwarding and
  metadata can keep the original host ID. Production replay in
  `crates/server/src/trace/worker/tests/corpus/claude_delegation.rs` checks independent
  activity states, invocation outcomes, usage units, both exact child answers and one
  spawn across early metadata, source order, reread, restart and same-cut rebuild.
  The same replay also checks the forwarded prompt's unknown author, coordinator
  context, parent notification, exact bodies and separately recorded admissions. Every
  SDK-forwarded claim cites a sufficient retained child-routing certificate. Protocol-derived
  variations in `crates/server/src/trace/worker/tests/claude_forwarding.rs` retire
  independently retained certificates and restart a committed bounded continuation.
  `crates/trace/src/mapping/claude/tests/forwarding.rs` additionally checks bounded
  refinement and transitive correlation certificates; template admission cannot bypass
  the configured semantic-expansion limit. These variations do not claim another vendor
  recording. `crates/server/src/trace/worker/tests/claude_forwarding/streams.rs`
  exercises protocol-derived interleaved SDK start/delta/final frames with late identity,
  a certificate returned later on the same SDK source, restart, reread and cold rebuilding.
  A long blocked lane retains one pending head while parent history advances; direct
  continuation checks measure skipped-envelope item and byte credit against configured
  bounds. Malformed routing fences the physical source across restart without creating
  another root activation or stopping an independent source; its valid earlier prefixes
  remain replayable when their route resolves. Sessionless control/keep-alive envelopes
  from the pinned stdout union preserve content assembly without creating execution.
  A nested late-route case commits and reopens ready grandchild work, then finishes at
  the same evidence cut without requiring another capture. A stale ready flag returns
  to a blocked wait instead of spinning at that cut.
  Synthetic failures separately test unrelated agentId fields, failed resume results,
  overlapping starts, old notifications, transient missing invocation evidence and
  bounded completed-task checkpoints. No unobserved child SDK loop is fabricated.
- The controlled [Claude schedule cycle](../fixtures/unified-trace/claude/schedule/capture.json)
  retains creation, nonempty listing, deletion, empty listing and missing-ID deletion
  failure from one real CLI process. The local provider supplies the tool requests;
  the CLI supplies IDs, storage results and error flags. The fixture is non-durable,
  scheduled far in the future and removed during capture. Production replay in
  `crates/server/src/trace/worker/tests/corpus/claude_schedule.rs` verifies independent
  registration/cancellation facts, exact requested cron/prompt, failed-call outcome,
  delayed invocation classification, source ordering, restart and rebuild. It does not
  claim a firing or a cross-session identity certificate for project-wide schedules.
- The [Codex accounting excerpt](../fixtures/unified-trace/codex/accounting/capture.json)
  retains complete lexical accounting rows and an explicitly extracted structural header.
  Its response, turn, response-event thread and token-count thread series have independent
  expected values. The [Claude two-query capture](../fixtures/unified-trace/claude/accounting/capture.json),
  produced by `scripts/capture-claude-trace.mjs usage`, isolates query-local usage from
  process-cumulative model usage and cost with synthetic provider usage and real CLI
  framing and persistence. Together with the Pi compaction bundle, production replay in
  `crates/server/src/trace/worker/tests/corpus/accounting.rs` checks typed model scope,
  checkpoint usage, receipt deduplication, source ordering, incremental restart and
  same-cut rebuild without assuming disjoint totals or an unknown zero baseline.
- The [Codex goal snapshot](../fixtures/unified-trace/codex/goal/capture.json) retains one
  actual `thread_goals` row and its schema from a read-only SQLite transaction. It is a
  logical row capture, not a database-page copy; its producing binary version is unknown.
- The controlled Codex 0.155.1 [goal-context capture](../fixtures/unified-trace/codex/goal-context/capture.json)
  runs a bounded loopback provider, holds the second goal request until interruption,
  and submits the exact retained goal text as ordinary input after pausing the goal.
  The native `goal.internal_context` and `user.text` annotations differ despite equal
  bodies. Private provider metadata witnesses the goal trigger, while captured public
  and native turn-start records omit it. Provider requests and logical header maps are
  research witnesses, not production mapper input. The production replay in
  `crates/server/src/trace/worker/tests/corpus/codex_context.rs` verifies purposes,
  authorship, exact bytes, independent turn outcomes, goal state/counters, missing causal
  joins, source order, restart, redelivery and same-cut rebuild. Provider output and usage
  are synthetic; native persistence and the app-server lifecycle are observed.
  `crates/envagent/src/runtime/database.rs` verifies goal acquisition against that schema
  with committed WAL data and an added opaque column. Production replay in
  `crates/server/src/trace/worker/tests/corpus/codex_goal.rs` keeps exact captured detail,
  resource identity and reader-visible counter units through reread, interleaving,
  restart and rebuild. Replacement, missing-row and idless public-event scenarios are
  explicitly synthetic variations, not additional vendor recordings.
- The [Codex input bundle](../fixtures/unified-trace/codex/input/capture.json) retains
  Codex 0.155.1 native records and both directions of a real app-server stream, with
  their observed interleaving. `scripts/capture-codex-input.py` uses an isolated loopback
  Responses provider. Identical submitted text has distinct client and vendor message
  IDs. Production replay in `crates/server/src/trace/worker/tests/corpus/codex_input.rs`
  combines these bytes with explicitly synthetic control acceptance/dispatch facts to
  check native-only, live-only and mixed sources, late control evidence, duplicate
  receipts, rereads, restart and rebuilding. Synthetic transformations preserve the
  submitted revision while retaining the vendor wrapper; an omitted dispatch cannot
  establish admission. Separate tests require both source epochs for previews and retain
  selected-path restrictions when exposing related submitted content. Adapter tests
  reopen the same scripted transport to prove send, steer and queue IDs survive actual
  request-counter reuse. No acceptance or model inclusion is inferred from dispatch.
- The [Codex reusable-child excerpt](../fixtures/unified-trace/codex/reuse/capture.json)
  retains a root and one child from the native structural inventory, using
  `scripts/capture-codex-reuse.py`. Two child activations, equal-text task deliveries,
  an intervening non-triggering message, child replies, activity notices and parent
  calls retain their actual IDs, clocks and original source coordinates. Prose is
  replaced by deterministic equality-preserving markers and headers are structural
  extracts; this is neither a complete history nor a live/native pair. Production
  replay in `crates/server/src/trace/worker/tests/corpus/codex_reuse.rs` checks stable
  identities and immutable late routing refinements under independent source order,
  duplicate receipts, rereads, restarts and same-cut rebuild. Synthetic two-peer
  routing additionally tests independent author/recipient certificates.
  `crates/server/src/trace/worker/tests/route_proofs.rs` uses minimal protocol-derived
  headers to verify corroborating certificates across source order, restart, retained
  proof deletion and bounded late-join continuations. These are implementation regressions,
  not additional controlled vendor captures. The production reader also proves that new
  mailbox admissions are not inherited history. `crates/server/src/trace/worker/tests/copied_communication.rs`
  wraps complete retained communication payloads in explicit protocol-level copied-prefix
  fixtures; it verifies missing/late/deleted origins, exact text and opaque parts, preserved
  lineage and the absence of replayed child deliveries or execution. Unknown selected
  heads and request inclusion remain unknown.
- The retained Codex 0.150.1 [delegation capture](measurements/codex/captures/s10_delegation_fork.jsonl)
  supplies all inbound app-server frames to the production replay in
  `crates/server/src/trace/worker/tests/delegation.rs`. A pinned digest guards the complete
  recorder artifact; `RawValue` extracts each whole frame without JSON reserialization,
  and its original recorder line is retained as the source coordinate. This establishes
  the observed child and command/process lifetimes, parent completion before child
  completion, distinct answer bodies, activity notifications without invented agents,
  and the recorded fork origin independently of spawn ownership or a checkpoint.
  It supplies no child follow-up run, delayed sidecar, native/live join, proved spawn
  result or caller-truncation pair. Its independent native replay source is a separate
  session, explicitly not a counterpart to this live recording. Historical tool text
  is retained data and is never executed by the harness.
- The retained Claude fork records in
  `fixtures/native/claude/fork-replay-messageuuid-mirrors-own-uuid.jsonl` pass through
  production admission, mapping and reading in
  `crates/server/src/trace/worker/tests/corpus/fork_lineage.rs`. Each original-session
  stamp survives cold replay, restart, overlapping reread and same-cut rebuilding.
  The origin shortcut remains separate from parentage and descendant membership.
  This fixture does not establish a universal copied-prefix cutoff or a context
  checkpoint, and it does not contain the original session's transcript.

The historical Pi fork headers from `fixtures/native/pi/fork-parent.jsonl` and
`fixtures/native/pi/fork-child.jsonl` are replayed with explicit source-inventory
certificates in `crates/server/src/trace/worker/tests/pi_origin.rs`. The inventory is
harness-authored acquisition metadata, while the header bytes are retained fixtures.
Every ordering, worker restart and same-cut rebuild preserves the origin identity;
independent-proof deletion and contradictory bindings preserve the appropriate current
and older views. This source join does not certify which copied entries belong to the
fork or where child-local production begins. The complete retained parent/child records
also run through `crates/server/src/trace/worker/tests/corpus/pi_admission.rs`: parent
inputs retain their admissions, copied child text stays byte-exact without creating
child deliveries, and independent child retention survives original-scope deletion and
rebuild. The same replay now supplies native inventory and checks `ContextCopy` matches at
exact original/copied source positions. The reader projection independently rejects a
relation whose endpoint position was not retained for that entry, so a reused native ID
cannot borrow a different version. Early/late inventory and parent/child ordering
produce identical claims, while deleting the origin removes only the optional source
links. A separate held-continuation test verifies persisted matching indexes and same-cut
restart recovery beyond one work budget. These establish positive native record copies, and the reader now
presents the original owner for one unambiguous exact-version `ContextCopy` chain while
keeping copied bytes independently readable. Multiple/cyclic witnesses remain unresolved;
unknown terminal owners remain absent. The reader regression uses declared messages and
revisions and tests an intermediate entry observed at two different source positions.
The HTTP copy-link regression separately retires relation evidence while leaving both
endpoint headers readable, checks every nested edge, and preserves an independent
alternative witness until its own deletion. These checks use held snapshots and retain
payload access independently of the optional lineage label. A held page-assembly window
also tests proof deletion and grant revocation after cached authorization has run.
Selected-version, terminal-owner, nested-edge and page-revalidation mutations fail their
own assertions. Deep-copy fixtures measure immediate proof storage at 100 and 1,000
entries and require constant reader-label bytes rather than embedded ancestry paths.
They do not establish the local-production status of unmatched entries or an ultimate actor
for nested copies. A partial-history replay deliberately delays the parent's final
assistant record while retaining the child's copied predecessor and successor. The
successor stays unresolved until its own original arrives; it never becomes a local
suffix merely because its predecessor has a copy witness. Both source orders, restart,
same-cut rebuilding and the held earlier view preserve this distinction. The reader and
production regressions fail when the predecessor-based local classification is restored.

Pi entry snapshots have an additional production continuation certificate in
`crates/server/src/trace/worker/tests/pi_snapshots.rs`. The real admission path receives
paired RPC writes and replies, persists the bounded mapper checkpoint, extends a baseline
through empty append windows and sibling navigation, then reopens, rereads and rebuilds
the generation. Request evidence is part of the selected snapshot claim. Faults for
missing or recycled IDs, wrong cursors, source gaps, generation changes, rejected or
overlapping replies, and session switches leave the response's own content readable but
do not borrow the old baseline. The reducer keeps inactive branches through private
snapshot proof rows rather than copying every prior claim into each poll.

Pi RPC entry/tree snapshots are also checked independently of native ownership in
`crates/trace/src/mapping/pi/tests.rs`: assistant messages, operations, shell records and
checkpoints make no producer claim from the reader identity. Native declarations refine
that absence in either order while fork handles stay unresolved. Reducer regressions in
`crates/trace/src/reducer/tests/owners.rs` preserve conflicts across known actors and
native handles; absence cannot bridge incompatible claims. HTTP acceptance in
`crates/server/src/trace/worker/tests/provenance/unknown_owners.rs` reads retained
operation input bytes through the Work row's operation target without inventing an owner
or execution state. `crates/trace/src/reading/tests.rs` preserves history anchors and
keeps unknown-owned invocations independent of local activity; synthetic desktop/mobile
browser coverage in `frontend/e2e/trace-unknown-owner.spec.ts` opens retained inputs while
keeping ownership uncertainty explicit and preserving the draft. These focused fixtures
establish the absence/refinement and inspection rules, not a new native/live identity join.

Sanitization preserves equality relationships, key scope, missing-versus-empty values,
ordering and branch structure. Replacing two distinct IDs with one placeholder would
destroy the very property under test. Full payload fixtures use controlled substitute
content with provenance that labels substitution; they are never presented as original
private bytes. Historical prompts and tool bodies are data and are never executed.

Result representation selection is checked independently of the first occurrence page
in `crates/server/src/trace/worker/tests/provenance/result_representations.rs`. The
normalized-fact fixture covers a late caller result, representation-bound cursors,
missing bytes versus absent results, empty bytes, optional contributor retention,
post-probe authorization changes and bounded availability continuations. Browser cases
in `frontend/e2e/trace-result-representations.spec.ts` cover active-only result pages,
comparison, page replacement, explicit choices and continued availability checks. These
fixtures establish query and interaction contracts; they do not establish vendor
payload formats or replace the retained vendor result corpus.

Payload-use format checks in `crates/trace/src/mapping/pi/tests.rs` preserve identical
shell text and JSON argument bytes as different uses. Codex mapper tests assert the
native argument and pinned runtime-writer format contracts without re-encoding lexical
bytes. `crates/trace/src/reducer/tests/payload_formats.rs` covers absent-field refinement
and known-format conflicts. `crates/server/src/trace/worker/tests/provenance/payload_formats.rs`
restores the committed worker and reads equal-byte uses through HTTP while an unrelated,
ungranted derivation claims HTML. The transport stays binary and each authorized use
retains its format. Synthetic browser coverage in `frontend/e2e/trace-payload-formats.spec.ts`
checks text, JSON and unclassified rendering at desktop and mobile widths; the fixture
is not evidence for unobserved vendor formats.

Structured exploration uses the pinned dependency in `Cargo.lock`; its skip-name and
skip-value paths were inspected before adopting it. `crates/server/src/trace/structure/parser.rs`
covers duplicate members, exact large-number spelling, UTF-8/escape validity, malformed
suffixes, limits and large scalars/names. Index tests compare late-page VM work at tenfold
sibling growth and reject node/depth/disk/deadline overruns. HTTP tests in
`crates/server/src/trace/worker/tests/provenance/structure.rs` cover a 20 MiB scalar,
independent retained views, Subject isolation, job capacity, cancellation, expiration
and deletion while a page is prepared. `frontend/e2e/trace-structure.spec.ts` covers
explicit admission, bounded navigation, duplicate labels and cleanup of late admission.
The added parser runs only on the server; the browser component loads on explicit demand.

Origin assertion acceptance in `crates/server/src/trace/worker/tests/provenance/lineage.rs`
keeps unresolved/missing targets visible, authorizes each proof independently, and pages
within the configured limits. It verifies that deleting one of two assertions naming the
same endpoints leaves the other readable, that exact claim inspection remains available,
and that target revocation neither leaks metadata nor expires the recipient's view.
`frontend/e2e/trace-origins.spec.ts` covers lazy catalogue/proof reads, failed-page retry,
independent origin navigation, bounded Back state and preserved drafts at both widths.

Full environment export acceptance in
`crates/server/src/trace/worker/tests/provenance/export.rs` verifies exact unknown vendor
bytes, repeated admissions, fixed cuts, sparse attachment ranges, independent snapshot
ownership, revocation/deletion and post-read authorization, output deadlines, crash-pin
retirement, atomic ticket/pin publication and query work under unrelated archive growth. Client tests verify the complete
framing digest before file commit, including equal-length corruption and clean EOF without
a footer. `frontend/e2e/trace-evidence-export.spec.ts` checks explicit environment scope,
verified desktop/mobile downloads and transactional streaming-file failure. The exporter
reads retained evidence, not current vendor files or reconstructed transcript values.

Paced playback acceptance in
`crates/server/src/trace/worker/tests/provenance/export/playback.rs` preserves receipt
order, exact large-integer clock metadata and intermediate bytes at a fixed cut, including repeated equal revisions,
backward clocks and clamped gaps. It checks independent playback capacity and retained
views, policy bounds and revocation. `frontend/lib/trace-playback.test.ts` checks
unspent waits across pause, bounded UTF-8/binary previews, sparse prefixes and expiry.
`frontend/e2e/trace-playback.spec.ts` verifies desktop/mobile controls, bounded mounted
rows, provisional-content clearing, focus restoration, stop and access loss. The common evidence-stream validator rejects unverified completion for both paths and
keeps file-commit cancellation inside its lifecycle.

Entry-provenance presentation is verified independently of optional origin navigation.
`crates/trace/src/reading/tests/entry_context.rs` covers messages, operations, checkpoints
and opaque content, unresolved membership, foreign authors without inheritance, and
work grouping across provenance changes. The origin-visibility regression in
`crates/server/src/trace/worker/tests/provenance.rs` deletes/re-adopts or revokes the origin
while preserving the inherited marker, copied bytes and recipient view. Browser coverage
in `frontend/e2e/trace-entry-context.spec.ts` checks unavailable and unresolved states,
retained-view origin inspection and recipient drafts at both widths. Existing retained
Codex copy/communication and reusable-child replays now assert entry context separately
from authorship; the Pi fork corpus preserves unresolved provenance across source orders,
restart and origin deletion. These checks do not certify a child-local suffix or an
ultimate original actor.

Search ownership and scope are verified independently of protocol role and containing
identity. `crates/server/src/trace/service/search.rs` checks unknown, foreign and inherited
metadata, missing production and agreed ownership under purpose conflicts. The normalized
HTTP fixture in `crates/server/src/trace/worker/tests/provenance/search_scope.rs` checks
retained versus authored/selected membership, older retained selections, missing/unresolved
membership rejection, invalid combinations, cursor scope binding and explicit archive
inspection at the result view. Browser cases in
`frontend/e2e/trace-search-scope.spec.ts` cover scope transitions, request cancellation,
archive navigation and read-only composition at desktop/mobile widths. Explicit selection
inspection remains read-only even when its frozen header says current; unknown native
membership in the default live reader does not override authorized control.

Search membership cost is checked through the production indexer by
`crates/server/src/trace/worker/tests/provenance/search_membership.rs`. Retained selections
span multiple configured batches while sharing one body that fits a document; adding a
selection must preserve physical document IDs, JSON and FTS blocks and perform no payload
range reads. The indexing gate stays held across fixture publication and measured passes.
`crates/server/src/trace/search/tests/membership.rs` separately checks interrupted membership
replacement and restart, immutable result jobs, count/byte admission and query-owned
selection metadata in the pre-hydration result budget.

Exact search-body access is verified by
`crates/server/src/trace/worker/tests/provenance/search_result.rs`: a superseded composed
JSON body remains readable and transferable while its surrounding conversation shows a
later revision. Wrong subjects, views, revisions and representations are rejected;
prepared structured exploration outlives search invalidation but cannot outlive evidence
access. The retained-contributor deletion test covers first access through a fresh lease.
`crates/server/src/trace/search/tests/result.rs` covers item positions, expiry, invalidation,
framing budgets and oversized corrupt result jobs. The reader-store context test checks
old/current versions, retirement, delivery exclusion and measured lookup work under growing
superseded density. `frontend/e2e/trace-search-body.spec.ts` checks later byte windows,
desktop/mobile navigation and unchanged-query refresh after expiry.

Inherited-only context status is checked in `crates/trace/src/reading/tests/entry_context.rs`
against empty, unknown, incomplete and local-work alternatives, including conflicted
entries omitted by readable history, unresolved predecessors and undeclared content entries. The append regression in
`crates/trace/src/reading/append/tests/inheritance.rs` requires reconciliation on the first
local tail and permits subsequent ordinary appends. The normalized HTTP case in
`crates/server/src/trace/worker/tests/provenance/inherited_summary.rs` preserves older views
and unavailable origin content. `crates/server/src/trace/worker/tests/frozen_history.rs`
checks native frozen-prefix mapping, local work outside its selection, restart and rebuild.
`frontend/e2e/trace-inherited-summary.spec.ts` keeps inherited content readable and live
controls independent of this status at both desktop and mobile widths. These cases do not
certify unresolved Pi copy ownership or an unobserved local suffix.

Claude orphan-result semantics are covered in
`crates/trace/src/mapping/claude/tests/tool_results.rs`: explicit failures survive
missing invocations, and an unresolved delegation result registers its retained-object
dependency without inventing a child. The production late-invocation test in
`crates/server/src/trace/worker/tests/semantic_acceptance.rs` requires a later call on
another source to recover the child with both pieces of evidence, including on a fresh
replay. The invocation guard fails these regressions; sidecar/runtime retention and
unrelated-tool rejection remain independently tested.

Codex orphan-result admission is covered by
`crates/trace/src/mapping/codex/tests.rs`: function and custom outputs keep the same
result identity and admitted delivery with or without a preceding invocation, while
never declaring invocation arguments, execution outcomes or inference inclusion from
the output. `crates/server/src/trace/worker/tests/semantic_acceptance.rs` exercises
retained admission, readable orphan content, a late invocation on another source and
fresh generation replay. Both regressions fail against the invocation-dependent
admission guard. This establishes the admission distinction, not complete vendor
conformance.

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
