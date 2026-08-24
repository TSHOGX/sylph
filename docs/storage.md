# Storage

The record is lossless, replayable from any frontend, and pruned only when a
downstream authority has confirmed the data — never because time passed.

## What "lossless" has to mean here

A turn's event stream is not a log of text. It is a log of text *arriving*: a
hundred `delta` events, each a few tokens, then one `completed` carrying the whole
block. Today only `completed` is durable, so what a client saw is unrecoverable —
the timeline can be re-rendered, but the turn cannot be replayed.

Replay is the requirement that forces the design. If a frontend can only see final
state, then "open this conversation on my phone and watch what happened" is
impossible, and every future frontend inherits that limit.

So losslessness is stated precisely, because the naive reading is ruinously
expensive and the precise one is nearly free:

> Every event a client could have observed is reconstructible byte-for-byte,
> with its arrival time.

## Deltas are not independent data

Storing each delta as a row costs about 20× the block it belongs to. A 2 KB
assistant message arrives as ~400 deltas; as rows that is ~24 KB of mostly
duplicated text, since **every delta is a prefix-suffix of the same final
string**.

Nothing is lost by storing the final text once plus the *boundaries*:

```
block text:  "I'll start by reading the config file."
deltas:      ▲    ▲      ▲  ▲       ▲   ▲      ▲    ▲
offsets:     [6, 11, 18, 21, 29, 33, 40, 46, ...]
timings:     [0, 34, 51, 12, 40, 28, 33, 19, ...]   ms since block start
```

Two `u32`s per delta — 8 bytes against ~60 for a row — and the reconstruction is
exact: delta *n* is `text[offsets[n-1]..offsets[n]]`, arriving at
`block_start + cumsum(timings)[n]`. For that message, 3.2 KB instead of 24 KB,
with nothing approximated.

This is the same move as a keyframe plus its deltas, or a git packfile: store the
resolved state, plus the minimal instructions for how it came to be. It is what
makes lossless replay affordable rather than a thing to be traded away.

Offsets are byte offsets into UTF-8, not character indices, because that is what a
reconstructor slices. A delta boundary can land mid-codepoint when a model emits a
multi-byte character in pieces, so a replayer must buffer an incomplete tail rather
than assume every boundary is a valid string edge.

## Three tiers, because there are three workloads

One `events` table currently serves three access patterns with opposite optimal
shapes:

| Workload | Rate | Row | Read pattern |
| --- | --- | --- | --- |
| Live turn append | thousands/turn | tiny | tail-follow by `seq` |
| Timeline read | once per open | one per block | full scan by session |
| Cross-session search | rare | — | inverted index (TraceDB) |

Conflating the first two is the performance bug waiting to happen, and it is a
property of SQLite rather than of this schema: **a long-running read transaction
blocks WAL checkpointing.** A browser scrolling a large archived session holds a
read open; meanwhile a live turn is appending. The WAL cannot checkpoint, so it
grows, and every subsequent append pays a longer WAL to walk. The symptom is
streaming latency that degrades with unrelated browsing — which reads as a network
problem and is not one.

So:

```
data_dir/
  journal.db      hot   unconsolidated turns only; stays a few MB
  history.db      warm  consolidated blocks; grows without bound, read-mostly
  cas/            cold  content-addressed blobs, sharded aa/bb/<hash>
```

`journal.db` staying small is the whole point of the split, and it is what
consolidation guarantees. Checkpoint cost is bounded because the file is bounded,
so the hot path's latency does not depend on how much history exists.

Pragmas differ because the durability requirements differ:

| | `journal.db` | `history.db` |
| --- | --- | --- |
| `synchronous` | `NORMAL` | `FULL` |
| Why | A crash loses at most the tail of a turn that was still streaming, which the client can see was interrupted. Paying `FULL` per delta batch would put an fsync in the token path. | It is the record. |

Deltas are batched into one transaction per ~50 ms rather than committed
individually. This is the single largest lever on the hot path: it converts
thousands of tiny transactions into tens, and fsync count is what actually costs.
The window is a latency/loss tradeoff, not a throughput knob — 50 ms is below
human perception for stream smoothness and bounds loss to one window.

## Consolidation

When a turn reaches a terminal state, its rows move from `journal.db` to
`history.db`, folding as they go:

1. Group journal rows by `stream_id` into blocks.
2. For each block, keep the `completed` text; encode its deltas as offset and
   timing arrays.
3. Externalize any payload over 4 KB to `cas/`, leaving a hash reference.
4. Compress remaining inline text over 512 bytes with zstd.
5. Insert the blocks into `history.db`, then delete the turn's journal rows —
   in that order, and never the reverse.

