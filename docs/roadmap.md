# Roadmap

This roadmap orders user-visible product milestones and owns project status. Finished
implementation belongs in git history; architecture decisions belong in `docs/adr/`. Earlier
proposals or discussion threads do not outrank the repository specifications. The topics that
shaped the early plan (native correlation, dynamic source reading, raw and blob retention,
interior state, event-time axes, and Codex parity) are scheduled below from the milestone that
needs them.

## Current baseline

The server, normalized trace reader, protocol crate, three acquisition adapters, and
independent control/evidence/projection/search stores exist, and **every execution path
runs through an envagent** — framing, credentials, path policy, WAL spooling, reconnect
epochs, and adapter hosting, with no in-process seam
beside it. A single-machine install is therefore two processes.

Each store's schema is one collapsed migration rather than a chain
(`docs/database-schema.md` § Schema evolution): there is no other consumer, and
projections are re-derivable from retained evidence. Discarding development stores and re-adopting is
the ordinary answer to schema churn; a vendor reread cannot recover unique live bytes.
No real store is erased as part of the implementation checkpoint.

## What remains

The boundaries below are specified in `docs/` and not yet true in code. They are the difference
between reading a spec and reading the source. Each is a milestone below rather than a
separate list, since a boundary restated in two places drifts in one of them.

**A boundary leaves this table when it closes**, and what it recorded moves to its
milestone's own section. The table tracks the gap between spec and source, so a row reading
`complete` is a row asserting the opposite of what the table is for — and a reader scanning
for what is left has to read every row to find out which ones still count.

This project is spec-driven: `docs/` and the ADRs are the source of truth for behavior and are
written and settled before the code that satisfies them. Spec ahead of code is the normal state
of an active milestone, not a defect to flag inline. This table is where that implementation
gap is tracked. The roadmap records sequence and status; it is not a requirements backlog, and
external proposals or discussion threads are not project instructions.

| Boundary | Owner | State |
| --- | --- | --- |
| Production semantic acceptance | Foundation implementation below | Core storage/failure acceptance and retained three-vendor replay are established; the complete vendor semantic and replay certificates remain S2 work |
| Projection and delivery scale | Milestone 4 and foundation implementation below | Reader pages are bounded and every S1.4 workload dimension and required report is measured under mixed spool/socket/ACK load; whole-reduction reader materialization for general reconciliation remains, and its superlinear re-materialization term is measured rather than removed |
| Vendor trace semantics | Foundation implementation below | Runtime cutover is implemented; remaining vendor certificates, workflow and schedule mappings follow core stabilization |
| Reader acceptance | Conversation and work panel delivery below | Normalized reader is implemented; full visual/resource acceptance and VoiceOver remain open; Safari testing was excluded by the user |

## Unified trace foundation review

The design review resolves D1–D7 in
[unified-trace-foundation.md](unified-trace-foundation.md). It is the normative target,
with [storage and read protocol](unified-trace-storage.md), separate
[Codex](unified-trace-codex.md), [Claude](unified-trace-claude.md) and
[Pi](unified-trace-pi.md) mappings, and the dependent [UIUX contract](unified-trace-ui.md).
The affected architecture, protocol, storage and reader documents now route to those
owners. This supersedes the former block/span foundation rather than layering repairs
or compatibility branches over it.

**Established design evidence:** pinned Codex/DeepSeek Harness/Pi source and Claude SDK
contracts; reproducible structural extracts from the real long histories, native
Claude records/sidecars and Pi branch data; and a disposable SQLite exercise covering
occurrence identity, outbox retry, temporal pagination and generation publication.
Intentional wrong-identity/branch/ordinal expectations and five storage faults were
observed failing. These checks establish specific design claims, not a passing
production reducer. [Conformance](unified-trace-conformance.md) names their limits.

### Foundation delivery gates

The following finite gates govern this delivery. Evidence is a named production test,
retained-corpus replay, or measured connected workflow; source inspection establishes a
gap, not acceptance. Stage 1 is accepted; Stage 2 owns the remaining vendor semantic
and replay certificates. Full reconciliation is allowed where the storage contract permits it;
universal incrementality is not a separate requirement.

| Gate | Classification | Exit evidence / dependency |
| --- | --- | --- |
| S1.1 Deterministic authorization provenance | Closed | Retained support is now selected by the least declaration pair rather than first arrival. `crates/server/src/trace/worker/tests/epoch_order.rs` is enforced, no longer ignored: reversed independently scoped arrival yields identical retained claims, resolved graph and reader epoch provenance. `worker/tests/provenance.rs` and `worker/provenance_support_tests.rs` cover supporting-observation epochs, agreeing redeclarations and origin deletion against recipient-owned copies. |
| S1.2 Large valid correction | Closed | An aggregate batch-limit overflow now routes to bounded private reconciliation instead of suspending the source. `worker/tests/reconciliation.rs` proves a 150-message selection producing 1053 changed reader rows against a 1024-row batch limit publishes every retained message in order with `suspended_sources == 0`, while cancellation, a failed final commit and fencing each keep the prior cut readable and refuse the stale writer. Non-aggregate limits still record a source gap by design. |
| S1.3 Connected delivery | Closed | `tests/connected_delivery.rs` drives the production TCP relay, spool, admission, projection and reader with only vendor acquisition synthetic. A held post-commit ACK opens the backlog window deterministically; the suite proves exact retained lexical bytes and monotonic admission order per producer sequence, responsive file commands during backlog, live reply delivery across a forced disconnect, one vendor dispatch per input, deletion discarding payload while retaining the accepted watermark, frozen snapshots excluding later rows, search reaching the last retained message, bounded history pages preserving source order, identical membership after `rebuild()`, and a drained spool. Restart replay of a lost receipt is covered separately. |
| S1.4 Supported production workload | Closed | All seven `docs/unified-trace-conformance.md` § Performance fixtures dimensions now have a production measurement. History length and superseded density are bounded in `reading_store/tests.rs`; the cumulative reducer-claim rate projects the largest retained history inside its limit in `worker/tests/reconciliation.rs`; `worker/tests/workload.rs` measures concurrent sources (flat per record), child fan-out and activation count (bounded at the observed corpus ceiling, superlinear term reported past it), payload bytes (1000x body growth moves neither rows nor VM steps, and page bytes by at most one preview per visible row) and source rewrite frequency (constant per-incarnation step, row identity unchanged). Queue delay and the three cuts are reported by the mixed spool/socket/ACK run in `tests/connected_delivery.rs`, which measures a 166-item queue peak draining after a forced reconnect and asserts that neither the projection nor the search cut leads the evidence cut. Durable mapper-state restoration is paged independently of total retained size. The workload suite reports serialized state, per-record growth and concurrent-source overhead directly from each run; this measures retained state rather than attributing shared-process RSS to one worker. Active leases remain bounded by held views, and contention tests measure writer-lock occupancy at growing histories. Every assertion here was observed failing against a deliberate mutation. |
| S1.5 Core failure and replay matrix | Closed | A clause-level audit of `docs/unified-trace-conformance.md` § Storage, query, and failure acceptance and `docs/unified-trace-conformance.md` § Required semantic mutations leaves the following remaining cases and records the evidence closing each gap. **Correctness/security, now covered:** embedded Stop reaching the anchor session; snapshot paging across a member merge and a member delete; one base view token serving header, history, activation and object details, asserting both one lease and one snapshot. A stalled projection is proven not to hold the evidence writer: with the generation writer held in a competing transaction, admission still commits and advances the evidence cut while the projection is demonstrably still blocked. A search index left behind by collection refuses its query rather than serving current text under retired membership; both the `start_query` checkpoint comparison and the in-transaction `search_state` cut comparison are load-bearing, since removing either alone still refuses while removing both returns hits from retired membership. Export jobs as a CAS collection root are covered after all, by `gc/tests.rs` `raw_and_manifest_addresses_share_physical_bytes_until_the_last_typed_root_releases`, which holds both roots as `PinOwnerKind::Export` and collects only after the last releases. **Correctness/security: closed.** **Crash windows:** `trace/exporter/crash_tests.rs` kills isolated processes before and after result commit, evidence export admission and export-mark commit. Reopening both stores preserves exact submitted and receipt bytes, fixed observation identity, atomic pending counters and one dispatch attempt; a lost result becomes unknown, and replay admits each fact once without granting another at-most-once dispatch. Allowing unknown at-most-once retries was observed failing the new regression. `spool/capture/tests.rs` now kills before acknowledgment handling and before/after its commit, proving receipt dispositions, exact queued bytes, queue release and producer watermark recover together. Suppressing the watermark update was observed failing when the unrelated tail could no longer be acknowledged. Combined with the evidence admission kill tests and connected lost-receipt restart, these cover the acknowledgment crash boundaries. **Vendor source shapes:** deterministic recycled-file-identity injection now proves the complete-prefix check preserves both byte versions across restart. `tests/connected_delivery/retention.rs` exposed and now covers a production sidecar stall: attachment observations omitted transfer declarations for their already captured body. Same-epoch source-occurrence proofs now authorize retained sidecar manifests/chunks, with transactional revalidation, indexed per-observation primary payloads for alternate manifest layouts, and independent cross-scope, corrupt-proof, restart and deletion tests in `trace/upload/tests/attachments.rs`. The connected test removes the vendor file after the real spool/socket/ACK path completes and retains exact lexical bytes plus runtime results through rebuild. **Semantic failures, retention and coverage now covered:** `trace/worker/tests/semantic_acceptance.rs` retains an unknown required vendor kind as exact bytes with a sourced diagnostic, refuses to certify that history interval as completely mapped, and proves receipt clocks cannot supply absent native execution endpoints. `trace/service/tests/retention.rs` interprets a complete native prefix, runs actual collection with an unrooted sentinel, rebuilds and restarts, and then checks every live occurrence, lexical body and capture clock. `trace/worker/tests/provenance/coverage.rs` verifies the normalized-fact boundary preserves every coverage dimension and both complete and missing intervals through reduction, publication and HTTP reads; vendor-authored coverage certificates remain S2 work. Every new regression was observed failing under its targeted semantic mutation. **Rebuild failure sweep:** `trace/service/tests/publication_failures.rs` enumerates initialization, replay/cut checks, baseline/semantic/tail validation, fencing, sealing, readiness and pointer publication against the real mapper and reader. It preserves old leased content across restart, refuses partial publication, resumes the old writer before pointer commit, respects the committed pointer afterwards, and releases failed build capacity. This exposed and fixed initialization errors leaving jobs reserved before the caller received their ticket. **Superseded staging now retained:** staged data uses bounded chunks and durable closed-generation jobs. The drain preserves each complete record and terminal fragment, original positions, capture clocks and authorization epochs; source identity proofs also name their retention epoch. Metadata credit allows a full spool to repackage data without exceeding its configured ceiling. `spool/capture/tests/tails.rs` covers independent record/byte limits, concurrent drainers, fixed job frontiers, restart and SIGKILL at the transfer transaction. `tests/connected_delivery/retention.rs` verifies terminal bytes are admitted before native reread completion and survive rebuild. The connected case also exposed immutable claim collisions between independent captures of one physical position; claims now distinguish canonical retained observations while domain references remain source/native scoped. `trace/worker/tests/semantic_acceptance.rs` proves one readable message/revision remains after one capturing scope is deleted. The corresponding source definitions accept independently retained proofs of the same native identity position without conflating those proofs with their locator/owner binding or growing recursive inventory proof chains. Production replay preserves one domain message/revision across independent scopes and after origin deletion. Full affected suites and targeted falsification cover this core boundary; vendor-specific completeness certificates and reader acceptance remain the following gates. |
| S2.1 Vendor semantics and evidence | Required acceptance gap; depends on S1 | Audit each adapter spec clause; complete independently authored native/live expectations and source certificates. Retained Claude forwarded-message provenance and certificate retirement are covered. Late-bound Claude SDK stream assembly is covered; remaining checks include the complete vendor clause audit and reusable-child/resource/result matrix. Unobserved joins and selected heads remain explicitly unknown. |
| S2.2 Adapter conformance | Required acceptance gap; depends on S2.1 | All vendor replay schedules and applicable semantic mutations pass through production admission, mapping, reduction and reads; acquisition preserves exact external vendor records. |
| S3.1 Reader functionality | Required acceptance gap; depends on S2 | Full descendant navigation, resource state histories, structured values, observed-content transfer and separately budgeted export/playback satisfy `docs/unified-trace-ui.md`; action targets remain exact. Bounded resource observations, immutable revision payload pages and explicit observed-content copy/download are implemented. Full spawn-descendant directory navigation and progressive exact filtered counts are implemented. Structured exploration now has explicit bounded indexing and paged child navigation. Full environment evidence export and receipt-order observation playback are implemented, with resource/representation acceptance following the complete matrix. Recorded Codex/Claude origins and Pi retained source-path joins now supply authorized header navigation. A paged origin catalogue exposes unresolved and multiple assertions with independently authorized links and exact proof inspection. Pi native copy matches now retain exact source-entry/version relations with independent authorization. Historical snapshot production ownership remains unknown until compatible evidence refines it. Entry provenance remains visible across all row kinds and unavailable origin links. The inherited-only aggregate is implemented for positively proved entry provenance. Child-local suffix classification and copy-derived ultimate attribution remain open. Historical snapshots and unresolved native fork entries do not create child admissions; the unproducible correlation query is removed. |
| S3.2 User-facing acceptance | Required acceptance gap; depends on S3.1 | Complete specified desktop/mobile visual, keyboard, accessibility and resource matrix, actual VoiceOver pass, long reading/live/reconnect/search/rebuild workflows. Safari testing remains excluded by the user. |

