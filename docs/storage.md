# Storage

[Unified Trace Storage and Read Protocol](unified-trace-storage.md) is authoritative
for the replacement storage architecture. [unified-trace-foundation.md](unified-trace-foundation.md)
owns semantics. Existing block-era code has not yet been replaced; its remaining
section references below route to the target contract rather than preserve conflicting
rules. Implementation sequencing belongs in [roadmap.md](roadmap.md#unified-trace-foundation-review).

## What "lossless" has to mean here

Evidence means exact captured bytes, occurrences and clocks, including live-only data.
See [the storage contract](unified-trace-storage.md#capture-contract).

## Deltas are not independent data

The former universal prefix-offset assumption is removed: streams may replace content.
See [the storage contract](unified-trace-storage.md#capture-contract).

## Workloads and database boundaries

Operational, evidence, trace and search writers have explicit durability and workload isolation.
See [the storage contract](unified-trace-storage.md#authority-and-database-boundaries).

## Consolidation

Compaction changes physical encoding or rebuildable projections, never semantic ownership or lifecycle.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

### Thresholds, and why these numbers

Compression and externalization thresholds are storage parameters, not semantic boundaries.
See [the storage contract](unified-trace-storage.md#sidecars-mutable-locators-and-large-records).

## Content addressing

BLAKE3 addresses exact decoded chunks; manifests and explicit encodings preserve representation and ranges.
See [the storage contract](unified-trace-storage.md#sidecars-mutable-locators-and-large-records).

## Blocks, and the three time axes

Typed objects replace consolidated blocks; recorded, monotonic and receipt clocks remain distinguishable.
See [the storage contract](unified-trace-storage.md#logical-tables-and-constraints).

### Native records are kept whole

Lexical records, source generations, sidecars and unavailable states survive semantic rebuilds.
See [the storage contract](unified-trace-storage.md#capture-contract).

## Replay

Receipt playback and semantic snapshot reading are separate bounded operations.
See [the storage contract](unified-trace-storage.md#read-protocol).

## Search

Search is a projection with an explicit generation, view revision and evidence checkpoint.
See [the storage contract](unified-trace-storage.md#search-consistency).

### The index is standalone, and that is forced

Search uses an independent bounded writer and cannot hold evidence admission behind tokenization.
See [the storage contract](unified-trace-storage.md#search-consistency).

### Tokenization

**The tokenizer is `jieba`, because `unicode61` cannot search Chinese.** `unicode61`
splits on non-alphanumerics, so an entire run of Han characters becomes one token and a
query for any substring of it matches nothing — which matters here, since this project's
own transcripts are substantially Chinese. `crates/fts5-jieba` segments CJK, folds
diacritics, and stems English, so one tokenizer serves both languages at no cost to
English capability.

**It is linked into the binary rather than loaded as an extension**, which is a
requirement. A loadable extension is a per-platform artifact that can be missing at
runtime, and an index built with one tokenizer and queried with another silently returns
nothing — so a fallback would answer *wrongly* rather than worse. Registration happens per
connection in `HistoryStore::open`, and a failure there is fatal.

**The dictionary is process-wide.** Building it costs about 600 ms, and FTS5 creates a
tokenizer per table per connection, so a per-instance dictionary would multiply that by
the pool size on each connection's first query. Sharing is sound because the dictionary is
immutable once built and tokenizing takes `&self`.

The index is tokenizer-specific, so changing the tokenizer requires reinserting the FTS
rows. A test for CJK search must fail against `unicode61` to be evidence about the index;
`chinese_text_is_searchable_by_word` in `crates/server/tests/storage.rs` is written that way and
was verified by reverting the tokenizer.

### A user's query is not an FTS5 query

**Quote the user's text into a phrase.** FTS5 reads punctuation as syntax, so
`MATCH 'needle-one'` parses as `needle` NOT `one` and answers `no such column: one` — an
ordinary search string producing an error about a column. Hyphens are the common case,
since identifiers and flags are exactly what someone searches a transcript for.

**Binding the value as a parameter is not sufficient.** Binding protects against SQL
injection, and the bound value still reaches FTS5's *expression* parser; a comment
asserting that a parameter is safe here describes the wrong hazard.

Quoting changes behaviour rather than only hardening it: a multi-word query becomes a
phrase match rather than an implicit AND, so `foo bar` finds "foo bar" and not a block
mentioning both apart. That is the safe direction — it never errors and never inverts a
term — and supporting operators means *parsing* the user's text rather than escaping it.
Keep a test whose query contains punctuation, since every single-word query works either
way.

## Retention is watermark-driven, never time-driven

This former native-watermark pruning rule is superseded. Evidence has no automatic deletion policy.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

### Two retention classes

Authoritative evidence and reconstructible projections are the retention distinction; native-backed/terminal status cannot erase unique data.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

### Only the environment holding a source can confirm one

Only the owning envagent can capture source availability. Availability is coverage evidence, not permission to prune.
See [the storage contract](unified-trace-storage.md#source-identity-and-generations).

### Deriving a bound from a vendor's store

No native-derived watermark authorizes deleting Sylph evidence, including unique stream timing.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

### Retention runs on a schedule, and reports what it did

Maintenance reports projection reclamation, pinned roots, candidate work and faults; it cannot silently prune evidence.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

### Growth is accounted in bytes, per session

Report retained/staged bytes, projection/search growth, pins and worker lag separately from unavailable source sizes.
See [the storage contract](unified-trace-storage.md#retention-deletion-and-garbage-collection).

## Execution node projection

AgentThreads, Operations and Resources replace promoted execution-node containers.
See [the storage contract](unified-trace-storage.md#logical-tables-and-constraints).

## Conversation reading projection

Reader summaries pin history membership, order and immutable body references at a leased revision.
See [the storage contract](unified-trace-storage.md#snapshot-pages-and-live-changes).
