# Storage

[Unified Trace Storage and Read Protocol](unified-trace-storage.md) owns durable
capture, reduction, publication, snapshots, bounded reads and garbage collection.
[Database Schema](database-schema.md) describes the implemented database families.
There is one authoritative evidence archive; native history availability cannot authorize
removing unique live bytes, clocks, or raw fields.

## Workloads and database boundaries

Operational policy and immutable input are in `control.db`; capture admission and the
publication catalogue are in `evidence.db`. Semantic and search generations use separate
writers. Payload I/O, JSON decoding, tokenization and projection work do not run inside
an evidence admission transaction. All acknowledged evidence and command acceptance
use FULL-synchronous WAL commits.

## Content addressing

A typed `bytes:<digest>` or `manifest:<digest>` address names a logical payload. Its
physical digest is BLAKE3 over exact CAS bytes. A manifest can share physical storage with
a raw captured byte string without becoming the same logical object. Manifests retain
ordered chunk mappings, encoding, media type, total length when known, and available ranges.

Capture uploads reserve ownership before CAS publication. Admission requires each new
observation's byte receipts or a proven reread of the same source occurrence in the same
epoch. Unknown content and incomplete sidecars remain explicit evidence states.

## Search

Search is available only at its declared generation and projection checkpoint. Stable
result jobs pin the corresponding view. An indexing or work-budget response does not
silently return another generation's results.

### Tokenization

The SQLite FTS5 tokenizer is jieba, linked into the server through
[fts5-jieba](../crates/fts5-jieba/). Registration occurs per search connection through the
[SQLite pool helper](../crates/server/src/db/sqlite.rs). Registration failure is fatal;
falling back to another tokenizer would make existing indexes answer incorrectly.
The immutable dictionary is shared across tokenizer instances.

### A user's query is not an FTS5 query

Search operators, punctuation handling and query bounds belong to the search API.
SQL parameter binding prevents SQL injection but does not escape FTS expression syntax.
The search store validates its declared query syntax and reports malformed queries rather
than silently changing what a caller requested.

## Retention and explicit deletion

Evidence is retained by default. Runtime source coverage, EOF, compaction or a native
watermark cannot erase unique observed data. Rebuildable query rows and expired leases
can be collected independently.

Explicit deletion commits permanent scope tombstones and future-ingestion suppression
before removing access. The server cancels affected reads/jobs, exports terminal discard
receipts for old spool/outbox work, and removes input bytes through bounded deletion jobs.
Re-adoption is an explicit new admission epoch. Old snapshots and observations stay deleted.

CAS collection follows the durable physical root ledger, including active transfers and
leased generations. It reserves deletion before unlink, synchronizes filesystem changes,
and retries interrupted reservations. Shared bytes survive until their final live root
is released.