Every stage closes with the full affected Rust/frontend suites after the final edit,
a requirement-by-requirement audit and a detailed English Conventional Commit. Coherent
verified blocker fixes may be committed before the stage boundary. No stage closes solely
on unit tests, synthetic browser trees or the reference SQLite exercise.

**Deferred non-blocking improvements:** universal incremental graph maintenance, producer
sharding, byte-weighted scheduling and independently compressed body ranges. Revisit only
if a named primary-workflow measurement fails its required experience. These do not remove
any normative acceptance requirement from this delivery.

The milestones below remain the record of the current implementation and other product
work. Their block-era mechanisms are not requirements for the replacement. No store is
erased and no production trace implementation is claimed complete by this design review.

### Foundation implementation

The reader reports `Only inherited context observed` when every retained history entry
has known foreign provenance and complete content, with no owned activation, operation,
local message production or incoming delivery. Unknown/missing provenance cannot establish
this state, including conflicted declarations and undeclared content entries omitted from
the readable archive. Missing predecessors and unassigned execution also prevent it;
local work outside a selected prefix is still considered. Context status remains
separate from execution outcomes and live controls. The header's evidence includes the
contributing entries, while unavailable origin metadata does not own the child's content.
The first local append to an entirely inherited history requires header reconciliation;
after that transition ordinary independent appends keep their bounded path. Coverage spans
pure classification, the incremental boundary, normalized HTTP snapshots, native Codex
frozen-prefix replay/restart/rebuild and desktop/mobile live control. Positive `ContextCopy`
proofs now label copied entries with the original containing agent through one unambiguous
nested chain; conflicting or cyclic witnesses remain unresolved and origin metadata never
owns copied bytes. Pi copy-derived ultimate actor attribution for unmatched nested copies
and general child-local suffix proof remain open alongside the vendor and reader acceptance
gates.

Search hits now open their exact retained body revision through an explicit result
job authority, including composed stream prefixes superseded by a different final body.
Range reads, transfers and structured exploration share complete-tuple authorization;
prepared derivative jobs own their lease and keep checking retained evidence and grants.
Matched content starts at the indexed byte window. Surrounding-history navigation is
separate and resolves collapsed stream entries through the actual reading projection;
unavailable context does not hide independently retained matched bytes. Repeating an
unchanged search obtains a fresh result job. Result lookups bound retained job JSON and
include framing in the materialized-result byte budget. Coverage includes exact JSON,
wrong subjects/views/revisions, invalidation, derivative lifetimes, historical context
lookup and desktop/mobile navigation; independent faults were observed failing.
Remaining vendor, inheritance and reader acceptance gates stay open.

Search scope membership now has a separate bounded publication path from immutable
indexed body occurrences. Retaining another named selection reuses the body's document
IDs and FTS storage without rereading payloads. Both body and membership completion gate
publication; interrupted membership replacement restarts privately and old result jobs
keep their original scope, snippets and view. Query-owned selection metadata participates
in the result byte budget before indexed metadata is hydrated. Search schemas change in
place, with the per-scope FTS rows removed. Production cost coverage spans multiple
configured membership batches, inspects the actual FTS rows/blocks and holds the indexing
gate across measurement so a background pass cannot make its work counters vacuous.
The remaining inheritance/vendor/reader gates remain open.

Search no longer treats a missing or unresolved original owner as locally authored
work. Authored scope requires a matching known agent author and production owner with
no inherited/unresolved entry assertion; shared common-field resolution preserves agreed
ownership across unrelated metadata conflicts. Retained search remains available for
human, incoming, unknown and copied content. The unused guessed original-owner field is
removed from disposable search documents, with no schema compatibility path.
Agent search exposes explicit retained, selected-context and authored scopes, with query
cursors and result views reset on a scope change. Ordinary search uses retained history.
All retained named selections are indexed, including older branches; missing or unresolved
selection membership is rejected before creating a search job and on resumption.
Archive hits carry an explicit archive location and open that view at their search
checkpoint; archive and explicit named-history readers offer return-to-current instead
of a composer, including a selection that was current at the frozen checkpoint. Missing
native selection evidence does not disable an authorized default live conversation.
Matched-body inspection is distinct from the conversation presentation, which may show a
later revision at the same search checkpoint.
Scope, selection, archive, composer and cancellation regressions are falsified by
independent faults. The full run also exposed normalized HTTP fixtures staging unrooted
bytes beside the real collector; fixture captures now use the production upload tickets.
Forced collection between staging and admission reproduces the old loss and validates
the rooted path. The remaining inheritance and vendor/reader acceptance gates remain open.

Claude SDK checkpoints now follow explicit session/host processing partitions inside
the original physical source. A child whose identity certificate arrives late retains
one blocked head and a bounded evidence-backed tail; parent frames can still supply that
certificate later in the same stream. The regression previously lost the child answer
entirely. Restarted replay now preserves the exact start/delta/final sequence and own
outcome, and repeated capture does not append the deltas again. Skipped sibling envelopes
consume both item and byte replay credit. Invalid routing fields and malformed envelopes
fence later frames of the physical source rather than reusing uncertain per-lane state;
valid pre-gap prefixes can still resolve, and independent sources remain available. Routing keys are private bounded processing coordinates, never
agent identities or rewritten source positions. The pinned SDK transport-control envelopes
remain sessionless and never interrupt content processing. Routing discovered while
replaying a child tail commits a retry flag for its waiting grandchild, so restart and
idle catch-up need no additional capture. The disposable checkpoint schema and
Claude mapper build change in place. Broader vendor certificates, inheritance semantics
and reader acceptance remain open.