The ordering is the correctness rule: history is durable before the journal
forgets. A crash between them leaves a turn present in both, which
consolidation makes idempotent by keying on `(session_id, seq)`. A crash the
other way would lose the turn outright.

Consolidation is also the recovery path. A turn found in `journal.db` at startup
whose status is terminal was interrupted mid-consolidation, and is simply
consolidated again.

### Thresholds, and why these numbers

**4 KB for externalization.** Below a filesystem block, a separate file wastes
the rest of the block and costs an inode, so small payloads are strictly worse
outside. Above it, an inline blob inflates every row scan that does not need the
payload — and timeline reads usually don't, since a 200 KB file read renders as a
collapsed summary until expanded.

**512 bytes for compression.** zstd on shorter strings loses to its own frame
overhead. Above it, agent output compresses 3–5× because tool output, code, and
diffs are extremely redundant.

Both are recorded in the row, not assumed globally, so a stored row remains
readable after a threshold changes. A design where the threshold is implicit
cannot be tuned without a migration.

## Content addressing

Blobs are keyed by `blake3(content)`, sharded two levels (`cas/a1/b2/<hash>`) to
keep directory sizes sane, and refcounted.

Dedup is not incidental here — it is large. An agent reads the same file across
many turns of a session, and the same dependency source across many sessions. The
same content stored once is a property of addressing by hash, not an optimization
layered on top.

blake3 rather than SHA-256 for throughput on the hot path, and because collision
resistance is the only cryptographic property needed: this is a content index, not
a trust boundary. Content is never executed or trusted on the basis of its hash.

## Retention is watermark-driven, never time-driven

A time-based rule deletes data because a clock advanced, which is exactly the
case where the data may not exist anywhere else yet. So the only question
retention ever asks is:

> What sequence number has a downstream authority confirmed for this session?

```sql
DELETE FROM blocks WHERE session_id = ? AND seq <= :watermark
```

This is the discipline behind a Postgres replication slot's `restart_lsn` and a
Kafka consumer offset: the upstream keeps what any consumer might still need, and
a lagging consumer causes *growth*, never loss. Growth is visible and fixable; loss
is neither.

The watermark is the **minimum** across every consumer that must see the data.
Taking a maximum, or any single consumer's position, deletes data a slower
consumer has not read.

### Two retention classes, because one is not honest

TraceDB ingests from each agent's **native store on the host**. That gives two
genuinely different situations, and collapsing them would silently lose the
second:

| Class | When | Rule |
| --- | --- | --- |
| **Delegated** | A native store exists and TraceDB ingests it | Prune below the watermark |
| **Terminal** | No ingestion path — a sandbox whose store dies with it | Compact, never prune |

A terminal session's record exists *only* here. There is no watermark to wait
for, and pruning would destroy the sole copy. Such a session is still compacted —
deltas folded, blobs externalized, text compressed — but never deleted.

This is why the watermark is an interface with one question rather than a TraceDB
call:

```rust
#[async_trait]
pub trait RetentionAuthority: Send + Sync {
    /// The highest seq that is safe to drop for this session.
    /// `None` means drop nothing.
    async fn safe_to_drop(&self, session_id: &str) -> Result<Option<i32>>;
}
```

`TerminalAuthority` always answers `None`. A TraceDB-backed authority answers by
confirming what the archive holds. Whether that confirmation is cheap to derive is
a property of one implementation, not an assumption the storage design rests on —
and it is the open question below.

CAS objects are refcounted separately: an object is removed when no live block
references it, which is independent of any watermark. A blob shared by a pruned
block and a retained one survives, because refcounts are the authority on
reachability and sequence numbers are not.

## Replay

Replay belongs to the server, not to each frontend. A client asks for a session's
stream and receives events in original order with original relative timing:

```
GET /api/v1/sessions/{id}/replay?from_seq=0&speed=1
```

The server merges `history.db` blocks with any live `journal.db` tail, expands
delta arrays back into individual events, resolves CAS references, and emits the
same SSE frames the original stream carried.

Doing this server-side is what makes "one authoritative record, many frontends"
true rather than aspirational. Reconstruction from folded blocks is subtle —
UTF-8 boundaries, interleaving by `seq` across two databases, timing accumulation
— and a rule implemented once per frontend is a rule that will differ per
frontend. Discord, a terminal, and the browser then disagree about what happened,
which is the exact failure the single record was supposed to prevent.

`speed` is a server-side pacing hint; `speed=0` emits everything immediately,
which is what a timeline render wants.

