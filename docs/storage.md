# Storage

How the record is kept so that it is lossless, replayable from any frontend, and pruned
only when a downstream authority has confirmed the data. The implementation is
`crates/server/src/storage/`, and the tables are in `crates/server/migrations/history/`.

**This file owns `history.db`, `native.db`, and `blobs/`.** `docs/database-schema.md` owns `journal.db`.

## What "lossless" has to mean here

A turn's event stream is a log of text *arriving*: a hundred `delta` events, each a few
tokens, then one `completed` carrying the whole block. Keeping only the `completed` events
re-renders the timeline and loses the replay — what a client actually saw.

Replay is the requirement that forces this design. A frontend that can only see final
state cannot offer "open this conversation on my phone and watch what happened", and
every future frontend inherits that limit. So losslessness is stated precisely, because
the naive reading is ruinously expensive and the precise one is nearly free:

> Every event a client could have observed is reconstructible byte-for-byte,
> with its arrival time.

## Deltas are not independent data

Storing each delta as a row costs about 20× the block it belongs to. A 2 KB assistant
message arrives as ~400 deltas; as rows that is ~24 KB of mostly duplicated text, since
**every delta is a prefix-suffix of the same final string**.

Storing the final text once plus the *boundaries* keeps everything:

```
block text:  "I'll start by reading the config file."
deltas:      ▲    ▲      ▲  ▲       ▲   ▲      ▲    ▲
offsets:     [6, 11, 18, 21, 29, 33, 40, 46, ...]
timings:     [0, 34, 51, 12, 40, 28, 33, 19, ...]   ms since block start
```

Two `u32`s per delta — 8 bytes against ~60 for a row — and the reconstruction is exact:
delta *n* is `text[offsets[n-1]..offsets[n]]`, arriving at
`block_start + cumsum(timings)[n]`. For that message, 3.2 KB instead of 24 KB, with
nothing approximated.

This is the same move as a keyframe plus its deltas, or a git packfile: store the resolved
state, plus the minimal instructions for how it came to be. It is what makes lossless
replay affordable rather than a thing to be traded away.

Offsets are byte offsets into UTF-8, not character indices, because that is what a
reconstructor slices. A delta boundary can land mid-codepoint when a model emits a
multi-byte character in pieces, so a replayer buffers an incomplete tail rather than
assuming every boundary is a valid string edge.

## Workloads and database boundaries

| Workload | Store | Write scheduling |
| --- | --- | --- |
| Session control and live events | `journal.db` | One writer, independent readers |
| Lossless native records and staged sidecars | `native.db` | One writer, independent readers |
| Consolidated blocks and search | `history.db` | One writer, independent readers; bounded batches |
| Large immutable bodies | `blobs/` | Publish before committing a reference |

The tiers describe record temperature; database files isolate write workloads. Native
retention is on the live acknowledgement path and must not share SQLite's writer lock
with tokenization, consolidation, or pruning. History keeps blocks and their standalone
FTS index in one transaction so a committed block and its search entry cannot disagree.
Per-session databases would multiply pools and require cross-file search fan-out without
removing the need to bound individual writes, so the split follows workloads instead.

Every database uses WAL. A single writer connection serializes writes before they reach
SQLite; separate read-only connections keep a queued writer from consuming all read
capacity. Read-then-write transactions acquire write intent with `BEGIN IMMEDIATE`,
before taking a snapshot. A busy timeout handles brief external contention; it cannot
repair an obsolete read snapshot and is not a substitute for bounded work.

History publication and pruning release the writer between batches bounded by block
count and, for publication, decoded payload bytes. One oversized block is indivisible.
Preparing compressed bodies and publishing CAS objects happens before the SQL transaction.
A partial publication leaves the journal intact; replaying the publication upserts by
`(session_id, seq)` before the journal is deleted. Readers materialize query results before
resolving bodies or pacing replay, so no SQL cursor is held across network or filesystem I/O.

```
data_dir/
  journal.db      hot   sessions, control, unconsolidated events
  native.db       raw   lexical vendor records and native sidecar references
  history.db      warm  consolidated blocks and atomic search index
  blobs/          cold  content-addressed bodies, sharded aa/bb/<hash>
```