Claude forwarded-frame claims now retain their host-to-child routing certificate.
The retained delegation replay exposed claims whose ownership cited only the forwarded
SDK frame; its explicit host tag alone did not prove native child identity. A bounded
index now stores normalized templates and emits immutable alternatives for independent
routing certificates, including certificates arriving after the frame. Correlations
produced by forwarded frames retain the same route dependency. Continuations serialize
and resume after commit/restart without advancing stream state again, while deferred
templates remain subject to the semantic-expansion bound.
The recording's forwarded prompt remains unknown-authored, resumed coordinator input
remains context, and the parent task notification remains runtime-authored. Exact bytes,
recipient scope and admission survive source interleavings, reread, restart and rebuild;
no exact model inclusion is fabricated. Removing one independent certificate preserves
the remaining supported content; removing both leaves the raw frame without invented
child attribution. The broader vendor-clause audit, inheritance semantics and reader acceptance remain open.

Entry context is now a property of every reader row, independent of message authorship
and optional origin navigation. Known inherited entries keep their label when origin
metadata is absent, deleted, re-adopted or revoked; unresolved original-owner assertions
remain visibly unresolved without claiming a copied prefix or local production. Work,
checkpoints and opaque entries share the same contract, and grouping cannot erase a
change in provenance. Origin inspection retains the current view and preserves the
recipient's draft and control target. This removes the message-only original-owner
field in place and updates the disposable projection schema. Ultimate copy-owner
attribution and child-local suffix classification remain open. The inherited-only
aggregate is implemented for positively proved entry provenance above.

Receipt-order observation playback now consumes an explicit environment-scoped stream at
one retained cut, preserving repeated observations and intermediate exact payload bytes.
The server reports original receipt deltas, scaled waits, long-gap clamps and clock
discontinuities separately from capture/source clocks and agent execution. Exact metadata
text preserves large clock ticks and producer sequences in browser previews. The shared
validator keeps cancellation attached through a pending download commit. Playback has
independent capacity and source/output/work/lifetime bounds while sharing the archive's
retained leases, transactional pins, authorization checks and framing digest.
The reader offers explicit start, speed, pause/resume and stop. Pauses retain the unspent
wait without extending lifetime; bounded raw prefixes and a bounded visible observation
window keep the stream out of ordinary reader caches. Completion verifies every frame;
corruption or access loss clears provisional previews. Pacing, pause, lifetime, binary
labeling, sparse prefixes, digest, clock-overflow, policy-bound and capacity faults were
observed failing their targeted regressions before restoration. Browser faults separately
expose ignored pause/stop, accumulated rows and stale protected previews. Remaining vendor and inherited-context
acceptance stays open.

Full environment evidence export now streams a checksummed JSON Lines archive at one
retained evidence cut. Raw sources can contain several agents, so the UI names the whole
environment scope explicitly. Every retained admission, including rereads of one source
occurrence, keeps its receipt identity, source/capture clocks and exact payload bytes.
Attachment manifests retain sparse ranges without leaking unreferenced bytes from shared
physical chunks. A completion footer carries frame/byte totals and a SHA-256 digest;
clean truncation and equal-length corruption cannot commit the browser's output file.
The browser uses a transactional file writer when available and a separate bounded
buffer otherwise, with progress and cancellation outside the reader cache.
Environment/admission indexing bounds scans independently of unrelated archives.
Export jobs own snapshots and temporary payload pins, recheck grants and environment
visibility after output backpressure, and retire on disconnect, deadline, completion or
startup recovery. Ticket validation and pin publication are atomic, so expiry cannot
leave a new root without its recovery record. Deleted evidence and unadmitted staging are excluded explicitly;
deletions during export invalidate completion. The evidence schema hard-breaks in place
with its new admission index, export registry and environment visibility counter.
Targeted mutations reproduce widened environment/cut reads, ignored visibility/grants,
borrowed reader views, leaked pins, missing post-read authorization, unindexed scans,
stalled deadline capacity, bypassed output bounds, delayed preparation/cleanup and
unverified archive completion. Retiring a ticket before pin publication was also
observed creating an orphan pin before the transactional fix.
Remaining vendor conformance and inherited-context acceptance stay open.


Context origins now have a bounded assertion catalogue available from main and companion
readers. Each row retains its claim identity and its own proof epochs; repeated endpoint
pairs do not combine independent witnesses. Missing or unresolved targets keep explicit
unavailable rows while current grants and retained target epochs govern navigation.
Proof inspection opens the exact claim; target reads do not lend their retention to the
recipient. The catalogue pages independently of history selection, retries the same
cursor, and preserves its bounded page state across origin navigation and Back. Existing
unique-origin header shortcuts remain conservative; origin assertions do not establish
spawn ancestry, copied membership or ultimate authorship. HTTP and desktop/mobile
regressions cover independent evidence, unavailable targets, paging and draft preservation.
Targeted faults reproduce missing unresolved assertions, combined or omitted proof epochs,
leaked target metadata, lost page state, missing exact proof selection and companion
scope substitution before restoring each mechanism.


Structured JSON exploration now builds a disposable index on explicit request, using
Struson's streaming validation and scalar/name skipping with path tracking disabled.
The parser preserves exact spans, duplicate object members and lexical number precision;
large values do not become in-memory JSON trees. A complete validated index pages by
parent/ordinal with a VM-step bound and bounded previews. Source bytes, nodes, depth,
disk use, lifetime and job capacity are independently bounded and reported. Jobs own
retained views, recheck authorization while scanning and before returning pages, and
release indices on cancellation or expiration; startup discards only the dedicated
scratch indices after acquiring the archive lock. Source evidence remains authoritative.
The lazy reader exposes progress, nested navigation, bounded Back history and exact
scalar byte windows without loading the whole payload or changing a control target.
Parser, HTTP, large-payload, indexed-work and desktop/mobile regressions cover the new
surface. Targeted mutations reproduce shifted spans, omitted trailing validation,
numeric conversion, ignored cancellation, broad child scans, lost disk limits, borrowed
views, missing final authorization, cross-Subject reads, oversized previews and stale
scratch indices. Browser mutations detect accumulated pages, leaked late jobs, stale
protected data, oversized scalar reads and omitted expiry cleanup. Remaining foundation
acceptance stays open.

Operation result inspection now uses an authorized catalogue of the three result
representations, with separate indexed occurrence and readable-payload collections.
Bounded presence checks retain private authenticated continuations and cannot borrow content-page
cursors, manufacture absence at a work limit, or bind optional result contributors to
the operation's own lease. Grant changes and retired positive witnesses are rechecked
before replying. Result occurrences no longer accept submitted-input representation;
operation summaries no longer embed a mixed result preview. The reader chooses an
available caller result first, preserves explicit choices, and mounts one page per
selected representation. Unavailable views remain explained; empty catalogues have no
empty chooser. Inputs are labeled as inputs, and result availability follows payload
evidence rather than the presence of a reference. Production HTTP and browser regressions cover these contracts; targeted mutations
reproduce the corresponding authorization, pagination and selection failures.

Operation inputs and result occurrences now retain a typed payload use with optional
media type and encoding, independent of its physical byte digest. Vendor decoding
helpers carry the format they actually produced; native Codex JSON argument strings
retain exact lexical bytes, custom inputs remain text, and the pinned raw writer
establishes JSON/UTF-8 for runtime bundles. Unclassified sidecars borrow format only
from their own immutable manifest. Reader encoding remains absent when unobserved,
and byte-range HTTP transport no longer selects an unrelated equal-digest derivation.
Pi text inputs/results and JSON tool arguments with identical bytes retain different
formats. Reducer refinement fills absent fields without overwriting known conflicts;
worker restoration, HTTP and browser checks pin these meanings through the read path.
Scoped format, manifest, conflict and HTTP-header mutations fail the new regressions.
Paced playback, full environment evidence export and structured exploration are implemented above.

Observed operations with unknown or unresolved producers now remain Work rows with
explicit unknown-owner labels and direct inspection targets. They retain their own
history-entry anchors and cannot borrow local activation membership or group merely
because both owners are absent. The operation input HTTP regression follows the target
returned in the actual reading row; desktop/mobile browser coverage opens retained input
without dispatching commands or losing the reader draft. Restoring generic unknown rows
and removing the owner label each fail their corresponding regression. This closes the
inspection path for unknown-owned work, not inherited-owner classification or attribution.

Pi historical entry/tree snapshots now retain unobserved production ownership instead
of assigning the containing reader as the actor. `OwnerRef::Unknown` distinguishes
absence from an unresolved native handle. Field-specific reducer rules refine absence
from compatible native declarations, in either arrival order, while preserving known
actor conflicts and distinct unresolved references. Assistant messages, tool calls,
shell records and checkpoints retain the distinction; unknown owners do not acquire
requesters, activations or producer labels. Operation inputs remain readable through
the retained namespace even when ownership is unknown or unresolved.
Mapper, reducer and HTTP regressions cover refinement and payload access; targeted
faults reproduce the incorrect actor labels, incompatible-owner collapse and broken
input reads before restoration. Native/snapshot refinement does not establish the
child-local suffix or the ultimate actor of nested copied history.

