# Storage

How the record is kept so that it is lossless, replayable from any frontend, and pruned
only when a downstream authority has confirmed the data. The implementation is
`backend/src/storage/`, and the tables are in `backend/migrations/history/`.

**This file owns `history.db` and `blobs/`.** `docs/database-schema.md` owns `journal.db`.

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

## Three tiers, because there are three workloads

| Workload | Rate | Row | Read pattern |
| --- | --- | --- | --- |
| Live turn append | thousands/turn | tiny | tail-follow by `seq` |
| Timeline read | once per open | one per block | full scan by session |
| Cross-session search | rare | — | FTS5 index beside `history.db` |

Conflating the first two is a property of SQLite rather than of any schema: **a
long-running read transaction blocks WAL checkpointing.** A browser scrolling a large
archived session holds a read open while a live turn appends; the WAL cannot checkpoint,
so it grows, and every subsequent append pays a longer WAL to walk. Streaming latency then
degrades with unrelated browsing, which presents as a network problem.

```
data_dir/
  journal.db      hot   unconsolidated turns only; stays a few MB
  history.db      warm  consolidated blocks; grows without bound, read-mostly
  blobs/          cold  content-addressed blobs, sharded aa/bb/<hash>
```

`journal.db` staying small is the point of the split, and consolidation is what guarantees
it. Checkpoint cost is bounded because the file is bounded, so hot-path latency does not
depend on how much history exists.

Durability differs because the requirements differ: `journal.db` runs
`synchronous = NORMAL`, since a crash loses at most the tail of a turn the client can see
was interrupted, and `FULL` per delta batch would put an fsync in the token path.
`history.db` runs `FULL`, because it is the record.

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
5. Insert the blocks into `history.db`, then delete the turn's journal rows.

**Step 5's order is the correctness rule:** history is durable before the journal forgets.
A crash between them leaves a turn present in both, which consolidation makes idempotent
by keying on `(session_id, seq)`. A crash the other way loses the turn.

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

## Blocks, and the three time axes

A block is one consolidated event: its `completed` sequence number, turn, kind, phase,
`stream_id`, `span`, and name; its text either inline (possibly zstd) or as a CAS hash,
with the uncompressed length always stored; its packed delta offsets and timings; and its
JSON payload under the same inline-or-hash rule. `(session_id, seq)` is the primary key
and `WITHOUT ROWID`, because the key *is* the access path.

`session_id` is the single-column encoded triple here rather than three columns, because a
foreign key cannot cross the database split (`docs/database-schema.md` § Identity in the
schema).

The uncompressed length is stored even when the text is externalized, so a timeline can
lay out and truncate without touching CAS. Fetching a 200 KB blob to learn it is large is
the read amplification that makes a list view slow.

`seq` is the sole replay and ordering axis. Three time fields answer three different
questions:

| Field | Whose clock | What it means |
| --- | --- | --- |
| `arrival_at_ms` | the server's | when the event was received |
| `recorded_at_ms` | the vendor's | when the vendor says it happened, when it says |
| `started_at_ms` | derived | the block's first delta, or the event itself |

`started_at_ms` is what makes timing replayable — a block's deltas are offsets from its
start, and without the start they are relative to nothing. It is paired with
`recorded_at_ms` when the source supplied a recorded close, and with `arrival_at_ms` for a
stream record, so **a native source with no recorded close cannot claim a duration merely
because its row has an arrival timestamp.**

### Native records are kept whole

Ingest persists each vendor record before projecting later messages: the parsed JSON, and
for JSONL sources the original line bytes. The parsed value drives projection; the raw
bytes preserve lexical detail a JSON value cannot represent — key order, duplicate keys,
whitespace, escape spelling. Large sidecar payloads are recorded as validated locators and
fetched into CAS only when a caller asks for the body, so ingest never blocks on an eager
full-file read.

Native records are a separate table from projected blocks for two reasons: re-projection
needs fields the current adapter ignores, and raw records are never safe to prune against
an event watermark.

Raw retention is an ingestion prerequisite rather than best-effort telemetry. A write is
retried once for a transient SQLite or sidecar race; if it still fails, adopt returns an
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
and `seq` `UNINDEXED` so a hit resolves without a separate rowid mapping.

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
`chinese_text_is_searchable_by_word` in `backend/tests/storage.rs` is written that way and
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
| **Terminal** | no reachable native source — a sandbox that is gone, an unpaired environment, a session the vendor never wrote | compact, never prune |

A terminal session's record exists *only* here, so there is nothing to wait for and
pruning would destroy the sole copy. It is still compacted — deltas folded, blobs
externalized, text compressed — and never deleted.

The boundary rests on whether a native source exists **and is reachable**, never on who
started the session. An unpaired environment is unreachable, so its sessions become
terminal and keep everything (`docs/environments.md` § Unpairing).

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
asked about a session that has none.

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
  environment is probed once per pass, and its sessions are then left alone: not pruned,
  and not recorded as checked. Report an absent peer once at `error` and at `debug`
  thereafter, in whatever holds the connection — a timer-driven caller otherwise
  reproduces the same unbounded stream from one missing dependency. **An install with no
  environment paired is a supported configuration**, so it must not be reported as a fault
  forever.
* **Blob sweeping is per pass, not per session**, since reachability spans sessions.

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

**Growth needs a stored earlier observation.** Current size is a `SUM` and needs no table.
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