`journal.db` runs `synchronous = NORMAL`; `native.db` and `history.db` run `FULL`.
Raw bytes commit before a producer receipt, and history commits before the journal forgets.
No cross-database write transaction or acknowledgement of an unfinished record is needed.
Read-only history connections attach the native database for accounting and reachability;
writer connections never attach it, so a history transaction cannot reserve its write lock.
Accounting across the files is a local observation, not an atomic cross-file snapshot.

Deltas are batched into one transaction per ~50 ms. This is the largest lever on the hot
path — fsync count is the cost, and batching converts thousands of tiny transactions into
tens. The window is a latency/loss tradeoff rather than a throughput knob: 50 ms is below
human perception for stream smoothness and bounds loss to one window.

**Only ephemeral events are batched, and the two kinds already have different replay
paths.** A stable event is journaled *before* it is published, so a client reconnecting
immediately after seeing it always finds it in replay; `EventHub` keeps a bounded
in-memory ring of ephemeral events, which is what a mid-stream reconnect replays from.
Batching a stable event would open a window where a reconnect sees a hole, while batching
an ephemeral one costs nothing observable. Deltas are thousands per turn against tens of
stable events, so that captures nearly all the win and needs no flush-on-read machinery.

The window has one ordering rule: **a turn's buffer is flushed before consolidation folds
it.** The fold reads journal rows and then deletes them, so a window still in memory would
be omitted from the block and the rows that would have fixed it destroyed. The settle path
awaits the flush.

## Consolidation

Stream metadata is indexed once per fold. Looking up a stream's first event, last
recorded timestamp, or terminal sequence cannot rescan the turn for each output block.
The CPU fold runs on a blocking worker under the existing consolidation gate, so a
large archive cannot occupy the async executor that dispatches sync and file commands.
History publication still commits before journal eviction; offloading computation does
not change producer order, receipt ownership, or the retained record.

When a turn reaches a terminal state, its rows move from `journal.db` to `history.db`,
folding as they go:

1. Group journal rows by `stream_id` into blocks.
2. For each block, keep the `completed` text; encode its deltas as offset and timing
   arrays.
3. Externalize any payload of 4 KB or more to `blobs/`, leaving a hash reference.
4. Compress remaining inline text of 512 bytes or more with zstd, keeping the compressed
   form only if it is smaller. Encoding is recorded per row rather than inferred from
   length, so incompressible text (base64, random ids) stays `raw` instead of growing by a
   frame header and costing a decode on every read.
5. Insert the blocks into `history.db`, then delete only the journal row identities and
   versions captured by that archive snapshot. Later appends and in-place corrections
   remain journaled for the next pass. Cleanup commits as one journal transaction;
   a failed delete cannot leave a partial stream for a retry to fold over the archive.
   Whole-turn transcript reads merge the archived prefix and any journal tail, preferring
   hot versions when publication temporarily leaves the same sequence in both tiers.

**Step 5's order is the correctness rule:** history is durable before the journal forgets.
A crash between them leaves a turn present in both, which consolidation makes idempotent
by keying on `(session_id, seq)`. A crash the other way loses the turn.

The journal itself is the durable consolidation queue. Scheduling under load coalesces
wakeups instead of accumulating a suspended task per turn. A full sweep batch schedules
the next bounded pass immediately, so a backlog does not wait for the maintenance timer.

Consolidation is also the recovery path: a turn found in `journal.db` at startup whose
status is terminal was interrupted mid-consolidation, and is consolidated again.

**An open delegated trajectory is preserved as open.** Step 2 keys a block on its
`completed` event, so a block for a still-running trajectory is written from its declaring
phase instead, keeping `phase: "started"`. A client reads a trajectory's status off its
phase, so that says *running*, which is what the work is — consolidation is not entitled
to invent an outcome nobody reported.

That path is ordinary rather than exceptional: the turn-settle sweep deliberately skips
work an adapter declares backgrounded (`docs/session-protocol.md` § Delegated
trajectories), so a delegation outliving its turn reaches consolidation open by design.