Pi native copy matching now joins the explicit fork locator, a native source-incarnation
certificate, native entry ID and canonical content witness. Only documented predecessor
and compaction kept-boundary rewrites are excluded; other content and unknown fields
remain significant. `ContextCopy` retains both entry references and exact source
positions without merging entries, changing their predecessors or replacing independently
retained child content. Source, witness and plan indexes persist in bounded rows, share
immutable roots and resume bounded joins after restart. Metadata continuations preserve
unrelated pending dependencies rather than losing tool-result or history work.
Retained Pi parent/child replay proves matches in either source order with early or late
inventory, unchanged body authorization through origin deletion, reread and same-cut
rebuild. HTTP inspection treats copy links as optional metadata. Relation responses now
name the target agent scope, so cross-agent inspection retains the view without reusing
a foreign history selection; Back preserves the preceding scope and draft.
Targeted identity, content, source-generation, continuation, authorization and navigation
faults were observed failing the regressions before restoration. Positive source matching
does not establish an unmatched child-local suffix or ultimate
actor attribution for nested copies. Those semantics and inherited-owner presentation
remain open; historical RPC snapshots now preserve absent ownership as described above.

Pi historical reads and unresolved native fork records no longer create new delivery
or admission claims from a user/tool-result protocol role. Direct runtime message events
and proven local native inputs retain their independent admissions. Snapshot revisions
remain readable without being treated as newly delivered bytes, and unresolved copied
tool records no longer name the child as their requester. The dead copied-entry
correlation query is removed; unresolved provenance is represented directly instead of
waiting indefinitely for a nonexistent producer. Retained parent/child replay proves
exact copied text, unchanged parent admissions, source-order/restart/reread stability,
and surviving child reads after origin deletion and rebuild. Targeted mutations detect
restored false admissions and child requester attribution. Richer inherited-context
navigation remains open; positive native copy certificates and historical snapshot
ownership handling are implemented above.

Pi fork-source identity now joins the exact recorded path to retained native inventory
and header evidence in the same environment and vendor. Companion ownership cannot
stand in for a native session header. Late, repeated and conflicting certificates use
the shared indexed owner-refinement mechanism, with bounded continuations that survive
restart; the former Codex-specific refinement module and generic-source type placement
are removed. Independent certificates retain separate proofs and competing identities
never choose a current origin by arrival order. Production replay covers every ordering
of the retained Pi headers and inventory, same-cut rebuilding, independent-scope deletion,
a held continuation across restart and unchanged older views after a conflicting binding.
The existing Codex routing/provenance regressions also exercise the shared mechanism.
Targeted scope, proof, persistence and work-bound mutations fail the new regressions.
This establishes source identity, not copied-entry membership or a local-work boundary;
the remaining local-production proofs and their execution/usage effects remain open.

Explicit fork lineage no longer requires a fabricated context checkpoint. Codex native
and live fork fields and Claude copied-record origin stamps produce separate retained
relations. The reader exposes a shortcut only for one resolved origin; multiple or
unresolved origins never select a target by arrival order, and all retained relations
remain inspectable. Header shortcuts independently recheck the lineage proof and target
metadata while preserving the fork's own view. Retained Codex and Claude production
replays cover restart, reread and same-cut rebuilding, and browser navigation preserves
the fork draft without borrowing origin controls. This closes the previously unconditional
empty-origin projection for recorded identities. Targeted mapping, ambiguity,
authorization and navigation faults were observed failing the regressions before
restoration. Native Pi copy proofs now complement these links; explicit
unavailable/multiple-origin surfaces remain open.

The agent directory now contains its focused agent and every proven spawn descendant,
with forks and communication excluded from membership. One preorder interval and the
incoming edge proofs are retained per agent; independent trees have independent
coordinates. Current authorization filters metadata before bodies, while unavailable
incoming proofs skip entire subtrees without borrowing hidden ancestor metadata.
Private forward continuations bind the retained view, query, grant partition and deletion
revision. Metadata-only passes finish exact filtered counts without hydrating bodies;
visible pages retain their own position. Lease scope pressure rotates only the directory
window at the same generation/cut/revision and preserves the main reader's lease.
The obsolete direct-child materialization and separate count scanner are removed.
Production storage/HTTP regressions cover deep membership, topology correction, deletion,
revocation during an opened scan window, long navigation and corrupt body cells. The
late-page workload reports 457 SQLite VM steps at both 1,000 and 10,000 agents; restoring
the loose seek scans a growing prefix. Removed focus and parent-link lookups seek the
latest retained row before checking liveness, so superseded versions cannot turn an
unavailable scope into an exhausted query budget. Same-revision component moves prefer
the live row over the old component's tombstone. Reader pool initialization now loads
SQLite's fixed schema before installing per-query VM budgets; forced pool expansion
cannot masquerade as history-dependent scan work. Pure projection tests pin linear proof storage
and unrelated-tree stability. Retained replay comparisons include interval endpoints,
parent identity and normalized incoming-proof epochs, and the observed delegation replay
asserts focused directory membership through the production scan. Browser tests cover
empty search prefixes, progressive counts, returned views, First page and cancellation
after changing search. Full-browser verification also exposed follow-up waits that could
accept the session setup greeting. Distinct echoed replies now exercise the intended
turn and expose a live reader defect: following refreshes must request the latest
window, while accepted corrections and expired-view recovery preserve the anchor.
Following is explicit reader intent rather than a distance recomputed after layout
changes; pending updates offer a latest-window action to resume arrivals. Page-boundary
and paused-resume regressions fail when anchored live refreshes or disabled following
are restored. Targeted mutations establish regression sensitivity. These close the directory scope boundary;
the remaining S2 certificates and S3 functionality/acceptance stay open.

Explicit observed-content transfer owns an independently bounded lease at the reader's
exact snapshot, streams immutable retained intervals and rechecks access while producing
bytes. Complete text copy validates UTF-8 before changing the clipboard; downloads retain
gaps in a manifest and separate ZIP entries, with transactional file output or a bounded
browser buffer. Transfer buffers stay outside reader caches. Production API regressions
cover exact large bodies, old views, revocation/deletion, preparation and output deadlines,
disconnect cleanup, capacity, byte budgets and empty content. Browser coverage exercises
large multibyte copy, visible-range copy, cancellation, exact archive delivery, file commit
and rollback, complete previews and opaque downloads. Each new regression was observed
failing under a targeted mutation, including revocation while the final chunk waits on
a full output queue. Full evidence export and paced
playback remain independent reader requirements.

The protocol now declares typed agents, activations, messages and immutable revisions,
deliveries, operations/results, independent resources, history selections, context,
measurements and evidenced claims. Reference encoding preserves namespace and tuple
boundaries; capture preserves independent producer receipts, source positions, fixed
ingestion epochs, payload manifests, attachment availability and clock domains. Claim
validation refuses cross-kind equivalence, unpriced monetary values and exact context
inferred from a summary.

The independent evidence and control stores implement FULL-durable admission, source
occurrence reconciliation, permanent deletion receipts, immutable accepted inputs,
at-most-once dispatch ambiguity and a bounded outbox exporter. Chunk manifests are fixed
before control acceptance, so large accepted inputs reach evidence without changing their
receipt identity. Repeated source bytes at distinct positions remain separate; differing
manifest layouts of the same occurrence compare their decoded bytes. These modules now
back the product runtime; accepted controls and capture evidence have independent
durability and one bounded outbox export path.

The envagent capture registry now reserves physical sources durably, verifies the complete
committed prefix, preserves rewrites as new generations and stages incomplete lexical
tails. Complete-record cursor advancement, producer identity, fixed authorization epoch
and chunked capture enqueue share one FULL transaction. Re-adoption recaptures the same
source positions under its new epoch; concurrent readers cannot advance past one another's
committed prefix. The runtime now uploads this queue through the capture wire and
processes durable receipts; vendor process bytes are spooled before control decoding.

The pure `sylph-trace` library maps server controls and the initial Codex native/live
surface, retaining decoded payload provenance and explicit archive-reference requests.
Reduction separates own outcomes, unknown field refinement, reversible reference classes,
message revisions, deliveries/resources and selected history. Direct and inherited
selections use one validated predecessor graph; source extents require explicit complete
mapping witnesses before an empty selection is known. Recorded fixtures verify exact
input/answer bytes, source checkpoint replay, copied headers, vendor ordinals, child
execution after parent completion and stream sealing. This is not full vendor conformance.

The generation store now applies complete bounded projection batches and serves temporal
summaries, membership pages, anchored lookups and claim provenance. Query candidates have
explicit work budgets; invalid-version density produces an advancing continuation rather
than an empty completed page. Payload anchors pin representation and body revision.
Writer fencing, validation, sealing and revision floors are implemented, with measured
SQLite work and fault-tested concurrency.

Inventory-scoped capture and immutable Launch controls now precede native identity. An
evidenced binding supplies the actual Agent without retagging accepted input or allowing
a second launch. Monotone epoch cutoffs preserve deletion through delayed identity binding
and re-adoption. The evidence catalogue owns publication jobs, immutable seals, pointer
CAS, reusable subject-bound leases and scoped payload pins. Publication validates a
bounded committed tail behind durable fences and resumes safely after timeout or process
death. Fresh capture admission requires owned byte-transfer receipts or an exact same-scope
source-occurrence proof, including retained sidecar references; a known CAS hash alone
cannot import another scope's bytes. Upload and physical
collection work share durable roots and guards, including shared byte/manifest digests.