## Schema

```sql
-- history.db
CREATE TABLE blocks (
  session_id   TEXT NOT NULL,
  seq          INTEGER NOT NULL,      -- the completed event's seq
  turn_id      TEXT NOT NULL,
  kind         TEXT NOT NULL,
  stream_id    TEXT,
  name         TEXT,
  -- Exactly one of these carries the text.
  text_inline  BLOB,                  -- possibly zstd, see text_encoding
  text_hash    TEXT,                  -- CAS reference when externalized
  text_len     INTEGER NOT NULL,      -- uncompressed length, always
  text_encoding TEXT NOT NULL,        -- 'raw' | 'zstd'
  -- Delta reconstruction. NULL when the block never streamed.
  delta_offsets BLOB,                 -- packed u32 LE
  delta_timings BLOB,                 -- packed u32 LE, ms deltas
  data_json    BLOB,
  data_hash    TEXT,
  is_error     INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL,
  started_at_ms INTEGER NOT NULL,     -- first delta, or the event itself
  created_at_ms INTEGER NOT NULL,     -- completion
  PRIMARY KEY (session_id, seq)
) WITHOUT ROWID;

CREATE INDEX blocks_by_turn ON blocks(turn_id, seq);

CREATE TABLE cas_refs (
  hash       TEXT NOT NULL,
  session_id TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  PRIMARY KEY (hash, session_id, seq)
) WITHOUT ROWID;

CREATE TABLE watermarks (
  session_id    TEXT PRIMARY KEY,
  authority     TEXT NOT NULL,        -- 'tracedb' | 'terminal'
  confirmed_seq INTEGER,              -- NULL: nothing confirmed yet
  checked_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
```

`WITHOUT ROWID` on `blocks` because the primary key *is* the access path — every
read is by session and seq — so the extra rowid indirection buys nothing and costs
a level of B-tree.

`text_len` is stored uncompressed even when the text is externalized, so a
timeline can lay out and truncate without touching CAS. Fetching a 200 KB blob to
learn it is large is the kind of read amplification that makes a list view slow.

`started_at_ms` beside `created_at_ms` is what makes timing replayable: a block's
deltas are offsets from its start, and without the start they are relative to
nothing.

## Deriving the TraceDB watermark

TraceDB has no ingestion-status call — its API is `list`, `show`, `search`,
`stats`. A watermark therefore has to be derived, and it is, from two facts
confirmed in its source rather than assumed:

**Ingestion is whole-session and transactional.** `Facade::scan` compares a
source fingerprint and either skips the session as unchanged or re-parses it
entirely; there is no incremental append. `IngestMode::retain_full` returns `Full`
unconditionally, and its own comment records why: *"Every new write is lossless.
`partial` remains readable and accepted as a compatibility spelling ... but it can
no longer create a partial record."* So a session present in TraceDB is complete
as of its ingestion, never half-delivered.

**`list` reports the extent of what it holds.** Each row carries `events` (a
count), `ingestedAtMs`, `endedAtMs`, and `mode`:

```json
{"agent":"claude","id":"claude:c1fd24f3-...","events":527,
 "mode":"full","ingestedAtMs":1787536152784,"endedAtMs":1787536141749}
```

That makes the derivation exact:

```
watermark(session) = max seq where created_at_ms <= endedAtMs
                     given mode == "full"
```

`endedAtMs` rather than `ingestedAtMs` is the load-bearing choice. `ingestedAtMs`
is when TraceDB *ran*; `endedAtMs` is the timestamp of the last event it actually
holds. Using the former would assume the agent had flushed its native store
completely by the moment the scan read it — a race whose window is the agent's own
buffering, invisible from here. `endedAtMs` is a statement about data rather than
about timing, so it needs no assumption about flush behaviour at all.

Because the event-identity question never arises, the earlier worry about matching
journal rows to archived events is moot: atomicity makes a timestamp bound
sufficient.

Two safeguards remain, because a derived watermark should be conservative:

* The `events` count is a cross-check. A count far below the journal's block count
  for the same span means the two disagree about what the session contains, and a
  disagreement is a reason to withhold the watermark rather than to reconcile it
  silently.
* A session TraceDB has never seen has no row, which yields no watermark and
  therefore no pruning. The absent case is safe by construction.

An explicit ingestion ack would still be better than a derivation, and it stays
worth doing: it would remove the `endedAtMs` reasoning entirely and let Sylph
prune on confirmation rather than inference. That is a TraceDB change, so it is a
separate decision — but the derivation above is sound enough to run on, which it
was not obvious it would be.