`started` is a true statement only while the process that would report the outcome is
alive, and no link survives a restart. So startup closes what history still holds as
running, as `abandoned` — the same repair the journal's own startup sweep performs, one
tier down. It stays correctable, because a server-authored close is precisely what a later
real outcome may overwrite.

### Thresholds, and why these numbers

**4 KB for externalization.** Below a filesystem block, a separate file wastes the rest of
the block and costs an inode. Above it, an inline blob inflates every row scan that does
not need the payload — and timeline reads usually don't, since a 200 KB file read renders
as a collapsed summary until expanded.

**512 bytes for compression.** zstd on shorter strings loses to its own frame overhead.
Above it, agent output compresses 3–5×, because tool output, code, and diffs are extremely
redundant.

Both are recorded in the row rather than assumed globally, so a stored row remains
readable after a threshold changes.

## Content addressing

Blobs are keyed by `blake3(content)`, sharded two levels (`blobs/a1/b2/<hash>`) to keep
directory sizes sane, and refcounted through a `cas_refs` table keyed by
`(hash, session_id, seq)` — reachability spans sessions, so the sweep is a global scan.

Dedup here is large rather than incidental: an agent reads the same file across many turns
of a session, and the same dependency source across many sessions. Storing that content
once is a property of addressing by hash, not an optimization layered on top.

blake3 rather than SHA-256 for throughput on the hot path, and because collision
resistance is the only cryptographic property needed — this is a content index, not a
trust boundary. Content is never executed or trusted on the basis of its hash.

Publication syncs the object bytes before renaming and syncs the containing directory
chain before returning. Both reads and deduplicated writes verify the content hash;
an existing filename alone is not evidence that its object is intact.

## Blocks, and the three time axes

A block is one consolidated event: its `completed` sequence number, turn, kind, phase,
`stream_id`, `span`, and name; its text either inline (possibly zstd) or as a CAS hash,
with the uncompressed length always stored; its packed delta offsets and timings; and its
JSON payload under the same inline-or-hash rule. `(session_id, seq)` is unique and indexed. An internal integer primary key is also the
FTS rowid, so index replacement and deletion seek directly instead of scanning unindexed
FTS metadata. This storage address is not a session identity.

`session_id` is the single-column encoded triple here rather than three columns, because a
foreign key cannot cross the database split (`docs/database-schema.md` § Identity in the
schema).

The uncompressed length is stored even when the text is externalized, so a timeline can
lay out and truncate without touching CAS. Fetching a 200 KB blob to learn it is large is
the read amplification that makes a list view slow.

Timeline reads use persisted bounded previews of text and metadata, with the original
byte lengths and a body reference when either preview is incomplete. Preview queries do
not select compressed bodies or read CAS. Full body and replay reads resolve and verify
the original payload; a corrupt or absent body is an explicit read error. A preview is a
projection for scanning, never a substitute used by reconstruction or deduplication.

Text and JSON metadata each record their own encoding and uncompressed length. Their
sizes are independent: a short tool-result label can accompany compressed or externalized
metadata. Decoding follows those recorded encodings, and corrupt metadata fails the read
rather than disappearing from an otherwise plausible block.

`seq` is the sole replay and ordering axis. Clock domains remain explicit:

| Field | Whose clock | What it means |
| --- | --- | --- |
| `arrival_at_ms` | the server's | when the event was received |
| `recorded_at_ms` | the vendor's | when the vendor says it happened, when it says |
| `started_arrival_at_ms` | the server's | receipt of the block's opening phase |
| `started_recorded_at_ms` | the vendor's | the recorded opening, absent when unknown |

Replay timing uses receipt timestamps. A native duration requires both recorded endpoints;
neither missing endpoint is filled from the server clock. A stream duration uses both
arrival endpoints. There is no combined effective event timestamp.

### Native records are kept whole