The production projection worker applies validated bounded observation units, persists
source checkpoints and deferred dependencies with the generation cut, and resolves late
identity without needing an unrelated later event. JSON preflight and mapping run on
bounded CPU workers; decoding gaps suspend semantic continuation explicitly while raw
capture stays retained. Immutable derived payloads preserve selector, rule, evidence,
MIME and encoding. Source-owner definitions require retained vendor identity proof;
capture routing hints never establish ownership. Claude and Pi mappers now cover their
recorded native/live slices, branching, media, task/outer-run outcomes, preserved context
membership and reported usage. Controlled real captures establish a Claude success pair,
Pi failed settlement, and a Pi 0.85.1 success pair against a deterministic loopback
provider. The latter establishes actual Pi streaming/persistence while clearly labeling
the provider's synthetic marker bytes; it supplies no missing live/native certificate.
The controlled Pi retry and manual-compaction pairs now retain failed inference attempts,
retry scheduling/progress and recovery, and separate live/native summary checkpoints.
Production replay verifies their exact result bytes, own outcomes, outer settlement and
unchanged claims/reader projections across source ordering, duplicate delivery, restart
and same-cut rebuild. This exposed compaction membership proofs borrowing later snapshot
evidence; predecessor proofs now remain scoped to their coherent source view. Missing
maintenance ends release the association without inventing completion, and explicit native
assistant errors remain diagnostics without invented execution. These cases extend S2
acceptance; the complete vendor clause/certificate audit remains open.

Claude workflow bundles now retain resource identity and own status separately from child
activity snapshots and keyed journal invocations. Source inventory binds manifests and
journals to the observed containing transcript; retained launch IDs and exact journal
locators supply the remaining joins. The recorded success pair preserves both child
answers and scoped usage. The recorded failed host knows one child in its manifest but
two in its journal, with neither invocation returned; host failure cannot erase the
second child or finish either invocation. Production replay covers both bundles with
late joins, restart, redelivery and same-cut rebuilding. Progress indices and journal
keys remain unjoined, and absent child Activation boundaries remain unknown. Broader
vendor certificates, reusable-child delivery cases and reader acceptance remain open.

Native Codex goals now keep their actual instance IDs, explicit status and separate
cumulative token/time counters, including seconds as a typed measurement unit. A null
budget stays absent; malformed status stays unknown rather than becoming a vendor value.
The retained database row and schema anchor acquisition and production replay tests for
WAL reads, unknown columns, equal-objective replacement goals, unavailable rows, reread,
source interleaving, restart and rebuild. Public goal events still lack an instance ID
and cannot override a database goal by matching text or recency. Their outer and inner
thread identities are checked independently. Bounded reader state histories are implemented;
resource interaction and visual acceptance remain part of S3.1/S3.2.

The controlled Codex goal-context capture now records a real goal continuation, explicit
pause and interruption, and an ordinary input containing the exact same goal-looking
text. Complete host annotations identify harness context and notices independently of
the reported user role; explicit user-input annotations and typed UserMessage items
identify task input without inventing a human Subject. Output phases apply only to
assistant output. Targeted mutations were observed failing for annotation alignment,
part shape, source grammar, mixed authorship, notices, phase scope, typed input and
text-prefix guessing. Production replay verifies exact equal bytes with distinct message
identities, native/public ordering, restart, reread, rebuild, independent turn outcomes
and the native goal's paused state and scoped counters. The private provider witness
reports the goal trigger, while the actual public/native turn-start surfaces omit it;
ordinary production mapping therefore preserves the missing trigger and goal-instance
join explicitly. This closes the captured goal-trigger availability check for this
observed version without introducing a provider interception path. Remaining vendor
creation/delivery and schedule certificates stay in the S2 clause audit.

Controlled Claude read and Bash captures establish the SDK's `tool_use_result` field
independently of native `toolUseResult`, including plain file bytes versus numbered caller
text and a complete output sidecar versus its caller preview. Mapping and acquisition now
select the actual transport field, preserve explicit null/empty results, and refuse
ambiguous content pairings. Outbound SDK writes cannot manufacture returned results,
admission or child identity. Production replay covers live-only prefixes, early sidecars,
restart and rebuild. A shared-file regression also exposed sidecar result identities
collapsing different operations; the operation now qualifies each result while the
physical bytes remain shared. The new tests were observed failing under their targeted
mutations, including the SDK follower retaining its earlier revisions after file removal.

The controlled Pi Bash corpus now executes two calls with the same reported ID and
independent full-output files. Runtime associations follow the observed invocation;
finished IDs, interleaved entry snapshots and outer settlement cannot retag another
call or invent its outcome. Progress preserves each result without growing checkpoint
proofs. `fullOutputPath` binds at its actual runtime, message, native-entry or RPC-tree
pointer. Echo associations require a contiguous message prefix and exact count, with
bindings stored in the archive rather than accumulated in mapper state. Production
replay drives acquisition's selectors through early sidecars, native/live interleavings,
restart and rebuild while preserving unjoined native/live calls and shared physical
bytes. Targeted mutations fail the new regressions. The broader S2 and S3 gates remain.

Claude schedule results now resolve through their retained built-in invocation rather
than generic result field names. The controlled create/list/delete cycle proves
registration, cancellation and failed deletion independently; empty lists and ID-less
firing notices do not invent state transitions or delivery. Late invocation evidence
resolves a pending schedule result across restart, and production replay preserves the
same resources and facts across source ordering and rebuild. Unrelated durable listings
retain unknown identity scope until a creator/project certificate is available. These
cases extend vendor conformance; the clause-level S2 audit and reader gates remain open.

Retained accounting fixtures now distinguish Codex response, turn, response-event thread,
thread token-count and latest samples, including native cache-write tokens. Missing actual
owners cannot borrow causal root identities. A controlled Claude process with two queries
establishes query-local usage versus cumulative model usage and total cost; typed model
scope survives projection and prevents cross-model deltas. Pi compaction and branch-summary
usage belongs to its ContextCheckpoint, preserving explicit zeroes without invented
inference. Production replay checks duplicate receipts, rereads, source order, incremental
restarts and same-cut rebuild. Targeted mutations fail the accounting regressions. The
complete S2 audit and S3 gates remain open.

The retained Codex reusable-child excerpt now tests two child activations, three incoming
communications (including repeated equal task text), independent parent calls and child
replies. It exposed root headers omitting `agent_path` despite explicit root `session_id`,
and late path resolution rewriting immutable claims. Routing now uses the vendor's root
identity contract and retains stable containing-history message addresses; evidenced
refinements explicitly supersede unresolved author/recipient declarations. Fresh received
messages remain local history instead of borrowing an inherited-owner label from their
sender. Genuine copied communications retain their original containing-history identity,
including opaque encrypted parts, without replaying delivery or triggering the child.
Copied authorship stays unknown until the original message is retained and returns to
unknown after origin deletion while the copied bytes remain readable. Metadata pairs
require adjacent retained positions, and task-input purpose cites that metadata directly.
The copied-prefix, inheritance-label, metadata-gap and ciphertext regressions were
observed failing before their fixes or under their targeted mutations. Production
replay covers native source order, duplicate receipts, rereads, restart and same-cut
rebuild; synthetic two-peer cases verify independent certificate arrival, and targeted
mutations fail the new regressions. Independently retained headers corroborating one
route now produce certificate-specific immutable refinements in every source order.
A persisted index targets the affected declarations, and bounded continuations resume
from committed coordinates without remapping source bodies. Production regressions cover
restart, same-cut rebuilding, one-proof and last-proof deletion, and a late certificate
refining more declarations than one mapper page. Ordinary goal polling retains every
resource state while keeping one current identity witness; only explicitly corroborating
bindings accumulate alternatives.
Conflicting path certificates now retain each target with its own evidence and remove
the single-target lookup instead of stopping replay. Local non-root paths use the same
refinement rule, so later conflicts cannot bypass earlier local matches. The reader
keeps agreed content and metadata while leaving disputed authorship, purpose and
production unresolved. Production regressions cover reversed source order, worker
restart, frozen snapshots, authorized payload ranges, retained claim witnesses and
resolution after one conflicting scope is deleted. Production evidence inspection now pages authorized claim previews, loads one bounded
claim fact and its original supporting sources on demand, and follows explicit correction
links in both directions. Exact retained source ranges preserve duplicate JSON keys,
binary bytes and lexical whitespace. Private cursors bind the subject, view, query and
grant partition; denied candidates advance under a cumulative work bound, and source
reads recheck access after reading. Metadata remains inspectable when a source body is
corrupt, while that range fails explicitly. Browser regressions cover lazy reads, exact
failed-cursor retry, bounded mobile facts/references, correction navigation, safe text
rendering and revoked-access cleanup. Targeted faults were observed failing for exact
bytes, source membership, denied claims, continuation order, cumulative work, eager source
reads, post-read revocation, correction indexing and preview bounds. All three browser
regressions were also observed failing for cursor retry, changed source bytes and
unbounded supporting-observation rendering. The correction
query stays at the same measured VM work when later revisions grow; its regression
fails when the publication-aware index is removed. Full evidence export remains separate. The new regressions were observed failing under their
targeted mutations before the implementations were restored. The remaining vendor certificate audit and S3 reader
gates stay open.

The controlled Claude delegation bundle now retains a synchronous hand-back and a
SendMessage resume of the same child, including native child metadata and both answers.
Task handles are reusable: each observed start has its own activity identity, creator,
states and scoped usage. Old invocation-qualified notifications cannot finish a newer
activity, and overlapping unqualified updates remain unresolved. Child result fields
are interpreted only through their retained built-in invocation; a failed SendMessage
remains failed independently of the SDK result. Explicit origin fields keep human and
channel provenance, while forwarded input cannot become human by default. Missing
forwarding joins report temporary mapping status instead of permanent order-dependent
failures. Production replay covers all retained sources, restart and rebuilding; full
vendor certificates and reader acceptance remain open.

