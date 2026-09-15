# Database Schema

[unified-trace-storage.md](unified-trace-storage.md#logical-tables-and-constraints)
owns the replacement table families, keys, transactions and indexes. Operational Session
and authorization schema belongs to the control store; trace objects belong to versioned
projections over immutable evidence. The initial schema changes in place, with no migration
or legacy reader. Current DDL still describes the previous implementation; the gap lives
in [roadmap.md](roadmap.md#unified-trace-foundation-review).

## Identity in the schema

Public Session identity is the native triple. Trace AgentRefs additionally address real
embedded child identities without minting a public Session. Object keys retain native
namespace or deterministic source occurrence scope, and equivalence is proved rather
than inferred. The complete key algebra is
[unified-trace-foundation.md](unified-trace-foundation.md#identity-and-reference-resolution).

Same-database constraints enforce resolved relationships. Cross-database references
use typed manifests, not foreign keys SQLite cannot enforce. See
[unified-trace-storage.md](unified-trace-storage.md#logical-tables-and-constraints).

## sessions

The control store holds native identity, working-directory provenance, title, identity
injection and reachability/policy metadata. A session has no creator-based class and
cannot change native identity. `inject_identity` stays immutable after creation.
AgentThread execution, coverage and history selection are trace facts, not overloaded
session row state. The store has no event-count-derived identity allocator.

[architecture.md](architecture.md) owns public identity and policy;
[unified-trace-foundation.md](unified-trace-foundation.md) owns embedded agents and trace
relationships. Spawn, fork, context lineage and current selected head are separate.

## turns

The former row combining queued request text and execution outcome is replaced by
immutable command/message revisions, Delivery attempts and native Activation objects.
Queued/starting/stopping control state does not fabricate an execution boundary.
See [unified-trace-foundation.md](unified-trace-foundation.md#messages-deliveries-and-model-visibility)
and [the operational outbox](unified-trace-storage.md#operational-facts-use-an-outbox).

### A terminal status means the record is finished

This former guarantee is superseded. A terminal Activation describes the execution's own
outcome. Late history, child results and sidecars may still arrive. Evidence admission,
projection/search progress and observation coverage explicitly describe record completeness.
See [unified-trace-foundation.md](unified-trace-foundation.md#independent-lifecycle-claims).

## events

The kind/phase/span journal is replaced by immutable capture occurrences and typed
semantic claims. Producer receipt, source position, archive admission and query change
revision are separate axes. Sealed message content, operation return and child completion
cannot share one phase column. Table constraints and typed missing/conflicting references
are specified in [unified-trace-storage.md](unified-trace-storage.md#logical-tables-and-constraints).

## pending_requests

What an agent is blocked on, waiting for a human. A resource rather than a stream message,
because the agent waits until someone answers: a client closing its laptop must not strand
it, and the answer has to be possible from a frontend that was not attached when the
request was raised.

A row carries the request's kind (`approval` | `dialog` | `elicitation` | `choice`), the
tool and tool-use id it concerns, the declared options and form, a default, a risk marker,
an expiry, and its resolution. A partial index covers open rows only, since resolved rows
accumulate without bound and never appear in the hot "what is this session waiting on"
query.

`adapter_request_id` is the adapter's own id, scoped to one link and never serialized to a
client — a client-supplied adapter id would be a way to answer a request that was never
raised. Clients address a request by `id`.

**`status = 'pending'` in the resolve UPDATE's WHERE clause is the entire concurrency
control.** Two clients answering at once both run it, SQLite serializes them, and exactly
one gets a row back. A restart marks every still-pending row `abandoned` rather than
leaving it answerable, because no link survives a restart — and the row is kept, so the
timeline can explain why a turn stopped instead of showing a gap.

`form_json` is the input the request declared it wants collected, and absent is meaningful
rather than missing: a request with no form is an approval, whose subject is the one
proposed tool input (`docs/session-protocol.md` § Request forms).

## Subjects, environments, and grants

`subjects` holds one row per authenticated actor
([ADR-0002](adr/0002-environment-workspace-subject.md)). The bootstrap seeds one explicit
development row, while the login and registration routes add registered Subjects alongside it.

`is_admin` is the server administration authority — the party ADR-0006 assigns as the
authority root for every `grant_authority = server` environment. It is a column here rather
than a grant row for two independent reasons: grants are keyed per environment and this
authority is not, and a grantable `administer` would restore the escalation chain ADR-0006
removes. Nothing sets it over the wire, so an admin cannot mint another admin. It confers no
`observe` or `control` on any environment — an admin who wants to read a session grants
themselves one, which leaves a grant row, since bundling reads into the administration
authority would make the audit surface the largest unlogged reader.

**Environment installations and pairings** are server-owned. Pairing codes are stored as
hashes and consumed atomically; the bearer token itself is never written to the journal. A
reconnect is accepted only for the current connection epoch. An installation carries
`grant_authority`, `machine` or `server`, derived from pairing provenance and immutable
thereafter — it records where authorization legitimately originates, and a value that could
be re-pointed later would record nothing.

Unpairing removes the installation and its lease **while leaving every recorded session
readable** (`docs/environments.md` § Unpairing is the terminal operation).

**Grants** are one row per `(subject, environment)` with a capability of `observe` or
`control`, plus the provenance of the local signing act that issued it, its expiry, and its
consumption state. `administer` never appears, because a row asserting it would be a grant
of the power to grant.

Expiry is what makes the link's `expired` distinguishable from `not_granted`: an expired
row is still present and still proves the grant was once legitimate, so the remedy is a
renewal rather than a first issuance. Revocation deletes the row, and a deleted row cannot
be distinguished from one never issued — which is why "revoked" survives on the turn it cut
short, as `close_reason = grant_revoked`, rather than in the grant table.

Invite codes share the pairing code's hashing and atomic consumption, and are validated on
five properties: environment scope, capability, expiry, single consumption, and issuer
provenance. Scope is stored rather than inferred from the redeeming connection, so a grant
asserts only over the environment that issued it.

**Evidence receipts** deduplicate the complete producer/epoch/sequence identity and
commit with evidence admission, before semantic mapping. Source occurrence and retention
authorization epoch are independent keys. The replacement transaction and collision
rules are [the evidence admission contract](unified-trace-storage.md#admission-and-acknowledgments).

## Search lives beside history.db

Search moves to its own generation/checkpoint store so tokenization cannot delay evidence
acknowledgments. It reports coverage and lag, applies current grants, and pins corresponding
trace membership/body revisions. [unified-trace-storage.md](unified-trace-storage.md#search-consistency)
owns this contract. Native stores are read only by their capture adapter; directory scopes
remain canonical exact path/containment matches, never substring matches.

## Schema evolution

Each replacement database has a single initial schema applied at open. Runtime checks
validate its schema/build rather than accumulating a migration chain. The exact target
layout is in [unified-trace-storage.md](unified-trace-storage.md#authority-and-database-boundaries).

**Schemas change in place.** The store is disposable: a build that cannot read one refuses
to start and names it, and the answer is to discard `data/` and re-adopt from the vendors'
own native stores. So the chain stays collapsed rather than growing. `sqlx` checksums each
file, so editing one a running database has already applied makes that database refuse to
open — that failure is the design. It is loud, it lands while someone is looking, and it
costs one `rm -rf ~/.sylph/data`. A chain is what you would keep instead to make an edit
survivable, and nothing here needs it: preserving the order a schema was discovered in is
only worth the second reading path while some database is stranded partway along it.

**A collapsed migration does not use `IF NOT EXISTS`.** In an appended chain it is
harmless, because the object it guards genuinely may exist from an earlier member. In a
single file that only ever runs against a fresh database, the guard can only turn "this
table already exists, so something is wrong" into a silent success over a schema that may
be missing every column added since — which is the class of defect `AGENTS.md` § Where
Design Rules Live opens with, a plausible result in place of an error.

Attachments use immutable payload references; accepted command revisions and evidence
export use the operational outbox. A turn row is not their shared container.

**`/api/v1/health` reports `MAX(version)` from `_sqlx_migrations`, never a stored key**, and
the test reads the highest filename prefix out of the journal's migration directory rather
than pinning a literal. A number the runner already maintains cannot have a second copy,
since nothing would compare the two. It reads the prefix and not a count of the files
because `MAX(version)` is what the runner records: the two agree only while the files are
numbered contiguously from 1, so a count would survive today and report a version nobody
assigned the moment a second file appeared out of order.

There are no `sqlx::query!` macros here, only runtime `sqlx::query`, so there is no
`DATABASE_URL` and no `.sqlx` offline directory. A schema mistake surfaces in the store
tests rather than at compile time, which is why those tests matter.

## Execution nodes

Promoted block-derived execution nodes are replaced by independently typed agents,
activations, operations and resources. Query projections index those objects without
becoming their identities. See [unified-trace-storage.md](unified-trace-storage.md#logical-tables-and-constraints).

## Conversation reading addresses

Every issued semantic reference remains resolvable across verified identity reconciliation.
Temporal summaries pin order, membership and immutable payload revisions under a reusable
view lease. [unified-trace-storage.md](unified-trace-storage.md#snapshot-pages-and-live-changes)
owns the address and cursor contract.