Ingest persists each vendor record before projecting later messages: the parsed JSON, and
for JSONL sources the original line bytes. The parsed value drives projection; the raw
bytes preserve lexical detail a JSON value cannot represent — key order, duplicate keys,
whitespace, escape spelling. The owning envagent resolves sidecar locators and transfers
their bytes in bounded, durable producer chunks before the referencing native record.
The server stages each chunk before acknowledging it and retains the complete body in CAS
before acknowledging the record. A server never opens an envagent's source path.
Sidecar bodies stay outside timeline payloads; retaining one does not require a client to
open it before the vendor removes its copy. Missing, unreadable, or oversized sources
remain explicit errors on the retained reference.

Native records are a separate table from projected blocks for two reasons: re-projection
needs fields the current adapter ignores, and raw records are never safe to prune against
an event watermark.

Each retained record has a durable first-seen sequence within its session. Replay of the
native bundle follows that sequence, including when records from several sources
interleave. A revision is an opaque unsigned 64-bit identity, never an ordering key; its
SQLite integer representation preserves all bits. Repeated observations keep the original
sequence. Sidecar availability is an observation separate from the raw identity: a later
successful read fills a previously unavailable body, and disappearance at the vendor
never withdraws bytes already retained. Two different available bodies for the same raw
reference remain a collision.

Raw retention is an ingestion prerequisite rather than best-effort telemetry. A write is
retried once for SQLite contention; a collision or corrupt body is not retryable; if it still fails, adopt returns an
explicit error and a live link closes with `LINK_LOST`. Normalized events are never kept
without their source record, and the failure counter is diagnostic only.

## Replay

Replay belongs to the server, not to each frontend. A client asks for a session's stream
and receives events in original order with original relative timing:

```
GET /api/v1/sessions/{id}/replay?from_seq=0&speed=1
```

**Both parameters are optional and neither default is the value shown above.** `from_seq`
defaults to `-1` (from the beginning) and `speed` to `0.0` (emit everything immediately,
unpaced), so a bare `GET …/replay` is the timeline-render case, which is what almost every
caller wants. Pass `speed=1` explicitly for original-timing playback.

Paced replay clamps any inter-event sleep to 2 seconds. A user who steps away mid-session
leaves a minutes-long hole, and reproducing it would hold a connection open doing nothing
— the pacing conveys how the work unfolded, not the coffee break.

The server merges `history.db` blocks with any live `journal.db` tail, expands delta arrays
back into individual events, resolves CAS references, and emits the same SSE frames the
original stream carried.

Doing this server-side is what makes "one authoritative record, many frontends" true
rather than aspirational. Reconstruction from folded blocks is subtle — UTF-8 boundaries,
interleaving by `seq` across two databases, timing accumulation — and a rule implemented
once per frontend is a rule that will differ per frontend. Discord, a terminal, and the
browser would then disagree about what happened, which is what the single record prevents.

## Search

Cross-session search is one FTS5 table beside `history.db`, written by consolidation in
the same transaction as the blocks it indexes, carrying the block text plus `session_id`
and `seq` `UNINDEXED` for result metadata. Mutations address the FTS row by the block
integer primary key; unindexed metadata is never a deletion predicate.

Interactive session search also includes current journal bodies, assembled through the
same body resolver as expansion. An explicit query builds a private jieba FTS index over
nodes with hot fragments, so phrases can cross delta and archive boundaries without
rewriting a whole-stream index for every token. It reads the journal snapshot before
history, deduplicates by logical node, and excludes superseded body revisions before
applying the result limit. Cold bodies use the persistent FTS index. Search does not poll
or scan previews in the browser. Hot search work scales with current body bytes; it is
not covered by the constant-work node-page measurements.

Coverage is therefore statable as a contract: **everything the server holds is
searchable**, including sessions parsed out of a vendor's native files, whether or not the
environment that produced them is still reachable.

### The index is standalone, and that is forced

External content is the obvious FTS5 shape and the wrong one here. It re-reads the column
from the base table at query time, and a block's inline text is not reliably text: the
encoding may be `zstd`, and an externalized payload leaves the column `NULL` with the
bytes under `blobs/`. Such an index would hold compressed bytes for some rows and nothing
for the rows most worth finding — the large ones. Consolidation therefore inserts the
*decoded* text, which costs a second copy and is the price of the tier split.

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

A time-based rule deletes data because a clock advanced, which is exactly the case where
the data may not exist anywhere else yet. So the only question retention asks is:

> What sequence number has a downstream authority confirmed for this session?

Pruning then deletes whole rows at or below that watermark. This is the discipline behind
a Postgres replication slot's `restart_lsn` and a Kafka consumer offset: the upstream keeps
what any consumer might still need, so a lagging consumer causes *growth* rather than loss.
Growth is visible and fixable.

The watermark is the **minimum** across every consumer that must see the data. A maximum,
or any single consumer's position, deletes data a slower consumer has not read.

Pruning is not lossless against the local store, which is the entire reason a watermark
gates it: what makes the deletion safe is a confirmation that the vendor's own store still
holds those records. Intra-message delta timing is the one thing no native store holds, so
that is destroyed outright.

### Two retention classes

The only downstream copy of a session is the **vendor's own native store**, so the only
question retention can ask is whether that store still exists and still holds the session.
There is no second Sylph-side copy to wait for (`docs/architecture.md` § One authority).

| Class | When | Rule |
| --- | --- | --- |
| **Backed** | the vendor's native store still holds this session, on a reachable environment | prune below the confirmed bound |
| **Terminal** | the server has established that no native source exists (for example, the vendor never wrote the session) | compact, never prune |

A terminal session's record exists *only* here, so there is nothing to wait for and
pruning would destroy the sole copy. It is still compacted — deltas folded, blobs
externalized, text compressed — and never deleted.

The boundary rests on the session's own environment id and its current lease, never on who
started the session. A paired but disconnected environment is **unreachable for this pass**:
retention records no watermark and keeps the previous one untouched, so a transient outage
cannot become a false confirmation. An unpaired environment is also unreachable; it stays
withheld until an operator explicitly establishes that no native source remains
(`docs/environments.md` § Unpairing).

**There is no size trigger, and none is coming.** A terminal session has no downstream
copy by definition, so a size-triggered prune is a size-triggered deletion of the only
copy — the same trade a time-based rule makes. What an operator does about a session that
is too large is delete that session, or pair the environment holding its native source so
it stops being terminal. Both are decisions with an owner, and the reporting below is what
puts the decision in front of one.

### Only the environment holding a source can confirm one

A confirmation is a statement about a native source, so the envagent answers it, because
the file is on its disk. A confirmation carries three things: how far a downstream copy is
confirmed to extend, what the environment reported about the native source
(`present` | `absent` | `unreachable`), and — when nothing was confirmed — why.

**"Cannot ask" is never recorded as "asked, and nothing is safe".** A watermark row is a
claim that a check happened; an unreachable environment means the question was never put,
so a zero watermark there would let a later pass believe a bound was established. The
withheld reason is what keeps those distinct.

A session with no native source is **terminal rather than unconfirmed**, and the type
enforces it: an authority that needs a native source in order to be constructed cannot be
asked about a session that has none. Reachability is resolved against the opaque environment
id in the session identity; a global "paired" health bit cannot answer for multiple
installations.

### Deriving a bound from a vendor's store

An environment cannot report "I have ingested through event 400", because a vendor CLI has
no such notion. What it can report is what its own store holds, and a bound is derived
from two facts rather than assumed.

**A native store is written whole-message.** Each of the three supported agents appends
complete records — `claude` at content-block boundaries, `codex` at item completion, `pi`
at `message_end` (`docs/agents.md`) — so the last record in the file is complete, and
everything before it is too.

**A record's identity is the vendor's own.** The bound is expressed as the last native
record the environment can see, translated by the adapter into the sequence number of the
Sylph event it produced. The translation belongs to the adapter for the same reason
parsing does: only it knows the vendor's record layout.

Two rules make a derived bound safe:

* **Bound it by the session's own end**, not by wall-clock recency. A store flushed
  mid-turn is at message granularity, so its last record may lag the live stream by a whole
  tool call.
* **Withhold on any ambiguity.** A last record that does not map cleanly onto an event, a
  file truncated mid-write (a real `pi` hazard, `docs/agents.md`), or a session the store
  does not contain yields a withheld reason rather than a zero bound.

### Retention runs on a schedule, and reports what it did