Codex input correlation now carries the durable command identity through every adapter
input method. Turn start, steering and native queue writes use that identity instead of
a transport counter, whose restart could alias retained queued work. Dispatched control
facts identify the exact submitted revision without asserting receipt; retained UserMessage
client IDs establish admission and separate representation relations. A qualified native
recording certificate relates model-history and display input without replacing either
vendor ID. The reader presents each submitted delivery once and preserves its original
bytes, selected-path restrictions and both retention epochs. A real loopback app-server
bundle and production replay cover equal text, separate IDs, late control, transformed
vendor content, source order, restart and rebuild. Targeted mutations fail these new
regressions. Goal triggers, independent route proofs,
forwarding provenance and the remaining S2/S3 gates stay open.

Resource inspection now pages retained state observations instead of presenting payload
availability as resource state. Goal updates, schedule registration/firing/cancellation,
yield, closure and unknown states retain independent claims, source positions and explicit
ordering limits. Creating/using operations and known activations remain linked. Immutable
revision details page their own representation bodies rather than selecting current message
content. Resource-state and revision payload anchors share the existing snapshot, containing
agent, selection and retention checks, and state-only payload references now load bounded
metadata even without a changed declaration. HTTP tests pin pagination, old-view membership,
exact bytes, cursor scope and deletion; browser checks cover explicit expansion, backward
paging, retry, bounded mounted pages and desktop/mobile accessibility. The mobile check
exposed an invalid dialog role on the inspector's aside element; its container now keeps
valid dialog/complementary semantics. Append details show the exact retained segment and
its predecessor, including selected-path reachability. Targeted mutations fail the new
regressions. Broader S2/S3 gates remain open.

### Runtime and reader cutover checkpoint

The product path is now acquisition → durable capture/upload → evidence → pure mapping
and reduction → published reading projection. The three runtime adapters retain process
control, approvals, native inventory and exact byte acquisition; they no longer emit
block-era semantic events. Source-state and late attachment observations preserve gaps,
missing files and later payload availability without rewriting parent captures. Durable
command receipts preserve at-most-once ambiguity through envagent restart.

The server uses control metadata, evidence/CAS, generation and search stores. The old
journal/history/native stores, event supervisor, block endpoints and compatibility
readers are removed. Typed trace routes serve agent bootstrap, selected history,
relations, object details, body parts/ranges, current controls, commands, search and
multiplexed changes. Subject-bound views, selected-object payload anchors and current
grants authorize reads independently of control permission. Search checkpoints pin
matching result views; deletion invalidates views and removes accepted input through
bounded erasure jobs. Startup recovery rebuilds a missing/corrupt published projection
from retained evidence, and the product schedules garbage collection.

The own-activation catalogue is now independently paged at a retained snapshot. It
includes proven activations without readable history, keeps unresolved owners outside
invented agent containers, and exposes each activation's own outcome and detail link.
The History inspector provides this catalogue alongside recorded history selections;
opening an activation retains its view without restricting it to the selected branch.

Administrator reprojection requests return a durable job receipt before replay begins.
Concurrent requests share the active job. Status and cancellation remain available while
the current generation serves reads and captures; cancellation and completed job status
are ordered against the publication pointer, with interrupted jobs reported after restart.

Codex mapping now includes runtime operations/resources, collaboration events and goal
metadata alongside native/live content and scoped accounting. This is implementation
coverage, not a certificate for every vendor shape. Selected-rollout metadata and frozen
multi-file history bases resolve physical rollout IDs, verify recorded ordinal and byte
boundaries, preserve inherited ownership in child-scoped aliases, and resume bounded
pages after restart. A reverted suffix remains unknown until its base is proved.

The frontend now reads opaque agent references and normalized reading units at
`frontend/app/agents/[ref]/page.tsx`. It provides agent/history/search inspection,
explicit payload loading, operational command receipts and a composer using the exact
control target. Shared bounded metadata/body caches, reusable view leases, virtualized
history windows, stable reading anchors, paused corrections, hidden-reader subscription
release and a paged assistive mode replace the old execution-tree client. Browser
coverage includes a retained Codex corpus projected by the production mapper, separate
synthetic layout fixtures, and actual server/envagent fixture execution.

Live Codex append occurrences now appear before finalization, retain repeated equal
segments in immutable manifests, and preserve replacement/final boundaries. Historical
selections resolve only their included revisions; a later final cannot replace a selected
prefix. Detail-page retries preserve the failed cursor, snapshot and already visible data.
Claude live parts and Pi compact RPC deltas also retain readable unfinished prefixes,
stable part identity and independent outer settlement. Raw Codex protocol boundaries
preserve exact failure/interruption evidence, and child-result observations establish
accepted deliveries without inventing history admission or child execution.

Document departure and cache invalidation release retained views; cancelled server reads
keep ownership until response assembly completes. Writer pools discard the orphaned
transaction state reproduced by cancellation during SQLx custom-BEGIN setup while
reusing healthy rolled-back connections. These regressions do not replace the broader
crash and mixed-load acceptance matrix.

Stateful source waits checkpoint one blocked head and an indexed replay cursor, so a
long admitted tail no longer fills the pending envelope queue before its releasing payload
can be mapped. Replay remains scoped to its source incarnation, authorization epoch and
evidence cut, and resumes bounded pages after restart. Reader materialization reuses its
previous committed rows and anchors; failed publication cannot advance that cache.
Mapper inputs also share the semantic archive for each immutable reduction. A new
reduction refreshes both object declarations and their supporting evidence, while
source metadata remains current for the bounded observation unit.

Entry topology is now separate from immutable content observations, and delivery identity
is separate from its observed revision bindings. Reobserved native entries retain their
predecessors and readable content without losing the distinct source revisions; selected
extents and snapshot observations restrict both rendering and payload reachability.
The retained Codex corpus and Claude/Pi pairs now pass independently specified body,
identity, ownership and lifecycle assertions across cold/incremental/restarted replay,
redelivery, overlapping reread, source interleaving and same-cut generation rebuilding.
These marker bundles do not cover every vendor operation, delayed sidecar or fault schedule.
The observed Codex app-server delegation replay now also checks child command/resource
lifetimes, exact runtime output, parent completion before the child's own end, retained
answer rows and activity notifications across restart, replay, unrelated-source interleaving
and same-cut rebuilding. It retains all inbound frame bytes from the pinned recorder
artifact. A sole unclosed observed activation now remains visible in its agent header after
an earlier activation terminates, even without live following coverage. The capture supplies
no child follow-up, spawn result, delayed sidecar or native/live pair; these remain separate
conformance work. Distinct-scope epoch provenance is covered by the closed S1.1 gate above.

Reader candidates now carry source and target authorization metadata in covering indexes.
Current grants and deletion epochs filter candidates before body hydration, including
hidden prefixes longer than one work budget. Public continuations encrypt skipped
positions and bind the Subject, exact lease and query/grant partition. The catalogue
retains their private key across restart. Counts are exact only when a bounded scan
exhausts the authorized scope; otherwise completeness is explicit. Projection contract
changes expire incompatible views and rebuild from evidence before reads resume.
Production HTTP regressions cover hidden targets, deletion on either side of a directory,
grant changes and multipart continuations; deliberate faults establish their sensitivity.
Embedded agent links now check the target header's retained epoch through a metadata-only
point lookup. Deleting or re-adopting a parent cannot expose its old metadata in a surviving
child view. Missing target headers are redacted; lookup work uses the candidate budget
independently of page size. HTTP regressions cover retained views, re-adoption and unreadable
target body cells, with deliberate authorization and hydration faults observed failing.

Projection writes and worker caches now bind the full generation, evidence cut and view
revision. A stale same-cut continuation cannot overwrite newer committed state, and an
uninitialized cache cannot hide a durable continuation. Mapper restoration checks the exact
snapshot in its read transaction. Recovery verifies the published revision's durable cut
mapping, and publication validates a fixed baseline before its bounded tail. Deterministic
regressions cover independent writers, same-cut cache refresh, zero-cut restoration and
persisted rollback; these do not establish bounded incremental reduction cost.

Typed reduction differences now retain changes and removals across every semantic family,
with old/new dependency endpoints. Object materialization consumes this delta, and a warm
receipt with no semantic, payload or epoch change reuses the committed reader with zero
rows materialized. The worker reports constructed reader rows as actual work. The full
comparison still scans both reductions. Independent complete-message tails now materialize
only the new message rows and their predecessor detail boundary, using shared immutable
row/anchor maps. The append proof rejects older unresolved references, selected boundary
collections, submitted delivery tails and changed existing payload previews. General
message replacement, streaming and topology updates still reconstruct the reader.
The production replay oracle now also compares row order, authorization metadata,
membership, counts, payload anchors and retained active payload bytes/manifests/roots.
Same-archive replay preserves exact epochs; separate corpus archives normalize only
verified scope identities, rejecting distinct epochs for one scope.

Mapper inputs now share immutable claim, evidence, correlation and physical source indexes.
Ordinary source records no longer rebuild history metadata by scanning every retained claim.
Source coordinates are indexed once during acquisition and reconstructed on restart;
contradictory lexical witnesses fail instead of replacing a prior coordinate. Frozen-history
operations derive their witnesses from the exact retained claim snapshot when needed.
This removes a repeated mapping-input cost while full semantic reduction and general
reconciliation remain open.

The reducer now uses persistent ordered maps/vectors for growing semantic state and keeps
an indexed pending-claim set. A fresh complete tail can resolve directly when its nested
references, corrections, selections and source extents prove no older value can change;
the resolver emits a typed delta and work counters without running the full oracle. Missing
predecessors, deferred corrections/equivalences, selected-source completion, sibling/order
changes and dependency-budget exhaustion fall back to the full deterministic reduction.
The 20/200 tail oracle suite compares every reduction to the full resolver, including
restart and redelivery; the complete production scaling matrix and general incremental
reconciliation remain open.

