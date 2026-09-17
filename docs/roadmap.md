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
| Production semantic acceptance | Foundation implementation below | Retained three-vendor corpus replay now preserves observed identity/content across source order, restart and reread; full semantic and failure matrices remain open |
| Projection and delivery scale | Milestone 4 and foundation implementation below | Reader pages are bounded; whole-reduction reader materialization and mixed spool/socket/ACK load still need production measurements |
| Directory scope and authorization | Conversation and work panel delivery below | Candidates are filtered before hydration, continuations are private and bounded counts cover authorized targets; full spawn-descendant membership and scalable exact filtered counts remain open |
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

**Next implementation, in dependency order:**

1. Stabilize the migrated architecture and primary stores. Prove
   cold/incremental/reread/interleaved/rebuilt equivalence through production admission,
   reduction and read projection, with independently authored expectations. Resolve
   core representation defects exposed by that replay before widening behavior. Complete
   the remaining required semantic mutations and mixed-source dependency schedules,
   including reusable children, independent resources and distinct result representations.
2. Extend query authorization acceptance under concurrent grant changes and deletion.
   Candidate filtering before hydration, private continuation positions and bounded
   authorized totals are implemented; preserve these contracts through incremental
   projection and future full descendant indexing. Replace first-arriving declaration
   epochs with deterministic retained-support provenance; the independently scoped
   parent/child replay currently produces different header and target epoch metadata
   under reversed source arrival. `crates/server/src/trace/worker/tests/epoch_order.rs` is an explicit ignored
   known-failure gate, not passing acceptance. Its fix must cover supporting evidence,
   contributor deletion, aliases, search and recipient-copy authorization together.
3. Measure the connected production pipeline under large retained histories, high
   version density, concurrent captures, reconnects and deletion. Replace full reduction
   and whole-reader materialization on routine updates with bounded incremental work;
   bound search indexing as well as returned pages. Extend fault coverage beyond job
   lifecycle and publication cancellation to the production storage and delivery matrix.
   Prove a valid correction larger than one projection batch completes through private
   reconciliation, and exercise the connected spool/socket/ACK path under concurrent load.
4. After these core gates stabilize, finish adapter evidence and mapping conformance:
   remaining runtime/delegation/accounting cases, source certificates and workflow/schedule
   coverage. Unavailable joins and selected-head evidence stay unknown.
5. Refine reader surfaces last: complete descendant navigation, structured-value
   exploration, observed-content transfer and separately budgeted export/playback, then
   visual, accessibility and scale acceptance including manual VoiceOver. The user
   excluded Safari testing. Synthetic layout fixtures do not establish vendor semantic
   correctness or production delivery isolation.

Each stage includes correctness, architecture, performance and authorization review,
followed by a bounded commit before another implementation stage expands the diff.

**Exit gate:** production cold/incremental/mixed-source conformance, demonstrated semantic
mutations, storage failure/performance experiments, and complete affected runtime/frontend
checks from [unified-trace-conformance.md](unified-trace-conformance.md). Existing synthetic
UI trees and the reference SQLite exercise cannot substitute for that evidence.

The milestones below remain the record of the current implementation and other product
work. Their block-era mechanisms are not requirements for the replacement. No store is
erased and no production trace implementation is claimed complete by this design review.

### Foundation implementation

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
reread; a known CAS hash alone cannot import another scope's bytes. Upload and physical
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
conformance work. Distinct-scope epoch provenance fails the gate above and remains open.

Reader candidates now carry source and target authorization metadata in covering indexes.
Current grants and deletion epochs filter candidates before body hydration, including
hidden prefixes longer than one work budget. Public continuations encrypt skipped
positions and bind the Subject, exact lease and query/grant partition. The catalogue
retains their private key across restart. Counts are exact only when a bounded scan
exhausts the authorized scope; otherwise they remain unknown. Direct relation counts
are labelled accordingly and do not claim a descendant catalogue. Projection contract
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