A pass runs at startup and every five minutes over a bounded batch of sessions that hold
blocks, ordered least-recently-checked first, so successive passes rotate through all of
them rather than re-examining one prefix.

**A subsystem whose correct behaviour is usually "nothing happened" needs a report.**
Keeping every block is right for a terminal session and a symptom for a backed one, and
the observable state — no blocks pruned — is identical in both cases and identical to
never having run. So:

* **A watermark row is written even when nothing is confirmed**, which is what separates
  "checked, and nothing is safe yet" from "never checked". An empty watermark table looks
  exactly like a correctly conservative one.
* **An unreachable environment is one fact about the pass, not N session failures.** Each
  paired environment is snapshotted once per pass by its opaque id. Disconnected or absent
  ids leave their sessions alone: not pruned and **not recorded as checked**. Report an absent
  peer once at `error` and at `debug` thereafter, in whatever holds the connection — a
  timer-driven caller otherwise reproduces the same unbounded stream from one missing
  dependency. **An install with no environment paired is a supported configuration**, so it
  must not be reported as a fault forever.
* **Blob sweeping is per pass, not per session**, since reachability spans sessions.
  Block deletion and body replacement enqueue old hashes transactionally. The sweep
  consumes this bounded queue, seeks native references by hash, and excludes concurrent
  publication while rechecking and deleting each candidate. An empty queue does not scan
  live references. Failed filesystem deletion leaves the candidate available for retry.

`/api/v1/diagnostics` reports the last pass separately as examined, confirmed, withheld,
terminal, unchanged, blocks pruned, blobs swept, and which environments were reachable. A
single `pruned: 0` answers none of the questions that matter.

### Growth is accounted in bytes, per session

Those counts answer *was anything dropped*. They cannot answer the question an operator has
when a disk fills: **which session is responsible.** A terminal session keeping every
block is correct behaviour, so `withheld: 41` is both the right report and silent about the
one sandbox session that has reached a gigabyte while forty others sit at kilobytes.
Counting blocks cannot separate them either — one 200 KB tool result and one 20-byte status
are both one block.

So each pass measures the sessions it examined and records bytes, block count, and the
last *differing* observation per session. `/diagnostics` then reports `bytesHeld`,
`sessionsMeasured`, `bytesGrewBy`, and a bounded `largestSessions` list, each row carrying
its own `grewBy` and `grewOverMs`. The pass logs one line naming the single largest holder
when anything grew — one line, because growth every pass is a terminal session's expected
state.

Five decisions in that shape are load-bearing.

**A byte is the logical record, not the footprint on disk.** The stored length is
uncompressed even for an externalized payload, so this measures what the record contains.
Stored size was rejected on three counts: compression ratio varies 1–5× with content, so
ranking by it ranks partly by how compressible a session's tool output happened to be;
measuring it means a `stat` per blob per pass, or a size column to keep correct through
pruning and sweeping; and a blob shared by two sessions is one file with no well-defined
per-session share. The consequence to hold onto: **the sum over sessions exceeds the store
on disk.** This measure answers "which session is growing"; `du` answers what the disk is
doing.

**Growth needs a stored earlier observation.** Each database maintains current logical
bytes in a per-session counter updated by triggers in the same transaction as its rows.
An unchanged duplicate changes no counter; replacement and pruning apply exact deltas.
Accounting reads these small counters instead of rescanning every native payload.
A difference does, and without one the only available answer is "large" — which does not
distinguish a session that reached its size months ago from one adding a hundred megabytes
an hour. Those call for opposite responses.

**The baseline moves only on a differing measurement.** Re-recording an unchanged reading
would set the previous value equal to the current one and report a growing session as
stable the moment two passes happen to agree.

**`grewBy` is absent on a first measurement, and signed thereafter.** One observation is
not a trend, and reporting it as zero would claim a session is stable on the strength of
never having been looked at twice. Negative is a real and useful reading: pruning worked.

**Measurement is independent of the watermark.** Bytes are recorded on every path a pass
takes, including the one where a backed session is left untouched because its environment
is unreachable — otherwise the byte report would be empty on exactly the install whose
growth is unbounded. A watermark records what an authority confirmed; bytes held are a
local fact needing no authority, hence a separate table.