Independent tails now carry reducer-issued proofs bound to their exact immutable before
and after reductions. The worker emits only new archive memberships, and the reader uses
persistent reverse indexes for boundary relations, selected entries, submitted deliveries
and coverage instead of repeating full-history proofs. Index capacity and guard fanout are
bounded separately, with explicit work counters and reconciliation on exhaustion. The
20/200 production comparison includes membership and reader-proof work as well as complete
persisted rows, payload anchors and authorization metadata. Restoring a missing reader cache
uses committed previews and epochs, so a new binding for an old entry cannot mask the row
update required at the next view revision.

Worker candidates now share persistent source, pending, replay, gap, captured-binding,
preview and epoch maps; large mapper states and previews share their values independently.
The mapper semantic cache shares persistent declaration/provenance maps and applies proved
tails using only new references and keyed supporting claims. Full reconciliation prepares
its semantic input before publication rather than deferring an archive scan to the next
append. Work counters distinguish full refreshes from actual object, content, alias and
claim visits. Epoch-only updates also skip unrelated membership reconstruction. General
semantic changes still rebuild the mapper view alongside full reader reconciliation.

Existing-message deltas, replacements and finals now have a separate reducer proof for
one stable message with fresh archive-tail entries. Message-local declaration/revision
claims retain activation associations, final-purpose refinement and late-segment semantics.
An indexed reader region preserves the first row position and replaces its current parts
and payload anchors atomically; prior views retain their exact bodies. Selected, delivered,
mixed-content and cross-agent regions reconcile conservatively, as do excessive dependencies.
The production 20/200 phase comparison covers turn-bearing frames, exact repeated text,
replacement/final/late behavior, retained-view authorization, restart and failed/cancelled
publication. Revision selection and late-segment detection now use source-domain extrema
and stable identity grouping instead of pairwise scans. Exact pairwise-oracle comparisons
cover corrections, ties, incomparable positions and support order; comparison-count tests
cover 256/4096 candidates. Repeated message-local resolution and flat composed manifests
still revisit the affected chain and require further long-stream performance work.

Search reconciliation now persists complete source fingerprints and reuses unchanged
FTS text across process restarts. Bounded source writes and stale-source removal remain
private until a fenced update publishes its exact snapshot; interruption and invalidation
cannot publish partial text. Search body composition shares the reader's evidenced append
logic, including retained prefix revisions and replacement boundaries. Source enumeration
and append-chain composition metadata still need scaling work, and obsolete search payload
pins currently remain until the search generation retires.

The measurement sentence in commit `3ed5bf5` combined different runs: the 1000-record
358 ms figure predates incremental reduction, while the measured incremental run used
100 records and reported 47 ms, 9 rows, 7 resolved claims, one history entry and no full
resolution. It is not evidence of 1000-record incremental performance. The recorded
negative controls also do not establish that every new dependency guard was mutated.

Open implementation/acceptance work remains in the ordered list above. In particular,
`crates/server/src/trace/worker/reading.rs` still materializes general reconciliation from
the whole reduction. Ordinary complete-message tails now share candidate state and update
mapper inputs, memberships and reader guards from certified additions. One existing-message
region can update locally; multi-message units, selected/delivered regions and other semantic
changes still need general incremental reconciliation. Bounded HTTP
pages do not prove bounded incremental projection cost. The ignored
`worker_projection_scaling_probe` measures this production path under
`SYLPH_WORKER_BENCH_RECORDS`; cold replay and repeated live updates still need the full
scale matrix. Existing deletion/recovery tests also do not substitute for
the full in-flight cancellation and crash matrix. A complete new production conformance
run and semantic negative controls are required before declaring the replacement done.
One full browser run timed out waiting for a fixture reply while the page showed only
submitted inputs; the complete commands test file passed on a clean rerun. The saved
snapshot responses did not establish the cause. Connected-load acceptance must reproduce
or explain this intermittent visibility failure rather than treating a retry as a fix.
Acceptance compares ordering, anchors and authorization metadata as well as rendered
bodies, and measures warm append/delta/final work at growing history sizes. Full
reconciliation remains valid for uncommon topology or equivalence changes, provided it
finishes privately within bounded writer units while the published snapshot stays readable.
Universal constant-time graph updates are not a completion requirement.

The milestone narratives below record earlier product delivery. Block-era implementation
details are historical; the trace checkpoint above owns the replacement status.

## Milestone 1 — the envagent is the only path — complete

Run every machine, the server's own included, through
`Client → Server → EnvBridge → Envagent → Adapter`. Route session creation, turns,
watch/adopt, files, health, and control through the bridge; bind envagent ingress
unconditionally; delete the in-process route rather than leaving it reachable. A server with
no envagent paired serves records, refuses session creation with a message naming the
missing envagent, and reports the link unavailable with a typed reason.

Agent availability becomes per environment and time-varying: discovered in `hello` against
each adapter's version floor, re-reported when a machine's inventory changes, and read from
`GET /api/v1/environments` rather than from a server-wide list
(`docs/environments.md` § Agent availability).

**Exit gate:** no route can reach an adapter except through the bridge; a single-machine
install runs two processes and pairs; creation with no envagent fails with the naming error;
upgrading a vendor CLI on a connected machine changes what that environment offers without a
re-pair or a server restart; browser and fault-injection tests cover restart and reconnect
with no duplicate or missing canonical event.

**Built:** product routes, default ingress, paired fixtures, version floors, live inventory
rediscovery, remote lifecycle reconciliation, typed command and file contracts, and the
removal of the server-side adapter, link, and supervisor execution seams. The server also
moved into the workspace crate layout as `crates/server`, which is what made "no route can
reach an adapter except through the bridge" checkable rather than asserted.

## Milestone 2 — environment and workspace context — complete

Persist environment facts, envagent identity, OS-user and home metadata, and Workspace
resources. Canonicalize paths only in the owning envagent. Associate sessions with a
workspace when the vendor provides a reliable path, and preserve an unknown path as unknown.
Add multi-environment listing, native enumeration, and workspace filtering.

**Built:** environment machine facts and envagent identity are persisted and surfaced;
native paths remain nullable when unknown; workspace rows are scoped by environment;
filesystem roots and tree browsing dispatch to the owning envagent, which performs all
canonicalization, containment, symlink, and metadata work; frontend history, recents,
pickers, and session creation carry the environment alongside the path.

**Exit gate:** session identity remains `(environment, agent, native_id)`; live and reload
produce the same tree; no remote path check happens in the server.

## Milestone 3 — authentication, environment access, and transport — complete

Add registered-user Subjects alongside the bootstrap development identity, with login and logout, and explicit
grants under [ADR-0006](adr/0006-environment-authority-and-capability-grants.md):
`grant_authority` per environment, `observe` and `control` as separate grants, `administer`
reserved to the authority root, an invite issued locally on the machine, and the five
redemption checks. Filter sessions, search, workspaces, health, files, and raw records by
capability. Harden `sylph.env.v1` with TLS so an envagent can pair from another machine.

Four pieces the authority model needs and nothing else provides:

- **`subjects.is_admin`**, the authority root for every `grant_authority = server`
  environment. The schema, provisioning, and route gates now exist.
- The **two authority-root-gated routes**, grant revocation and unpairing, since ungrantable
  is not the same as unreachable and `DELETE /api/v1/environments/{id}` exists today with no
  gate defined.
- **`CloseReason` itself.** The exit gate below reads `close_reason = grant_revoked`, and the
  wire contract publishes the six-value enum and calls it authoritative in `crates/protocol`.
  The `turns.close_reason` column and `CloseReason` type now exist; revocation writes
  `grant_revoked` on affected in-flight turns (`docs/session-protocol.md` § Outcomes).
- **Subject provenance on in-flight work.** [ADR-0004](adr/0004-concurrent-control-ordering.md)
  decides the record retains `origin_subject`, and [ADR-0006](adr/0006-environment-authority-and-capability-grants.md)
  scopes the revocation sweep by it. The `turns.origin_subject` column, model field, authenticated
  population, and scoped revocation sweep now exist.

**Exit gate:** unauthenticated requests cannot read business data; two independent browser
contexts observe the same authorized environment; an unauthorized environment is absent from
every read and control surface; an `observe` grant reaches no control route; revoking a grant
refuses new control and lands in-flight vendor work as `abandoned` on a turn carrying
`close_reason = grant_revoked`; two Subjects with different grants on one environment receive
**identical** `link` facets and differing `controlBlock` values; an `is_admin` Subject can
revoke a grant and unpair a `server` environment while reading no session it holds no
`observe` grant on; a `machine` environment refuses both routes to a browser session with no
local confirmation code; unpairing leaves every session listable, readable, and searchable;
an envagent pairs and reconnects over TLS from a second host.

**Built:** registered Subject authentication with login/logout, authority-root grants and
revocation, one-shot invites with scope/expiry/consumption/provenance checks, capability
filtering across reads and control, subject provenance and grant-revoked turn closure, and
rustls transport for off-host envagent connections. Draft handles now carry their creator
Subject and re-check ownership plus the current grant on every metadata, capability,
configuration, file, and promotion operation. The bootstrap Subject remains the explicit
development identity used by local fixtures; the first registered Subject is provisioned as
the server authority root, and bootstrap never overwrites a valid registered cookie.

The transport and contract gates are executable: pairing covers HTTPS URL normalization and
IPv6 literals, all registered routes are present in `docs/api-v1.openapi.json`, and mutation
Origin checks apply consistently to auth, pairing, grants, and application writes.

## Milestone 4 — lossless native ingestion

