# Database Schema

SQLite stores operational authority, immutable evidence, and independently rebuildable
query generations. [Unified Trace Storage](unified-trace-storage.md) owns their transaction,
publication, snapshot, and retention contracts. The initial schemas are
[control policy](../crates/server/src/db/schema.sql),
[command outbox](../crates/server/src/db/control/schema.sql),
[evidence](../crates/server/src/trace/evidence/schema.sql),
[publication catalogue](../crates/server/src/trace/evidence/catalogue/schema.sql), and
[semantic generations](../crates/server/src/trace/generation/schema.sql).

## Identity in the schema

A native Session has the composite key `(environment, agent, native_id)`. The public
encoded Session ID preserves that tuple. Trace AgentThread and object addresses use the
typed reference algebra in [the foundation](unified-trace-foundation.md#identity-and-reference-resolution).
An acquisition inventory can exist before a native identity is available. It is an
admission scope, not a Session row or a substitute AgentThread.

## Operational authority

`control.db` holds Subjects, environment installations and grants, native Session
metadata, current runtime requests and execution leases, durable command outcomes,
immutable command acceptances and the evidence outbox. It uses one FULL-synchronous WAL
writer and separate query-only readers.

`sessions` stores the identity triple, optional native workdir/title, creation-time
`inject_identity`, retained fidelity, chosen launch flags, current reachability-related
status, and creation/update timestamps. A row is inserted only after a vendor supplies
a real native identity. Deletion removes its metadata after committing trace suppression.
Changing an identity or resetting a session is not an operation.

`trace_commands` retains the original acceptance, the exact current immutable submitted
revision, dispatch attempt count and state, and a separately evidenced launch identity
binding. An edit is allowed only while a server-queued command has no dispatch permit;
it writes a new revision and `Revised` outbox occurrence atomically. Dispatch reads and
records the selected revision in its own transaction. Vendor queue acceptance does not
prove consumption. Each edit names its preceding immutable revision explicitly.

`current_requests` holds scoped vendor notices and their waiting/resolving/resolved
state. A resolution reserves the exact notice JSON in the same transaction as its
command dispatch permit. A changed notice or second resolver cannot obtain a permit.
Accepted or unconfirmed resolutions remain non-actionable across server/envagent
reconnects; only an explicit vendor refusal releases the claim. Public request IDs
include the hosted process incarnation, so a restarted vendor may reuse its native ID
without reviving an earlier request.

`control_outbox` retains each fixed observation identity and payload until evidence
returns a terminal retained or deletion-discarded receipt. Retrying export cannot
repeat a vendor effect. `deleted_commands` preserves erased command identities while
bounded deletion jobs remove exported input bytes. A deletion job has a fixed accepted
sequence cut, so subsequent explicit re-adoption cannot place fresh input in an old job.

## Subjects, environments, and grants

`subjects` stores authenticated principals and the server-administration flag.
`env_installations` stores paired environment identity, credential hash, connection
epoch, grant authority, native agent inventory and machine facts. `env_grants` binds
one Subject to an environment capability, issuer and optional expiry. `administer` is
not a grantable capability: the server administrator and machine authority have distinct
responsibilities defined in [environment authority](adr/0006-environment-authority-and-capability-grants.md).

Pairing and invite codes are single-use hashes with expirations. Unpairing revokes the
installation and its grants while retaining native Session and trace records. An
expired or revoked Subject grant is reevaluated on each request and dispatch; it never
changes the environment's cached facet state. Connection epochs fence messages from
superseded envagent sockets.

Search generations are disposable. Their documents retain the containing agent, exact
body revision and authorization support; no guessed original-owner field is stored.
History scopes use separate source membership rows instead of duplicating FTS text.
Body and membership completion are independently fenced before index publication.
Locally authored membership follows retained author/production evidence as specified in
`docs/unified-trace-storage.md` § Search consistency.

## Evidence and acquisition

`environment_admissions` indexes the environment and admission sequence at capture commit,
so full evidence exports do not scan unrelated observations. The export registry retains
fixed cuts, environment visibility revisions and expirations. Temporary Export payload
pins are released by completion or bounded expiry/startup recovery; each deletion updates
its environment visibility in the same transaction as the tombstone. Private cursor keys
and unadmitted staging are never archive rows.

`evidence.db` holds immutable source occurrences, observations, durable producer
receipts and contiguous prefixes, payload roots, admission epochs and permanent
deletion tombstones. Capture admission deduplicates producer delivery independently
from source-position rereads. Equal bytes at different source positions remain distinct.

Source inventory authorization precedes capture. Epoch ordering and evidenced
inventory-to-agent bindings make deletion effective even when native identity arrives
late. Re-adoption issues a fresh epoch; neither old spool frames nor old server outbox
occurrences can be retagged with it.

Payload addresses distinguish exact bytes from manifests while retaining one physical
BLAKE3 CAS. Capture-upload records retain byte-transfer receipts and reusable
source-occurrence proofs under `docs/unified-trace-storage.md` § Admission and acknowledgments.
These include attachment relations to already admitted sidecar occurrences; a global
digest alone supplies no ownership proof. Each observation indexes its own primary
payload reference, so alternate admitted manifest layouts remain eligible without
scanning receipt envelopes.

## Semantic generations and snapshots

Each generation owns typed claims, object declarations, temporal summaries, membership
and order revisions, exact payload anchors, mapper checkpoints and atomic change batches.
Summary revisions are separate from history membership revisions, so streaming text does
not multiply history-order rows. Indexed provenance comes from the protocol's typed
`Fact::subject_refs` and `Fact::kind` producers.

A reader lease fixes generation, evidence cut and view revision. Every pagination cursor
also binds its complete query scope. Queries fetch a bounded metadata candidate window
before validity filtering or payload access. Dense superseded membership rows return an
explicit advancing work-budget cursor, not a false complete empty page.

The evidence catalogue owns generation build jobs, immutable ready manifests, the active
pointer, reusable leases, scope pins and publication guards. Pointer publication is one
FULL transaction after the old and new projection writers are fenced. An unpublished
ready directory cannot become active merely because its mtime is newer. The product
holds an exclusive archive file lock before resetting interrupted publication or GC
guards. A missing or corrupt published projection cancels its old views and reports
recovery while a private full replay reaches the current durable evidence cut. Only
the catalogue pointer transaction makes that replacement readable. Visibility has its
own catalogue revision: deleting an epoch triggers a private replay even when the
evidence admission cut is unchanged. Publication removes suppressed directory and
search membership together; prior views continue to fail their deletion checks.

## Search and garbage collection

Search uses a separate generation-specific database, the linked jieba tokenizer, and
its own declared projection checkpoint. Result jobs retain a stable matching trace view.
Indexing lag and work limits are explicit. Search and export checkpoint roots retain
semantic revisions as well as payload bytes; the temporal reclamation floor includes
these roots and live reader leases. The search worker reserves a pending view before
reading its checkpoint and transfers that protection to its durable index root before
releasing the view. An old index is invalidated after its last result/reader lease
expires, then its revision and payload pins are released in bounded passes.

The physical root ledger includes authoritative observations, pending capture transfers,
derived uploads, active/building/leased generation roots, and search/export roots. GC
releases eligible roots and reserves physical digests in bounded passes. A durable
collector guard and per-digest deletion reservations prevent a stale collector from
unlinking newly republished bytes. Startup recovery retries interrupted reservations.

## Schema evolution

The project has one consumer and no compatibility schemas or incremental migrations.
Change the initial schema and its typed consumers together. Evidence is authoritative;
query databases are disposable and rebuild from retained evidence under named mapper
and reducer builds. Never discard authoritative evidence as a remedy for a query-schema
change.

SQLx executes queries at runtime. Store and API tests therefore validate SQL structure,
constraints, transaction boundaries and concrete returned values. `cargo doc` and the
document pointer checker are additional repository gates.