Measurements include uncompressed block text and metadata, original native-record bytes,
retained native sidecar bodies, and staged sidecar chunk bytes. An unreadable reference
contributes no body bytes: its reported source size is not data the server holds. Sessions
holding only raw records or staged sidecars still participate in the accounting pass.

## Execution node projection

The interactive transcript reads a materialized projection in the journal, independent
of the tier holding original events. Event insertion, producer receipts, node updates,
and ancestor summary deltas commit together. Archiving removes event rows, never node
addresses. Replay and export continue to read original records.

A node address is scoped to the existing session identity. Logical streams keep their
first address and ordering across phases and late completion. Direct parentage expresses
execution groups and tool results as well as delegated trajectories; `span` alone is
not the UI tree. Consecutive execution items share a group within each trajectory and
turn. User messages and outward assistant replies end that group. First declaration
owns a delegated trajectory even when its close arrives on another turn.

Nodes contain bounded previews, a body address and byte lengths, first ordering key,
start/end clocks, direct child count, and materialized descendant counters. Failure,
running, and waiting counters propagate by differences along affected ancestor paths.
Running counts describe active leaves: a running ancestor does not count again when
its children are already active. Parallel duration is an interval, never a sum. Token usage remains explicitly scoped to
its reporting node; parent and child totals are not added. Pending approvals remain
durable request resources with their existing authorization and resolution endpoints.

Child pages seek an index on session, parent, ordering and node address. Every response
is bounded in both nodes and encoded bytes. Initial reads request recent root nodes;
expansion requests only direct children. No collapsed subtree or body is hydrated.
A read transaction captures a projection revision with the page. Changes after that
revision are bounded patches, including ancestor summaries, rather than replayed bodies.
Readers reconcile by stable node identity and revision; an older page cannot overwrite
a newer patch. Current node revisions are indexed per parent, so polling an unchanged
branch does not traverse its children.

Prompt and pending-request projection updates are transactional with their owning rows.
The first root user event fills an empty turn request before event projection, in the
same transaction as its receipt. Its event address resolves to that prompt. Additional
input with different text retains its own event node and cannot overwrite the request.
Ordinary stream addresses include their turn; trajectory aliases are session-scoped and
can promote a tool without replacing its arguments. A former parent keeps a coalesced
removal revision, and event-to-node addresses survive archive eviction for search jumps.
Cyclic declarations are attached to the root with an explicit structural warning.

Session titles are materialized when root user input is accepted. Metadata reads do not
search event bodies for a missing title or acquire the writer to fill it. An unknown title
stays absent until a producer supplies evidence. This also bounds the session lookup used
by file browsing when an untitled session has a large transcript.

A separate body revision changes only when a node's own content changes. Updating an
ancestor's counters cannot cause an expanded body to download again. A loaded body stays
readable while newer content is available through an explicit reload action; unused full
bodies leave the browser cache immediately. The node preview cache remains independently
bounded.

Execution lifetime is separate from UI parentage. Ordinary root streams close as
unfinished when their turn seals; an independent delegated scope can continue across
that boundary. Explicit trajectory closure seals only ordinary streams in that scope,
not nested independent scopes. Open-scope indexes bound this work to affected streams.
A later result clears an unfinished invocation without counting missing completion as
failure. A completed ordinary content block, or one sealed unfinished by its scope,
rejects subsequent deltas in the projection. The original events remain retained;
completed corrections and independent delegated progress still apply.

Tool events carry a bounded `toolSummary` derived by the server before preview clipping.
Command/read counters and browser headers consume that same description. Vendor input
shapes include nested arguments from Claude, Pi and native Codex, and top-level command
metadata from live Codex. Expanded edit details read the complete vendor arguments.

On-demand bodies follow the durable event-to-node index across both physical tiers.
A journal snapshot captures the current body boundary and hot fragments; archived
fragments whose terminal sequence is still hot are excluded. Started/completed
replacement boundaries prevent an older revision from being appended to a new one.
The assembled text and metadata lengths must match the projection, so a missing retained
fragment fails visibly instead of returning a plausible suffix.