Implement raw vendor records, sidecar and blob references, interior observability states,
event-time axes, re-projection, and retention classes under
[ADR-0005](adr/0005-raw-retention.md). Complete Claude, Codex, and Pi parity as separate
adapter work items. Large bodies stay out of default timeline payloads, and a missing or
unreadable source is an explicit state.

**Exit gate:** raw round-trip and blob integrity pass; retention never prunes raw; an absent
vendor time is not rendered as a measured zero; live, reload, vendor-specific, and
filesystem-free retained re-projection tests pass.
Claude, Codex, and Pi now emit lossless `NativeRecord` values before projection. Retention
confirmation for ordinary blocks remains conservative when no native-record correspondence
exists, so those blocks are retained until a durable correspondence is available.

On-demand native refresh and filesystem-free native projectors are implemented. Native
records preserve first-seen order and full revisions; all three adapters retain user input,
timestamps, delegation context, and live-envelope projections. Re-read and re-project use
bounded disk-backed transfers, so frame size does not impose a session-size limit. History
is cursor-paged with a snapshot sequence, timeline bodies are lazy with integrity-checked
fetches, durations honor fidelity and clock domains, notification streams reconnect, and
refresh reports pending, success, and concrete failure states. Storage corruption now fails
the read instead of becoming an empty successful transcript, and sidecar availability can
recover without colliding with the retained identity.

Finite imports acknowledge only after the complete transcript has been ingested. Bulk
sync subscriptions survive restart, share discovery, and refresh only changed source
revisions. Native reads have separate bounded command capacity from interactive work;
sidecar and source-close delivery wait for a fixed producer watermark. File roots come
from the server's durable record. Transcript and file queries are cancelled on navigation,
and failed capability reads no longer masquerade as an offline environment.

Storage isolates native retention in `native.db`, uses independent read pools and one
writer per database, and acquires write intent before read-modify-write transactions.
History upserts preserve an integer address shared with FTS, and publication/pruning use
bounded batches with coalesced background scheduling. Transactional usage counters replace payload scans in accounting and
spool capacity checks. Producer acknowledgement reads a transactionally maintained
contiguous cursor instead of rescanning all receipts. CAS sweeping examines bounded dangling references with publication
excluded. Sync now discovers new identities within the latest authorized workspace/agent
scope, including an initially empty workspace, and reuses catalogues for refresh.
Concurrency tests hold competing write transactions open; VM-step regressions check that
FTS replacement and spool enqueue cost do not grow with unrelated history or backlog.

The M4 acceptance suite covers raw ordering and unsigned revisions, sidecar transitions,
large summaries and bodies, native user-input provenance, all three adapter reprojections,
bounded transfer framing, pagination, reconnect recovery, and mobile focus/refresh behavior.

Execution nodes now provide the interactive read path: bounded direct-child windows,
transactional ancestor summaries, stable addresses across archiving, and revision patches
with former-parent removals. Ordinary stream ids are turn-scoped; delegated trajectories
retain ownership and promoted tool bodies. The browser keeps a bounded entity/window cache,
virtualizes expanded rows, and loads execution details and bodies only when requested.
Ordering corrections reposition the affected loaded row without sorting its siblings.
Live cache eviction retains a history cursor at the loaded edge so discarded rows remain
reachable by page-up. Both regressions were reproduced against the preceding implementation.
Execution groups default to collapsed; outward replies split groups. Expand-all and the
full-tree browser derivation are removed. Copy/export and full replay are explicit reads.

Native persistence failures now Nack the retained producer prefix and report a distinct
synchronization fault without declaring an agent stopped. Delivery stops at a Nack, and a
bounded replay task retries alongside command handling. Regression coverage includes
archive identity, late parent declarations, cycles, corrected failures, pagination,
transaction lock windows, and fault-injected query scans. Measurements belong under
`docs/measurements/`, and distinguish bounded node reads from full-turn exports.

The follow-up workload measurements exposed quadratic stream-metadata lookups during
consolidation. The fold now indexes those facts once and runs on a blocking worker under
the existing consolidation gate. That fold measurement did not justify producer sharding or a new scheduling policy.
Authenticated file reads remain responsive while native retention, archive publication,
search, pruning, CAS deletion, and continuous discovery overlap in the disposable harness.
Raw lexical bytes, archived block counts, search removal, and new identity discovery are
asserted after the run; GC deferrals are measured separately from successful deletion.

Timeline cache ownership now preserves visible summaries when evicting closed child
windows and releases unreferenced moved nodes. Older windows cannot acquire a disconnected
live tail, and callers can explicitly return to the latest window. Initial-read retries
and cancellation/restart preserve the initial-page boundary. Browser regressions cover
stable reading anchors during pagination, including height corrections in the gap below
timeline controls. These defects were reproduced before their fixes; the activity-index
and lazy-body regressions also fail with the corresponding storage fault restored.
The baseline, bounded-work, and browser measurements are in
`docs/measurements/performance/storage-baseline.json`,
`docs/measurements/performance/storage-bounded.json`, and
`docs/measurements/performance/file-tree-bounded.json`. Observed regression failures and
the corresponding restored behavior are recorded in
`docs/measurements/performance/regression-evidence.json`.

Archive eviction now uses snapshot row identities and versions, preserving same-turn
arrivals and in-place corrections during publication. Whole-turn reads merge an archived
prefix with its later journal tail. Root input ownership commits with
event projection; sealed ordinary streams ignore late deltas while completion corrections
and independent delegates remain accepted. Interactive search merges complete hot bodies
with history FTS, deduplicating logical nodes and excluding superseded body revisions.
Tool summaries have one server producer for both counters and headers, exercised through
retained records reprojected by the real Claude, Codex and Pi adapters.

Still open: producer sharding and byte-weighted scheduling have not been introduced;
full body requests still read whole retained bodies rather than independently compressed
ranges. Native checkpointing and separate current sync-state tables remain measurement-led
follow-ups. Next measurements must cover same-producer delivery through the actual
spool/socket/ACK path with concurrent large imports, small sessions, file commands and
reconnect recovery. Existing mixed storage and separate browser fixtures do not establish
delivery fairness. Idle branch polling, hidden-page suspension and revision notification
coalescing remain open, as do giant-body expansion, deep delegation, long-lived caches
and database growth. Explicit hot search materializes current bodies; measure its latency
and memory before introducing an incremental index or persistent checkpoint. Pi append
checks and the historical-task window queries still need independent synchronization
measurements. Atomic retained re-projection publication remains the separate boundary above.

## Milestone 5 — multi-client control convergence

Control intent is separate from transcript delivery. Send and guarded steer receipts name
a turn lease; adapter lifecycle ingestion shares the session command sequencer. Stop holds
the slot until the adapter settles. Native priority and follow-up queues remain available
with explicit delivery promises in the composer and environment settings; an uncorrelated
vendor queue receipt never asserts consumption. Live and historical sections expose durable
turn status, and fixture delegate closes author their terminal interior facts.


Allow authorized clients to submit control concurrently. The server assigns request ids and
sequence, persists accepted / queued / running / rejected state, enforces idempotency, and
broadcasts one authoritative stream. Define send and stop ordering, unsupported-queue
behaviour, and first-valid-wins approval resolution. Controller ownership and handoff need a
new ADR.

**Exit gate:** retries never duplicate prompts; concurrent clients converge;
independent-browser E2E covers ordering, queue rejection, and simultaneous approval.

## Milestone 6 — Khora sandbox and delegated execution

Provision sandboxes as ordinary environments with envagents, on `server` grant authority
since no OS-user sits beside one. Add lifecycle, pause and resume, health, cost and retention
policy, and completion notification. No sandbox-specific Session or Workspace model.
Khora is developed in the separate [TSHOGX/khora](https://github.com/TSHOGX/khora) repository;
its local development and deployment checkout lives in the OrbStack `sbx` microVM, so changes
at this milestone may require coordinated implementation and verification across both repos.

Depends on the Milestone 3 transport: a sandbox is off-host, so it cannot dial in until the
wire carries TLS.

## Milestone 7 — optional administration and finer permissions

Only after real requirements: raw deletion and audit, capabilities finer than the three in
[ADR-0006](adr/0006-environment-authority-and-capability-grants.md), sharing UI, directory
scopes, delegable granting, and controller handoff. Each addition needs its own ADR and
threat-model review.

## Conversation and work panel delivery

The current contract is [unified-trace-ui.md](unified-trace-ui.md), with shared shell
rules in [ui-layout.md](ui-layout.md). The normalized reader replaces the earlier
conversation/execution-tree implementation; its old acceptance results do not certify
the replacement.

| Stage | Deliverable | State |
| --- | --- | --- |
| Contract | Agent identity, reading units, independent lifecycle and bounded reads | Specified; typed protocol and server projections implemented |
| Runtime integration | Exact acquisition, trace routes and action targeting | Connected through the server/envagent product path |
| Reader | Conversation, agent directory, histories, payload/details and files | Implemented against normalized reads |
| Resource ownership | Snapshot windows, leases, cache bounds, anchors and hidden readers | Implemented with unit and browser regressions |
| Acceptance | Full suites, visual matrix, real devices and production scale | Automated regression coverage implemented; complete conformance and manual acceptance remain open |

Manual acceptance remains open for a real VoiceOver reading pass. The user explicitly
excluded Safari testing after the connected-device probe, so Safari-specific acceptance
is outside this delivery's remaining gate. An isolated fixture reader and inspector were
visually checked at 390×844 and 320×568: no document overflow, a visible 16px composer, and a draft
preserved through resizing and inspector navigation. Chromium viewport checks and axe do
not establish Safari toolbar, keyboard, safe-area or screen-reader behavior. Production
deployment and any real-store rebuild require a separate task.
