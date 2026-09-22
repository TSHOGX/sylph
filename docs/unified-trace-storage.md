# Unified Trace Storage and Read Protocol

Status: **Normative target design; not the current database implementation.**

[unified-trace-foundation.md](unified-trace-foundation.md) owns the objects and semantics.
This document makes their durable capture, publication, and query contracts concrete.
Implementation gates live in [roadmap.md](roadmap.md#unified-trace-foundation-review);
the falsification matrix lives in [unified-trace-conformance.md](unified-trace-conformance.md).

## Authority and database boundaries

SQLite remains the engine. This is a typed relational representation of a semantic
graph, not an EAV database and not a requirement for a graph server. Write workloads
are separated without dividing the authoritative evidence into hot and cold semantics.

| Store | Durable responsibility | Writer |
| --- | --- | --- |
| `control.db` | Subjects/grants, native Session metadata, command identities/revisions/leases, command outcomes, immutable evidence outbox | Short operational transactions, `WAL`, `synchronous=FULL` |
| `evidence.db` | Captured source inventory, complete observations, admissions, receipt prefixes, payload roots, publication catalogue | Bounded append/admission transactions, `WAL`, `synchronous=FULL` |
| `trace-<generation>.db` | Versioned semantic claims and objects, resolved relationships, temporal query summaries, mapper checkpoint and change log | One bounded projection writer per active/building generation, `WAL`, `synchronous=FULL` |
| `search-<generation>.db` | Rebuildable FTS5-jieba index, revisioned document metadata, index checkpoint | Separate bounded search writer; search lag is explicit |
| `blobs/` | Immutable raw bytes, chunks, payload manifests and derived body chunks | Publish and verify before any durable reference |

These filenames specify the replacement layout, not additional tiers beside the former
journal/history/native model. New initial schemas replace the old ones in place. No
production data is erased by a documentation change.

Operational policy is authoritative in `control.db`; the admitted trace is authoritative
in `evidence.db`. An outbox is a pending transfer of an operational fact, not a second
independent interpretation. Query databases are disposable interpretations with named
builds and evidence cuts. Loss of a query database requires rebuild; loss of unreplicated
evidence cannot be fixed by assuming a vendor still has equivalent bytes.

Each writer uses bounded transactions and independent read connections. Expensive JSON
decoding, compression, tokenization, graph ordering and payload I/O happen outside the
evidence/operational writer transaction. CPU work runs on bounded workers. No SQL cursor
is held while waiting on a browser, vendor, filesystem read, or paced replay.

Cross-file atomicity is never assumed. SQLite explicitly excludes WAL from its
[atomic attached-database transaction guarantee](https://www.sqlite.org/lang_attach.html).
`ATTACH` may support read-only diagnostics, but neither durable acknowledgments nor
publication depends on one transaction committing several WAL files.

## Capture contract

CaptureAdapter is the vendor-facing runtime on the envagent. It owns authentication,
process I/O, native filesystem reads, native database transactions, source inventory,
and sidecar acquisition. SemanticMapper is a pure vendor module over retained captures,
run by the server's archive worker. It cannot open the envagent's paths or send commands.
Both responsibilities belong to the vendor adapter package; splitting them does not
create another agent implementation.

The capture envelope carries:

```text
Capture {
  observation_ref, source_position, ingestion_authorization_epoch,
  declared_subject_refs[], captured_at,
  source_clock?, transport_kind, vendor_build?, schema_fingerprint?,
  payload_ref, attachments[], capture_diagnostics[]
}
```

The server issues the ingestion authorization epoch for the capture scope/inventory
before acquisition. The envagent persists it with every spool occurrence; admission
validates it rather than assigning the current epoch from arrival time. A pre-deletion
capture that was never admitted cannot acquire a new epoch merely by arriving after
re-adoption. Fixed epoch metadata does not change native SourcePosition or producer
receipt identity.

An ingestion scope is either an evidenced AgentRef or a server-issued InventoryRef
containing environment, vendor and inventory instance. An inventory can precede native
session identity and admit captures with no declared agent yet. It authorizes acquisition;
it is neither an AgentThread nor a reserved Session row. Binding a native identity later
adds evidence and known inventory membership without replacing earlier capture bytes,
source positions or authorization epochs.

The full native line/frame and lexical encoding survive; extracted routing headers are
indexes, not substitutes for bytes. JSON duplicate keys and escape spelling remain in
the raw representation even when a semantic parser refuses them. Native SQLite capture
uses an actual consistent vendor-supported read transaction/snapshot; reading a database
file while ignoring its WAL is not a capture.

The exact source version/build and acquisition surface are recorded whenever exposed.
Unknown semantic kinds are retained with coverage diagnostics. An incompatible Sylph
capture envelope is rejected before its producer prefix is acknowledged; there is no
dual-spelling wire fallback. Vendor format evolution is each mapper's external contract.

### Source identity and generations

Source identity identifies a physical/logical source stream, not an agent. SourceRef
is the foundation tuple of issuer environment/server and durable source-instance ID;
independent envagent registries cannot collide. Source path is
provenance. A durable envagent registry relates actual file/API identities and captured
generation boundaries; recreating a follower cannot allocate a new source for every poll.
The registry survives reconnect and reread. A lost registry requires explicit new-source
inventory and evidenced lineage before semantic equivalence can be asserted.

For JSONL, position includes the exact byte interval and source ordinal. A complete line
commits only after its bytes are retained. A trailing incomplete record stays staged,
does not advance the complete-record cursor, and is reported as pending. If the source
ends with permanently malformed bytes, those bytes are retained as an undecodable
occurrence with a diagnostic. Empty semantic output is not success.

Staged bytes remain split into bounded durable chunks. When a generation is replaced
or becomes unavailable, a durable drain job transfers its staged complete records and
terminal fragment to the receipt-gated queue in bounded passes, without reading the
vendor path again. The fragment carries an incomplete-record diagnostic. Positions,
original capture authorization and capture clock stay fixed to that staged generation;
a later adoption cannot retag them. Each queue insertion and staging release commits
atomically. Delivery drains these jobs even when no further source poll occurs.

Staging reserves metadata credit sufficient to enqueue one record from its bytes.
Queued drain records borrow that credit until their terminal receipt, so repackaging
cannot deadlock a full spool or exceed its byte ceiling. Drains have independent item
and byte budgets, with one record allowed to exceed the pass byte budget only within
the record-size bound. A finite import freezes its drain-job frontier and waits for
that work and its receipts; later source closures cannot extend the barrier.
Native ownership proofs are scoped to the admission epoch that retained their header.
Separate retained copies of the same identity position can prove the same source
binding. Their proof receipts remain independent; a differing proof is not a changed
locator or owner, and inventory announcements do not recursively become native identity.

Source generations are immutable prefixes. Append extends a known prefix; truncation,
replacement, in-place revision, or changed native content revision starts a new generation.
A new mapper build or a newly installed vendor binary alone changes provenance or mapper
generation; it cannot turn identical retained source occurrences into new ones.
Reusing a source position after restart/reread requires verifying the committed prefix
against retained chunk digests or an authoritative immutable source revision. Size,
mtime, a filename, or a sampled tail alone cannot prove prefix equality. Full verification
may be linear in source bytes and is budgeted background work, not a metadata request.
When continuous tailing relies on a vendor's append-only contract, that basis is recorded;
later contradictory prefix evidence creates a generation/conflict without rewriting the
old capture. A generation number is capture bookkeeping, not the vendor's history head.

Concurrent follower, reread, and inventory jobs share source reservations in the
envagent. Capture SourcePosition stays immutable across admission epochs. On the server,
`UNIQUE(admission_epoch, source_ref, generation, position)` plus verified digest
gives a second idempotence boundary. A new producer epoch does not make an old native
occurrence new. Overlapping range captures are resolved to the same complete-record
positions before semantic mapping. Distinct positions with identical bodies stay distinct.

A copied file becomes a separate observed source with optional proven prefix lineage.
It is not automatically either an append to the original or a duplicate. Sources that
cannot expose stable positions use durable capture occurrence keys and explicit mapping
coverage; the server never suppresses messages by a content digest.

### Sidecars, mutable locators, and large records

A sidecar locator is not its bytes. Capture records locator, subject/source relation,
observed version or capture occurrence, digest when available, and availability state.
An unreadable sidecar can later gain a captured body. A mutable path returning different
bytes produces a new observed version, not a collision hidden behind that path. Two
different byte strings claimed under one immutable version are a conflict.

CAS chunk addresses use BLAKE3 of the decoded exact bytes. Storage compression/encoding
is explicit object metadata and never an identity input; decoded length and digest
are verified on read. Payload manifest identity includes ordered chunk/range references
and representation metadata. SHA-256 in research inventories is a source-verification
choice, not a competing runtime address scheme.

Chunk transfers commit their offset, digest and staged durability before chunk receipt.
The final manifest commits only after all expected chunks and its digest verify. The
referencing record can name an explicit missing/unavailable sidecar; it cannot claim
complete coverage before completion. Record and payload byte limits bound transfer chunks,
not how much of an already observed native record the server silently throws away.
Oversized semantic decoding can stop with a resource-limit diagnostic while the original
bytes remain retained. Partial uploads never appear as complete payloads.

Payload references distinguish `bytes:<BLAKE3>` from `manifest:<BLAKE3>`. The digest
addresses immutable physical CAS bytes; the reference kind determines whether those bytes
are the payload itself or its manifest. Both references can share physical bytes without
sharing interpretation. A prefix inside manifest bytes cannot establish this distinction:
an arbitrary raw capture can contain exactly those bytes, including the prefix. Logical
payload keys therefore include the kind, and neither insertion order nor content sniffing
chooses how retained bytes are read.

## Admission and acknowledgments

Admission has two deduplication axes: producer delivery and source occurrence. They are
both required. Receipt identity is unique on `(environment, producer, epoch, sequence)`;
source occurrence identity follows the preceding section. Receipt deduplication stays
global across deletion/re-adoption. A newly authorized admission epoch must carry a new
ObservationRef and freshly captured bytes; replay of an old receipt can confirm only
its old receipt status and cannot resurrect deleted content. The default epoch lasts
until an explicit scoped deletion/re-adoption action changes it.

For one bounded contiguous producer batch:

1. Verify envelope version, current environment authority, source bounds, and referenced
   payload digests. Publish immutable bytes durably before a SQL reference can commit.
2. `BEGIN IMMEDIATE` on the evidence writer. Existing equal receipts are no-ops. Existing
   receipts/positions with incompatible bytes produce an explicit collision response.
3. Insert new observations or occurrence links, assign monotone `admission_seq`, and
   advance only the contiguous accepted producer prefix in that same transaction.
4. Commit with FULL durability. Only then acknowledge the prefix and announce evidence
   progress. Semantic workers consume the durable admission log independently.

Transport batching cannot skip a failed item and acknowledge later positions. A durable
malformed vendor payload may be acknowledged as retained evidence with a decode gap;
an envelope or storage failure may not. This distinction prevents endless retries of
bytes already archived while keeping admission faults honest.

Receipt dispositions distinguish retained evidence from an explicit deletion discard.
A stale ingestion epoch covered by an authorized deletion tombstone receives a durable
`discarded_by_deletion` disposition naming that tombstone, without re-retaining its
body. That terminal disposition can advance the contiguous receipt prefix so one deleted
scope does not strand unrelated spooled work. It is never reported as evidence admitted,
newly captured, or projection-complete. Old outbox items use the same disposition before
being marked transferred. Other admission failures remain Nacks and cannot skip a prefix.

A newly admitted capture owns durable upload receipts for its declared payload chunks,
fixed to its ObservationRef and ingestion epoch. A global CAS digest alone cannot import
another scope's bytes or count an erased occurrence as recaptured. An exact reread may
reuse payloads already rooted by that same SourcePosition in the current epoch. An
attachment relation can likewise name an already admitted sidecar SourcePosition: its
issuer, fixed admission epoch, source generation, locator and primary payload reference
must all match an admitted observation of the retained occurrence. Rereads can supply
different manifest layouts of the same decoded source bytes; each admitted primary
representation remains eligible. The server reconstructs that payload's bounded
transfer declarations from retained metadata, pins its chunks for the upload, and
rechecks the source proof in the admission transaction. This also works after the
vendor removes the sidecar. A known digest, a path, a source position in another epoch,
or an unadmitted source cannot supply this proof. All other new payload references
require provided and verified bytes. Upload roots precede physical
publication, and complete upload ownership is checked in the receipt transaction.

The envagent spool preserves its existing enqueue-before-send and terminal-receipt-before-delete
discipline. Bounded queue exhaustion applies backpressure, then records the actual
capture/execution consequences through reserved diagnostic capacity. Killing a controlled
process can stop further local output; an observer cannot claim it stopped an external
agent. Any missing interval remains a capture gap, not an invented tool failure.

All live observations offered as durable trace content follow this path, including
deltas. Group commit can amortize fsyncs, but there is no acknowledged 50 ms loss window
and no durable-looking precommit event publication. Network rendering can batch committed
changes without altering the source arrival timestamps retained in evidence.

### Operational facts use an outbox

Command acceptance commits the command ID, immutable submitted revision, authorization
decision/lease, retry policy, and fixed outbox occurrence including the ingestion
authorization epoch at acceptance, in one FULL `control.db` transaction. An outbox first exported after deletion cannot be retagged
with a later re-adoption epoch. An exporter idempotently admits that occurrence to
evidence, then marks it exported in control. A crash before marking exported simply retries the same occurrence.
Export never dispatches the command again.

Dispatch, native queue acceptance, rejection, unknown outcome, and actual consumption
each produce separate operational/vendor facts. A lost response to an at-most-once
command remains unknown and is not automatically resent. Exactly-once evidence export
does not claim exactly-once effects at a vendor that lacks that guarantee.

The first input can target a Launch inventory before the vendor supplies its native ID.
Its accepted target and exact input remain immutable. An IdentityBound fact names the
command, actual native AgentRef and the observations proving that identity; it must match
the inventory's environment and vendor. Until that proof arrives, mapping reports an
unresolved archive reference instead of inventing an agent or losing the initial bytes.
Later input targets the actual AgentRef. A restart can leave a launch's effect unknown;
evidence export and identity binding never authorize another launch attempt.

An evidence cut includes only exported/admitted operational facts. Responses expose
pending outbox count and oldest pending age separately from projection lag. Waiting for
"my sent message appears" can target its outbox observation/command ID, rather than a
session-wide running bit. Accepted user bytes remain durable in control until export
is confirmed; deleting the outbox row before then is prohibited.

## Logical tables and constraints

The implementation uses typed columns for keys and fields queried by core consumers.
Large bodies live behind Payload references. The following table families are a schema
contract; the exact Rust member spelling is defined once in the protocol implementation.

| Family | Essential key/constraints and purpose |
| --- | --- |
| Sources and generations | `(source_ref, generation)`; immutable provenance, capture contract, prefix extents and verified lineage |
| Observations and admissions | Globally unique ObservationRef; unique admission-epoch/SourcePosition; immutable raw payload reference; ordered admission index |
| Producer receipts | Producer/epoch prefix, atomically committed with admissions; acknowledged rows never depend on semantic projection |
| Evidence payload roots | Observation/manifest references; immutable object digest, encoded/decoded length and availability |
| Semantic claims | ClaimRef, kind, subject, typed value, rule/build, evidence joins, supersession joins |
| Identity equivalences | Typed endpoints and proof; reversible derived equivalence classes; no destructive owner rewrite |
| Agents, activations, messages, deliveries | Foundation object keys and owning references; immutable delivery-to-revision observation bindings indexed independently of delivery identity and stages |
| Operations, result occurrences, resources | Invocation/result/resource keys; explicit requester, activation and lifetime references |
| History entries, content observations, selections, context checkpoints | Independent predecessor adjacency; immutable content observations grouped by entry/source/slot; selected entry and observation membership; original ownership |
| Measurements | Scope/metric/mode/counter-domain keys; series position uniqueness; no unqualified total column |
| Diagnostics and coverage | Subject/source/dimension/interval, state/reason, evidence; structured conflict alternatives |
| Query summaries and memberships | Object reference, `valid_from`, `valid_to`, selection/order/membership, bounded preview, immutable body revision refs |
| Projection checkpoints and changes | Applied admission cut, view revision, mapper build, atomic changed/removed/moved object set |

Same-database relations use foreign keys where the referenced identity is resolved.
Unresolved references have a typed resolution record; they do not require placeholder
entities with fabricated fields. Cross-database joins validate through durable manifests
and typed references, never unenforced foreign keys to another SQLite database.

Core indexes lead with query scope, then ordering/cursor key and reference: agent history
by selection/order; activations by agent; operations by owner/activation or resource;
agent edges by source/relation and target/relation; deliveries by recipient and message;
measurement samples by subject/metric/domain/position. Claims have subject/kind and
evidence reverse indexes. Temporal tables additionally index current rows and the bounded
validity predicates used by snapshot pages.

Directory and related-object candidates carry typed target-environment authorization
metadata independently of the recording epoch. Resolve current Subject grants before
hydrating candidate bodies. A hidden target contributes no row, identity or count and
cannot turn an otherwise readable catalogue into a whole-page authorization failure.
Source/target deletion epochs remain checked even after an environment is authorized.
Embedded agent links use the target header epoch at the same snapshot. A missing or
deleted target is redacted without withholding the containing row; re-adoption cannot
authorize an older target epoch. Target checks read bounded indexed metadata, not bodies.
Catalogue totals use the same target-grant and epoch predicates as rows. A bounded
metadata count reports an exact value only after exhausting the scope at that view;
otherwise it returns `value: null, complete: false`. It never substitutes a global
projection count or the number of loaded rows. Direct spawn-relation counts are labelled
as direct relations and do not claim full descendant traversal. Candidate indexes cover
authorization, validity and filtering columns so rejecting a row does not read its body.

History content indexes lead with entry and stable slot, and separately with source,
generation, position and mapping rule. Reverse bindings connect revisions to their
content observations and observations to selections. An agreeing observation can add
provenance without rebuilding topology. A content change invalidates affected reading
rows, parts, payload authorization anchors and search membership together. Rendering
and selected-object authorization consume the same selected binding set; a source-bound
view cannot render one revision while authorizing another source's unselected revision.

No request discovers all siblings by decoding payloads, walks all ancestors to open one
agent, counts entities by rescanning source JSON, or builds a full execution tree in the
browser. Materialized membership/count indexes update in projection work. If an ancestry
or ordering correction affects an unbounded region, the worker queues bounded pieces
and publishes that change only after its affected state is complete; it must not expose
half-updated counts or block the evidence writer.

## Reduction and projection checkpoints

The implementation separates value-only protocol types, envagent capture/control, pure
mapping/reduction, and server storage orchestration. The protocol crate keeps its existing
no-runtime dependency rule. Pure mapping/reduction is a library with no vendor-process,
network, filesystem or clock dependencies; its API takes captured values, prior mapper
state and a fixed archive context, and returns claims, new state and typed unresolved
archive-reference requests. The worker supplies those references from the same evidence
cut. Mapper state is checkpointed with its build and projection cut, never hidden in a
process-local global current-message variable. Vendor mapping modules remain separate
within this library so an adapter review has one semantic owner per vendor.

Pure mappers consume capture surfaces with source context at a fixed archive cut. Output
claims cite the relevant observations, including pairing/adjacency proofs. Shared
reduction resolves identities, valid ownership, histories, lifecycles, coverage and
query objects. Live and native mapping use the same rules. There is no current-turn
fallback, global tool-name classifier, or digest suppression path.

The generation manifest pins semantic schema, mapper builds per vendor, reducer build,
capture inventory interpretation, and derived encoding versions. Version change rebuilds
a new generation rather than interpreting old rows with a partly new reducer.

Projection transactions apply a bounded complete unit of claims, resolved object updates,
temporal query revisions, and change-log entries, then advance their checkpoint. If a
single observation expands beyond that unit, claims can be staged, but its visible cut
advances only after the complete observation is resolved or explicitly quarantined.
Parser failure retains a diagnostic and coverage gap at the cut; it does not publish
guessed values. Dependent unresolved claims are indexed for reevaluation on later evidence.

Every projection commit compares the writer fence and the complete prior snapshot
`(generation, evidence_cut, view_revision)` inside its write transaction. A continuation
can advance the revision without advancing admission, so checking the evidence cut alone
cannot reject a stale prepared unit. Rejection leaves both durable checkpoints and the
worker's committed cache unchanged; retry restores the current snapshot first.

Worker caches name that same complete snapshot and have an explicit uninitialized state.
A default zero evidence cut is not proof that a cache reflects a persisted zero-cut
continuation. Claims restore from their fixed temporal snapshot; mutable mapper
checkpoints restore only after that exact snapshot is validated in the same database read
transaction. A stale restore fails instead of combining older claims with newer mapper
state. The worker installs the checkpoint returned by a successful commit, never one
predicted before the transaction.

A committed mapper checkpoint grows with retained history, so restoration reads it in
bounded pages, as claim restoration already does, and each page validates the same exact
snapshot. The worker's byte limit bounds one page, never the retained total: a limit
compared against the whole checkpoint is a ceiling on how long a history may become
before a restarted worker can no longer resume it, which the retained-evidence contract
does not permit. Every page delivers at least one entry, so no single large value stalls
restoration, and the page cursor is the last key delivered rather than the first one
skipped.

Mapper input snapshots share immutable ordered claim, evidence, correlation and source
indexes with the candidate worker state. Later captures cannot mutate a mapper's frozen
input, and constructing an ordinary input does not scan the retained claims or rebuild
physical history tables. Source records are indexed at acquisition and restored by source,
generation and local ordinal. Conflicting byte coordinates, vendor ordinals or headers
at one native coordinate fail explicitly while raw captures remain retained. Frozen-history
resolution derives its completeness and entry-position witnesses from the retained claim
snapshot when that operation needs them.
Candidate worker state shares persistent roots for source checkpoints, pending work,
payload previews, epoch bindings and captured-payload bindings. Large mapper-state and
preview values are shared independently so changing one tree leaf does not copy neighboring
bodies or chunk lists. Semantic mapper inputs also share persistent roots. A valid
independent-tail proof updates only its new declarations, content observations and keyed
supporting claims; an unsupported transition rebuilds that cache with full reconciliation
before publication, from the exact reduction. Initial/restored state initializes it once
before use. Held mapper inputs remain immutable across both successful
publication and discarded candidates. Work reports full semantic refreshes separately
from the objects, observations, aliases and supporting claims actually examined.

A stateful source awaiting an external payload retains one blocked head and a replay
cursor over its admitted tail. Successor envelopes stay in evidence instead of consuming
one pending slot per record. The replay index binds source, generation and ingestion epoch,
contains canonical occurrences only, and seeks within the fixed evidence cut. Resolving
the dependency resumes bounded pages from the last committed mapper state; a long blocked
tail cannot prevent the projection from reaching the later attachment that releases it.
Independent correlation joins do not impose this source-order barrier.

Current projection may lag evidence admission. Serve both cuts, worker status, affected
source gaps and diagnostics. Do not turn projection failure into an agent failure.
Full replay is deterministic over the complete cut. Incremental scheduling can change
when a revision appears, but cannot change its final semantic value for that cut.

General topological corrections are not claimed to be constant work. The reducer uses
indexed adjacency and deterministic order labels; a bounded affected region updates
incrementally, while a larger relabel/reconciliation rebuild runs as unpublished work.
This keeps admission and already-published pages available without pretending that
arbitrary graph updates have an O(1) algorithm.

A valid reconciliation exceeding one writer batch reserves a future view revision and
persists a bounded chunk plan. Each chunk commits privately, including temporal mapper
state, while every read and processed-occurrence lookup remains restricted to the
published checkpoint. The final transaction publishes its change notice, commit proof
and checkpoint together. Cancellation before the plan is complete discards only its
private preparation; a complete plan resumes after cancellation or restart. A generation
writer reservation spans these bounded transactions and is released on process death;
fencing drains a pending reconciliation before sampling its cut. Per-observation decode,
identity and semantic-expansion bounds still apply: a valid large change to existing
rows is not an excessive source record. Oversized change notices use the existing
backlog-exhaustion response to require a fresh view.

The incremental resolver retains structurally shared reduction maps, history order and
relation sequences. Newly validated claims are pending inputs until resolution commits;
exact redelivery creates no new pending work. A fresh predecessor chain can update its
new message, revision, content and history objects without resolving prior claims when
an indexed dependency check proves those new identities were not previously referenced.
Nested ownership, delivery, revision, selection and claim references participate in that
check, including unresolved references and corrections awaiting their targets. Selected
source extents and explicit reader-order constraints can require full reconciliation.
The dependency index has its own bound; exceeding it reports that incremental indexing is
unavailable and retains full semantic resolution, rather than dropping evidence.

Incremental resolution emits typed deltas directly. Several unpublished dependency passes
compose their initial and final endpoints, cancelling net no-ops without comparing the
entire archive again. The full reducer remains the independent oracle for replay and
reconciliation. Resolution work reports claims processed, history entries updated, full
resolutions and dependency indexing separately from materialized reader rows.
The guarded tail resolver issues an opaque proof bound to the exact immutable reductions
before and after the update. Only consecutive proofs can compose. Membership and reader
projection consume that proof rather than rediscovering the unchanged archive prefix or
rescanning old missing-reference dependencies. A restored, replaced or unrelated reduction
cannot borrow the proof, even when its values compare equal. Full reconciliation issues no
tail proof. Reader-only boundary dependencies remain indexed and separately bounded;
payload availability and source/target epochs are still independent invalidations.

An update to an existing message may resolve that message's retained declaration and
revision claims locally while appending fresh history entries. Its opaque proof names the
exact reductions and affected message; prior references to newly declared identities,
corrections, equivalences, selections and non-tail topology still require reconciliation.
The reader retains the first rendering location, its observed bindings and order position.
It replaces the affected row, parts and payload anchors together, preserving old snapshots.
Work may grow with the affected message's revision/part chain, never unrelated history.
Flat payload manifests still require work proportional to their chunk count; delta-only
total work is not claimed for arbitrarily long streaming bodies.
Revision selection indexes ordered coordinates by source, generation and locator kind.
Frame ordinals and JSONL byte starts are separate domains; missing, snapshot and opaque
occurrence positions never imply strict order. Surviving final/correction candidates take
precedence together, retaining conflicts and every supporting claim in their original
order. Otherwise each domain retains all candidates tied at its maximum coordinate.
Late append detection uses the earliest surviving seal in the same ordered domain.
Selected-history filtering precedes both correction and order evaluation. Selection and
late detection take O(K log K) comparison work for K eligible revision candidates;
one long message must not cause pairwise scans merely to select its current revision.

Reader invalidation consumes typed changes in every resolved semantic family, including
old and new endpoints and removed values. Object declarations alone cannot invalidate
execution, delivery bindings, content observations, selected membership, equivalence or
coverage. Payload availability and source/target epoch bindings are independent inputs.
A unit with none of these changes reuses its committed reader materialization. Work
measurements count rows actually constructed, including any baseline restoration, rather
than counting only rows whose final serialized bytes differ.

Reusing a committed reader region requires a dependency proof, including absent
references: a newly declared revision may satisfy older unresolved content. Independent
complete message additions at a proven archive tail append rows and payload anchors
without reconstructing earlier history rows; the predecessor's detail relations still
update. Reused regions retain their exact order labels and
authorization metadata. Submitted deliveries at the end of an archive, selected-history
membership, message replacement, adjacent work grouping and changed payload availability
can require reconciliation beyond the newly added entry.

Candidate reader state shares immutable committed rows and anchors; modifying the
candidate cannot affect an existing view or the next retry. Only the generation commit
installs the candidate. Sharing must avoid copying the full row catalogue merely to add
one row. This does not by itself bound semantic reduction, dependency proof evaluation or
larger topology reconciliation.

## Generation publication

A bounded interpretation continuation can advance `view_revision` while preserving
`evidence_cut`; it cannot mark a new source occurrence processed. Its mapper checkpoint
and aliases commit atomically. Publication validates the complete sequence of revisions,
including equal-cut refinements, and finishes pending continuations before sealing.

Publication switches a complete semantic generation, not one session's events followed
later by its directory and bodies. All agents and edges used in a graph read resolve
inside the same generation. Partial rebuilds still publish a complete generation
manifest that explicitly reuses unchanged immutable partitions; the initial implementation
uses full generations to keep that proof simple.

1. Record a job and capture evidence cut `C0` in the publication catalogue. Pin its
   evidence/payload roots while building a private generation.
2. Replay through `C0`, then catch up from the durable admission log. Validate object and
   ownership constraints, history DAGs, payload references, receipt/source consistency,
   and the declared coverage gaps. An unknown source is allowed only with its diagnostic;
   unexplained missing required data blocks publication.
3. Acquire the short projection-publication barrier, not the evidence or control writers.
   Fence and drain the old projection writer, including in-flight commits, and snapshot
   its last committed cut `Cold`. Select a new cut `Cpub >= Cold` from already admitted
   evidence. Catch the private generation up to `Cpub`, drain/fence its writer exactly
   there, and validate its tail. Both projection writers remain fenced through the
   pointer transaction; no old commit can race the sampled `Cold`. Evidence admission continues.
   If the tail cannot finish within the configured barrier budget, release, resume old
   projection, and retry after further background catch-up; never wait indefinitely.
4. Commit/checkpoint and verify the new database files and payload manifests, sync their
   durable files/directories, and record a sealed ready manifest containing `Cpub`, mapper
   builds and file identities. The seal certifies the initial publication checkpoint;
   the generation database subsequently grows through transactional updates, so a
   whole-file digest at publication is not a permanent checksum of the live database.
   Recovery validates its current checkpoint, WAL and payload references. A WAL is part of a live database; copying or renaming just
   its main file is not publication.
5. In one FULL evidence-catalogue transaction, compare the expected active generation
   and replace its pointer with the ready manifest. This transaction is the linearization
   point. A retry cannot publish over a different concurrent job's active pointer.
6. Route new reads to the new generation and resume its incremental worker at `Cpub+1`.
   Accepted later evidence is still in the durable log. Existing old-generation snapshot
   leases retain their files until expiration; send generation-change notices.

Publication does not wait for the optional search index to catch up. Search is available
only at its own declared cut with matching generation; until then it reports indexing.
It cannot silently query the previous generation or label partial results complete.

On restart the catalogue chooses the active generation; directory names and newest mtime
do not. A ready-but-unpublished generation is resumable/discardable scratch work. A
published but corrupt/missing file produces an explicit recovery state and rebuild from
evidence; any intentional fallback to a retained generation advertises its older cut.
No caller gets silently mixed objects from both generations.

Startup compares the published projection manifest with the current semantic, mapper,
reducer and encoding contracts before serving it. A differing contract enters explicit
recovery, cancels views of that generation and rebuilds from retained evidence. It does
not query older row layouts or add compatibility readers.
The recovered database must reach at least the published evidence cut and view revision,
and its durable revision record must map the published revision to the published cut.
Advancing the reader's revision floor does not discard this publication proof. Background
semantic validation names the exact snapshot used as the baseline for bounded tail
validation; a continuation committed during validation belongs to that tail.

### Crash obligations

| Crash point | Required recovery |
| --- | --- |
| Payload synced before admission | Unreferenced payload is GC candidate; producer retries same observation |
| Admission committed before ack | Redelivery returns prior prefix; one evidence occurrence |
| Control committed before export | Fixed outbox occurrence is exported; no command redispatch |
| Evidence export before outbox marking | Duplicate export resolves to the original occurrence |
| Evidence acknowledged before mapping | Resume mapping from checkpoint; no evidence loss |
| New projection transaction incomplete | Roll back that transaction; old visible revision/cut remains |
| New generation ready before pointer swap | Old generation stays authoritative for reads |
| Pointer committed before process exits | Reopen exactly the referenced durable generation and resume its tail |
| Arrival after publication cut | Replay from durable log; no discarded tail or regressed cut |
| GC interrupted | Retry bounded candidate work; never delete a live referenced payload |

## Read protocol

The following replaces the event/block-derived reading API. It is the authenticated implementation
contract for these endpoints. Agent/object references are
opaque encoded typed references. Routes remain under the existing authenticated API
origin and preserve environment-grant checks.

| Read | Contract |
| --- | --- |
| `GET /api/v1/trace/agents/{agent_ref}` | Bounded agent identity, known histories, current selection, coverage, cuts, own state, relations summary and authorized action target |
| `GET /api/v1/trace/agents/{agent_ref}/history` | Keyset page of directly readable entries under an explicit history selection; optional around-object anchor |
| `GET /api/v1/trace/agents/{agent_ref}/relations` | Direct related agents/interactions, paged by relation and direction; exact scope-labelled counts |
| `GET /api/v1/trace/agents/{agent_ref}/activations` | Bounded own activation summaries; unknown associations stay outside invented containers |
| `GET /api/v1/trace/objects/{object_ref}` | One typed summary, provenance and unresolved/conflicting fields; no eager body/tree |
| `GET /api/v1/trace/objects/{object_ref}/relations` | Bounded operations/results/resources/deliveries/evidence relations, with a typed filter |
| `GET /api/v1/trace/payloads/{payload_ref}` | An authorized object/snapshot-scoped manifest or bounded byte/part range; immutable revision required |
| `GET /api/v1/trace/changes` | SSE change invalidations from a bounded projection cursor, with reset/generation notices |
| `GET /api/v1/trace/search` | Authorized generation/scoped search at a declared search checkpoint; paged snippets and object anchors |
| `GET /api/v1/trace/reprojection` | Administrator-only active generation and bounded recent reprojection job status |
| `POST /api/v1/trace/reprojection` | Administrator-only durable job admission; returns `202` before replay completes |
| `GET /api/v1/trace/reprojection/{job_id}` | Administrator-only durable status of one reprojection job |
| `DELETE /api/v1/trace/reprojection/{job_id}` | Administrator-only cancellation request; returns the resulting durable job status |

The activation catalogue lists proven activations owned by the requested agent across
its retained histories, including those without readable content. Each row has an
activation reference, independent execution state and evidenced durations; its reference
opens the ordinary object inspector. Counts are scoped to `own activations`. Stable
reference order organizes the catalogue without claiming execution chronology. It uses
the ordinary snapshot, cursor, item/byte and authorization bounds. A history `selection`
is refused: selecting readable content cannot redefine activation ownership.

### Reprojection jobs

Reprojection admits at most one active product job. Concurrent submissions coalesce onto
that job in the same catalogue transaction that admits it; callers receive its stable
job ID, generation, state and timestamps. The states are `queued`, `running`,
`cancelling`, `completed`, `cancelled` and `failed`. Status reports include terminal jobs
and bounded progress metadata, never internal build tickets or filesystem paths. The
collection returns at most the latest 32 jobs; an individual job remains addressable.

Admission does not wait for replay or publication. A background worker replays retained
evidence into a private generation while current reads, active projection and capture
admission continue. Cancellation is cooperative at bounded replay checkpoints. It is
also checked in the publication pointer transaction: cancellation committed first
prevents publication; publication committed first leaves the job `completed`. The job's
completed state and published snapshot commit with the pointer. Cancellation never
aborts the coordinator while its publication guards or writers are fenced.

An accepted cancellation returns `202` while a running job unwinds, or `200` once it is
terminal. Repeating cancellation of a terminal job returns that same terminal state.
After exclusive startup recovery, an unpublished interrupted job becomes `failed`
with reason `server_restarted`, or `cancelled` when cancellation had already committed.
A pointer already committed before the crash remains authoritative and completed.

Action targets are typed routes, not a guessed Session ID. A Session control route is
returned only when that native identity addresses the focused agent. Embedded control
requires an evidenced vendor route carrying both host and child identity, supported
actions, and current lease selection. An embedded agent without such a route is
readable-only; its anchor Session must never become a fallback target. Authorization
for each actual action is reevaluated at dispatch.

Full evidence export and paced playback are explicit streaming jobs with separate byte,
work and cancellation budgets; they are not hidden implementation paths for an ordinary
history page. Playback follows captured admission/source timing and states any pacing
clamp. It is not reconstructed by reading only current object values.

### Snapshot pages and live changes

An initial reader request obtains a bounded reusable ViewLease. Related header, history,
operation, child and payload reads use `at_snapshot` to derive their query scope at the
same generation/cut/revision. A fresh scope does not silently acquire newer state.
Each pagination cursor separately binds its agent, selection, filters and ordering:

```text
ViewLease { generation, evidence_cut, view_revision, expires_at }
QueryScope { agent_ref, history_selection, filters, representation }
Cursor = opaque(view_lease, query_scope_hash, order_key, object_ref, direction)
```

The scope hash includes the complete QueryScope. A header or child detail can reuse the
ViewLease, but a pagination cursor cannot be reused for another scope. Cursors from
another agent/scope are invalid. Page responses carry both `next_cursor` and `has_more`,
actual returned counts, configured item/byte limits, and the cuts/coverage represented.
Server limits apply before bodies are read. Counts cannot mean "items loaded so far".

Public reader position cursors are encrypted and authenticated. Their associated data
binds the Subject, exact ViewLease and complete query/grant partition; their ciphertext
conceals positions of skipped unauthorized candidates. The catalogue owns one private
random key that survives normal restart and generation publication. It is not the
client-visible authentication token and is never serialized into config, views or
evidence responses. Missing or malformed key storage fails explicitly instead of
silently rotating it while retained views remain valid. Cursor input and output sizes
are bounded before decoding or encrypting.

Temporal query rows have half-open validity `[valid_from, valid_to)`. A transaction
closes prior summary/order/membership rows and inserts their replacement at the same
view revision. Body references name immutable payload revisions. All pages and body
lookups under a snapshot use its revision and selection. This fixes the omission/duplicate
window that keyset pagination alone leaves during late identity, order or branch updates.

Snapshot leases bound retained versions and old generations. Requests do not hold a SQL
transaction across HTTP. Expiration or version-floor eviction yields `410 snapshot_expired`
with an optional stable object anchor for reacquisition; malformed/mismatched cursors
yield `400 invalid_cursor`. A current-generation change does not rewrite an old snapshot.

SSE cursor is `(generation, change_revision)`, distinct from admission and producer
sequences. A committed change batch names changed, removed, moved and equivalence-resolved
references and the resulting cut. Batch framing is atomic. The client applies a bounded
batch or invalidates/reacquires the affected scope; it cannot overlay new values onto
an old snapshot and still claim that page is consistent. A missing change prefix yields
`reset_required`; no unbounded catch-up downloads a whole session by accident.
A multiplexed stream may name multiple retained views of one agent. Every named view
is authorized independently. The replay cursor must fall within the current generation
and retained revision floor; future cursors and generation replacements require restart.
The server returns bounded committed reference invalidations, or restarts when their
complete prefix cannot fit the configured item and byte limits.

At the live tail the client can advance to a new snapshot and preserve its logical
anchor. While reading history, it may hold the old snapshot and show new-work availability.
UI policy is specified in [unified-trace-ui.md](unified-trace-ui.md). Deep links preserve
object reference and desired selection, not projection row IDs; equivalence resolution
and changed membership have explicit responses. A withdrawn interpretation returns
`interpretation_withdrawn` with retained evidence/valid anchor where authorized, not
an indistinguishable missing-session response. Native object references are not reused
for unrelated objects.

### Query and payload bounds

The server owns configurable maximum items, serialized summary bytes, relation fan-out,
payload range bytes, concurrent reads, change backlog, snapshot lease lifetime, and
snapshot retained-byte pressure. The served trace limits are the one source for client
and tests; values must not be separately guessed in frontend code. UI acceptance targets
do not override these hard limits.

An individual summary is bounded independently, with truncated-preview metadata and
lazy detail references. A huge native record therefore cannot force an oversized page.
Payload endpoints require a `via_ref` authorization anchor and snapshot;
possession of a global CAS hash is not authorization. The anchor is a readable containing HistoryEntry, Delivery or Operation,
separate from original authorship. The requested payload, revision and representation
must be reachable through that anchor at the snapshot revision and selected history.
An R1 anchor cannot authorize its newer R2 payload. Retained inherited/copied bytes can
be read through their authorized recipient entry without access to the origin
environment. This grants neither origin-object metadata nor unobserved origin ranges.
Shared bytes do not expose another environment's existence or provenance. Range reads
report range, total length when known, encoding and
coverage; a body revision mismatch is a conflict, never a silent switch to the newest text.

Query validation includes SQL VM-step/scanned-row evidence at growing corpus sizes and
growing superseded-version density, not only an output row limit. A temporal validity
predicate plus a B-tree is not by itself proof of bounded work. If it scans too many
invalid versions, use indexed/persistent membership or an explicit resumable work-budget
result; budget exhaustion cannot return a false empty or complete page. Full export,
source prefix verification, rebuild, and search
over large data can legitimately do linear work; ordinary object/page reads cannot.

### Search consistency

Search documents refer to Message/Operation/Payload representation plus owner and
history membership. One original message copied into child context does not become
another child-authored search result. Query scope selects authored history, selected
context, or retained branches explicitly. Tool output and nonhuman context can be
searched without masquerading as human conversation.

The search worker consumes committed projection changes and commits its document
replacements and search checkpoint `(generation, view_revision, evidence_cut)` in one
transaction. Search responses identify that checkpoint, indexing status, and covered
scopes. The worker pins the corresponding trace revision/body membership before
advancing its checkpoint; temporal GC cannot overtake an index or its result jobs.
Large replacements may commit bounded private batches while the index reports indexing;
only the final transaction exposes the completed checkpoint. A fenced update binds every
batch to the same full snapshot and prevents an interrupted or invalidated writer from
publishing later. A durable source manifest fingerprints complete search membership,
body revision, payload and authorization epoch, so unchanged sources reuse their indexed
text. Source enumeration may be linear; payload reads and FTS replacements follow changed
sources. Reader change notifications alone are not a complete search dependency log.
If pressure requires releasing that pin, invalidate the search generation/jobs first
and report indexing until rebuilt. Snapshot leases for
search retain revisioned membership/snippets at that checkpoint; detail links either
resolve the same revision or explicitly open current state. Never combine current FTS
text with an earlier membership snapshot and call it a consistent result.

The initial implementation may pin one short-lived read snapshot inside a materialized
bounded search-result job, releasing SQL before returning pages. It must bound matches
examined, result bytes, job lifetime, and concurrent work. A changing FTS table is not
a stable cursor. Unknown binary/opaque content is searchable only through evidenced
metadata or an explicitly derived extractor with its own version and coverage.

## Retention, deletion, and garbage collection

Evidence and server-accepted input revisions are kept by default without automatic
pruning. Live-only timing remains evidence even when native history has final text.
There is no native watermark authorizing its deletion. Raw retention continues the
principle of [ADR-0005](adr/0005-raw-retention.md).

Compression, chunk packing, and archive compaction must prove exact reconstruction of
source bytes, source positions, clocks, and receipt identity before evicting an old
encoding. Projection caches, expired temporal versions, search jobs and obsolete
generations can be reclaimed because evidence can rebuild them. Current generation,
rebuild jobs, active snapshot leases and explicit export jobs pin required payload roots.

Explicit authorized deletion is a different operation. It commits a scope tombstone
before removing read access. A permanent deletion epoch/cut invalidates pre-deletion
evidence visibility and snapshots, while a separate suppression state blocks automatic
follower/reread re-ingestion for that scope. It cancels affected snapshots/exports and invalidates search; authorization and
tombstones are checked on every request even for an older generation. New explicit
re-adoption can clear future-ingestion suppression only as an intentional control action.
It starts a new admission epoch and verified source inventory; old tombstones and cancelled
snapshots remain invalid. Deduplication is scoped to that admission epoch, so an erased
old occurrence/receipt cannot count as newly recaptured bytes. Previously issued native
object references can resolve to new admitted evidence, never to deleted generations. This is not a
new deletion UI requirement for the foundation milestone; it is the minimum storage
boundary that prevents the archive from resurrecting deleted content.

Ingestion epochs also carry a monotone catalogue ordinal. Deleting an Agent records a
permanent cutoff over that ordinal, covering its associated launch inventories. A binding
learned after deletion still resolves an older inventory epoch to the deletion tombstone;
learning identity later cannot authorize the bytes again. Re-adoption creates an epoch
after the cutoff. This boundary is a durable order, not wall time or the set of bindings
that happened to be known during deletion. Visibility checks apply it immediately while
bounded background cleanup retires affected pins and jobs.

Suppression remains effective until explicit re-adoption. An inventory bound while its
Agent is suppressed receives a permanent deletion association. Re-adoption closes that
suppressed interval through the last previously issued epoch ordinal before creating the
fresh authorized epoch. Thus an inventory acquired during suppression cannot evade the
boundary by revealing its Agent identity only after re-adoption.

CAS garbage collection uses durable roots from evidence, pending uploads/outbox bodies,
active/building/leased generations, search checkpoints/result jobs and export jobs.
It marks candidates in bounded
batches, excludes concurrent reference publication during final eligibility check and
unlink, and retries after crashes. A shared payload survives until every live root
releases it. Source deletion is not enough to prove a CAS object is unreachable.

## Implementation sequence and falsification

Implement schema/types and capture admission first, then pure vendor mappings and shared
reduction, then snapshot reads/publication, then dependent adapters and UI integration.
Existing auth, spool, containment, CAS integrity and workload-isolation code can be reused
where its guarantees match. Existing event-phase promotion, content-digest suppression,
input-as-turn-text, turn-close sweeps and hot/cold semantic folding are removed.

The reference transaction exercise in
[storage-spike.py](measurements/unified-trace/storage-spike.py) checks the proposed
outbox/idempotence, temporal-page and publication cut mechanisms in disposable SQLite
databases. It is design evidence, not a replacement for real Rust crash, filesystem,
multi-process and performance tests. All production obligations are enumerated in
[unified-trace-conformance.md](unified-trace-conformance.md).
