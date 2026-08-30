# Storage

The record is lossless, replayable from any frontend, and pruned only when a
downstream authority has confirmed the data — never because time passed. The
implementation is `backend/src/storage/`.

## What "lossless" has to mean here

A turn's event stream is not a log of text. It is a log of text *arriving*: a hundred
`delta` events, each a few tokens, then one `completed` carrying the whole block. Keep
only the `completed` events and the timeline can still be re-rendered, but the turn can
never be replayed — what a client actually saw is gone.

Replay is the requirement that forces this design. If a frontend can only see final
state, "open this conversation on my phone and watch what happened" is impossible, and
every future frontend inherits that limit.

So losslessness is stated precisely, because the naive reading is ruinously expensive
and the precise one is nearly free:

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

Three access patterns want opposite shapes from one table:

| Workload | Rate | Row | Read pattern |
| --- | --- | --- | --- |
| Live turn append | thousands/turn | tiny | tail-follow by `seq` |
| Timeline read | once per open | one per block | full scan by session |
| Cross-session search | rare | — | FTS5 index beside `history.db` |

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
  blobs/          cold  content-addressed blobs, sharded aa/bb/<hash>
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

**Only ephemeral events are batched, and that is forced rather than cautious.** The
two kinds of event already have different replay paths: a stable event is journaled
*before* it is published, so that a client reconnecting immediately after seeing it
always finds it in replay, while `EventHub` keeps a bounded in-memory ring of only
ephemeral events, which is what a mid-stream reconnect replays from.

| | Replayed from | Batching it would |
| --- | --- | --- |
| stable | the journal | open a window where a reconnect sees a hole |
| ephemeral | the hub's ring | cost nothing observable |

Batching only ephemeral events captures nearly all the win — deltas are thousands
per turn against tens of stable events — while leaving the reconnect invariant
untouched and needing no flush-on-read machinery to preserve it.

The window also has one ordering rule, and it is load-bearing: **a turn's buffer
must be flushed before consolidation folds it.** The fold reads journal rows and
then deletes them, so a window still in memory would be silently omitted from the
block and the rows that would have fixed it destroyed. The flush is awaited on the
settle path for exactly this reason.

## Consolidation

When a turn reaches a terminal state, its rows move from `journal.db` to
`history.db`, folding as they go:

1. Group journal rows by `stream_id` into blocks.
2. For each block, keep the `completed` text; encode its deltas as offset and
   timing arrays.
3. Externalize any payload of 4 KB or more to `blobs/`, leaving a hash reference.
4. Compress remaining inline text of 512 bytes or more with zstd — but keep the
   compressed form **only if it is actually smaller**. Incompressible text (base64,
   random ids) otherwise grows by the frame header and costs a decode on every read, so
   such a row stays `raw` despite being over the threshold. Encoding is recorded per
   row rather than inferred from length, which is what makes that safe.
5. Insert the blocks into `history.db`, then delete the turn's journal rows —
   in that order, and never the reverse.

The ordering is the correctness rule: history is durable before the journal
forgets. A crash between them leaves a turn present in both, which
consolidation makes idempotent by keying on `(session_id, seq)`. A crash the
other way would lose the turn outright.

Consolidation is also the recovery path. A turn found in `journal.db` at startup
whose status is terminal was interrupted mid-consolidation, and is simply
consolidated again.

**An open delegated trajectory is preserved, and preserved as open.** Step 2 keys
a block on its `completed` event, so a trajectory that never closed had no block
to be: its `started` phase carries no text, so it reached neither the completed
path nor the leftover-delta path, and the whole delegation vanished from history
while its children survived and reattached to the root. Such a block is therefore
written from its declaring phase, keeping `phase: "started"` — a client reads a
trajectory's status off its phase, so that says *running*, which is what the work
is. Consolidation is not entitled to invent an outcome nobody reported.

This matters because a trajectory left open is no longer exceptional. The
turn-settle sweep deliberately skips work an adapter declares backgrounded
(`docs/session-protocol.md` § Delegated trajectories), so for exactly the
delegations that outlive their turn, reaching consolidation still open is the
normal path rather than a crash artifact.

`started` is a true statement only while the process that would report the
outcome is alive, and no link survives a restart. So startup closes what history
still holds as running, as `abandoned` — the same repair the journal's own startup
sweep performs, one tier down, and for the same reason: an unexplained gap is
worse than a recorded one. It stays correctable, because a server-authored close
is precisely what a later real outcome may overwrite.

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

Blobs are keyed by `blake3(content)`, sharded two levels (`blobs/a1/b2/<hash>`) to
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

The only downstream copy of a session is the **vendor's own native store**, so the only
question retention can ask is whether that store still exists and still holds the
session. There is no second Sylph-side copy to wait for (`docs/architecture.md` § One
authority).

| Class | When | Rule |
| --- | --- | --- |
| **Backed** | the vendor's native store still holds this session, on a reachable environment | prune deltas below the confirmed bound |
| **Terminal** | no reachable native source — a sandbox that is gone, an unpaired environment, a session the vendor never wrote | compact, never prune |

A terminal session's record exists *only* here, so there is nothing to wait for and
pruning would destroy the sole copy. It is still compacted — deltas folded, blobs
externalized, text compressed — but never deleted.

Note what the boundary rests on: whether a native source exists **and is reachable**,
never who started the session. Reading it as "sessions Sylph started are backed"
would leave a sandbox session prunable when its store died with the sandbox, and
reading it the other way leaves everything terminal and prunes nothing — which is
indistinguishable from retention working correctly on a terminal-only install. That
is the failure the reporting below exists to make visible.

**A confirmation is a statement about a native source, so only the environment
holding it can make one.** The envagent answers it, because the file is on its disk:

```rust
#[async_trait]
pub trait RetentionAuthority: Send + Sync {
    /// A name for the journal, so a stored watermark records what produced it.
    fn name(&self) -> &'static str;

    /// How far a downstream copy is confirmed to extend.
    async fn confirm(&self, session: &SessionId) -> Result<Confirmation>;
}

pub struct Confirmation {
    /// Highest sequence number safe to drop. `None` means drop nothing.
    pub safe_to_drop: Option<i32>,
    /// What the environment reported about the native source, for the audit row.
    pub source: Option<NativeSourceState>,
    /// Why nothing was confirmed, when nothing was.
    pub withheld: Option<WithheldReason>,
}
```

`TerminalAuthority` always answers `Confirmation::nothing()`. An
`EnvagentAuthority` asks the environment that owns the session, and an environment
that is not connected yields **no confirmation at all** — which is the next rule.

**"Cannot ask" must never be recorded as "asked, and nothing is safe".** A watermark
row is a claim that a check happened. An unreachable environment means the question
was never put, and writing a zero watermark for it would let a later pass believe a
bound was established. This distinction is why `Confirmation` carries a `withheld`
reason rather than only an `Option`.

**A degraded dependency is one fact per pass, not one per session.** Probe an
environment's reachability once per pass and log an absent one once. Per-session probing
turns one missing dependency into a warning per session per pass, and a timer-driven
caller then reproduces the same unbounded stream one layer down. **An install with no
environment paired is a supported configuration, not a fault**, so it must not be
reported as one forever.

**A subsystem whose correct behaviour is "nothing happened" needs a report.** Keeping
every block is right for a terminal session and a symptom for a backed one, and the
observable state — no blocks pruned — is identical in both cases *and* identical to never
having run at all. So the watermark row is written even when nothing is confirmed, and
`/diagnostics` reports confirmed / withheld / terminal / unchanged **separately**: a
single `pruned: 0` answers none of the questions that matter.

## Replay

Replay belongs to the server, not to each frontend. A client asks for a session's
stream and receives events in original order with original relative timing:

```
GET /api/v1/sessions/{id}/replay?from_seq=0&speed=1
```

**Both parameters are optional and neither default is the value shown above.**
`from_seq` defaults to `-1` (from the beginning) and `speed` to `0.0` (emit
everything immediately, unpaced) — so a bare `GET …/replay` is the timeline-render
case, which is what almost every caller wants. Pass `speed=1` explicitly for
original-timing playback.

Paced replay is also deliberately not faithful to long gaps: `MAX_GAP` clamps any
inter-event sleep to 2 seconds. A user who steps away mid-session leaves a
minutes-long hole in the record, and reproducing it would hold a connection open
doing nothing — the pacing conveys how the work unfolded, not the coffee break.

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
  phase        TEXT,                  -- an open trajectory keeps 'started'
  stream_id    TEXT,
  span         TEXT,                  -- delegated trajectory; NULL is the root
  name         TEXT,
  -- Exactly one of these carries the text.
  text_inline  BLOB,                  -- possibly zstd, see text_encoding
  text_hash    TEXT,                  -- CAS reference when externalized
  text_len     INTEGER NOT NULL DEFAULT 0,   -- uncompressed length, always
  text_encoding TEXT NOT NULL DEFAULT 'raw', -- 'raw' | 'zstd' | 'external'
  -- Delta reconstruction. NULL when the block never streamed.
  delta_offsets BLOB,                 -- packed u32 LE
  delta_timings BLOB,                 -- packed u32 LE, ms deltas
  data_json    BLOB,
  data_hash    TEXT,
  is_error     INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT 'agent',
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

CREATE INDEX cas_refs_by_block ON cas_refs(session_id, seq);

CREATE TABLE watermarks (
  session_id    TEXT PRIMARY KEY,
  authority     TEXT NOT NULL,        -- 'envagent' | 'terminal'
  confirmed_seq INTEGER,              -- NULL: nothing confirmed
  checked_at_ms INTEGER NOT NULL,
  -- What the environment reported about the native source:
  -- 'present' | 'absent' | 'unreachable'. NULL for a terminal session, which has
  -- no native source to describe.
  source        TEXT,
  -- Why nothing was confirmed, when nothing was; NULL when something was. This is
  -- what keeps "the environment answered, and nothing is safe yet" distinct from
  -- "nobody was asked" — see § Retention is watermark-driven.
  withheld      TEXT
) WITHOUT ROWID;

-- What each session holds, and what it held before. Separate from `watermarks`
-- because bytes held need no authority: see "Growth is accounted in bytes".
CREATE TABLE session_bytes (
  session_id     TEXT PRIMARY KEY,
  bytes          INTEGER NOT NULL,
  blocks         INTEGER NOT NULL,
  measured_at_ms INTEGER NOT NULL,
  previous_bytes          INTEGER,
  previous_measured_at_ms INTEGER
) WITHOUT ROWID;

-- Cross-session search. Standalone rather than external-content: see below.
CREATE VIRTUAL TABLE blocks_fts USING fts5(
  text,
  session_id UNINDEXED,
  seq        UNINDEXED,
  tokenize = 'jieba'          -- see Tokenization below
);
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

### `blocks_fts` is standalone, and that is forced rather than chosen

Cross-session search is one FTS5 table beside `history.db`, written by consolidation in
the same transaction as the blocks it indexes.

**It cannot be an external-content FTS5 table**, which is the obvious shape and the wrong
one here. External content re-reads the column from the base table at query time, and
`blocks.text_inline` is not reliably text: `text_encoding` says it may be `zstd`, and when
the text is externalized the column is `NULL` and the bytes live under `blobs/`. Such an
index would hold compressed bytes for some rows and nothing at all for the rows most worth
finding — the large ones. Consolidation therefore inserts the *decoded* text, which costs a
second copy of it and is the price of the tier split.

`session_id` and `seq` ride along `UNINDEXED` so a hit resolves to a block without a join
back through a rowid mapping that would have to be maintained separately.

### Tokenization

**The tokenizer is `jieba`, because `unicode61` cannot search Chinese.** `unicode61` splits
on non-alphanumerics, so an entire run of Han characters becomes one token and a query for
any substring of it matches nothing — which matters here, since this project's own
transcripts are substantially Chinese. `crates/fts5-jieba` segments CJK, folds diacritics,
and stems English, so **one tokenizer serves both languages** and adopting it costs no
English capability.

**It is linked into the binary, not loaded as an extension**, and that is a requirement
rather than a convenience. A loadable extension is a per-platform build artifact that can
be missing at runtime, which would make "extension absent" a state the server could run
in — and it must not, because **an index built with one tokenizer and queried with another
silently returns nothing.** A fallback to `unicode61` would therefore answer *wrongly*
rather than answering worse, which is the failure this design refuses. Registration happens
per connection in `HistoryStore::open` and a failure there is fatal.

**The dictionary is process-wide.** Building it costs about 600 ms, and FTS5 creates a
tokenizer per table per connection, so a per-instance dictionary multiplies that by the
pool size on each connection's first query. Sharing is sound because the dictionary is
immutable once built and tokenizing takes `&self`.

Two consequences for anyone changing the tokenizer later. **The index is tokenizer-specific,
so a change requires a rebuild** — the FTS rows have to be reinserted, since existing terms
were segmented by the old rules. And **a test for CJK search must fail against
`unicode61`**, or it is not evidence about the index: `chinese_text_is_searchable_by_word`
in `backend/tests/storage.rs` is written that way and was verified by reverting the
tokenizer.

### A user's query is not an FTS5 query

**Quote the user's text into a phrase.** FTS5 reads punctuation as syntax, so
`MATCH 'needle-one'` parses as `needle` NOT `one` and answers `no such column: one` — an
ordinary search string producing an error about a column, three layers from the text box
that typed it. Hyphens are the common case, because identifiers and flags are exactly what
someone searches a transcript for.

**Binding the value as a parameter is not sufficient**, and this is the trap: binding
protects against SQL injection, and the bound value still reaches FTS5's *expression*
parser. A comment asserting that a parameter is safe here is describing the wrong hazard.

The tradeoff is worth stating, because quoting changes behaviour rather than only hardening
it: a multi-word query becomes a phrase match rather than an implicit AND, so `foo bar`
finds "foo bar" and not a block mentioning both apart. That is the safe direction — it never
errors and never silently inverts a term — and supporting operators means *parsing* the
user's text rather than escaping it.

**Keep a test whose query contains punctuation.** Without one this is invisible, because
every single-word query works — which is exactly how it survived as a documented rule the
code did not implement.

## Deriving a native-source bound

An environment cannot report "I have ingested through event 400", because a vendor
CLI has no such notion. What it *can* report is what its own store holds, and a bound
is derived from two facts about that rather than assumed.

**A native store is written whole-message, not half-delivered.** Each of the three
supported agents appends complete records — `claude` at content-block boundaries,
`codex` at item completion, `pi` at `message_end` (`docs/agents.md`). So the last
record in the file is complete, and everything before it is too.

**Pruning deletes whole rows**, not merely their delta arrays: the operation is
`DELETE FROM blocks WHERE session_id = ? AND seq <= :watermark`. So retention is *not*
lossless against the local store, and that is the entire reason a watermark gates it —
what makes the deletion safe is a confirmation that the vendor's own store still holds
those records, never a claim that Sylph kept them. Intra-message delta timing is the one
thing no native store holds, so that is destroyed outright.

**A record's identity is the vendor's own.** The bound is expressed as the last
native record the environment can see, translated by the adapter into the sequence
number of the Sylph event it produced. The translation belongs to the adapter for the
same reason parsing does: only it knows the vendor's record layout.

Two rules make a derived bound safe:

* **Bound it by the session's own end**, not by wall-clock recency. A store flushed
  mid-turn is at message granularity, so its last record may lag the live stream by a
  whole tool call.
* **Withhold rather than guess on any ambiguity.** A store whose last record does not
  map cleanly onto an event, a file that was truncated mid-write (a real `pi` hazard,
  `docs/agents.md`), or a session the store does not contain at all all yield
  `Confirmation::nothing()` with a reason — never a zero bound, which would read as a
  completed check.

**A session with no native source is terminal, not merely unconfirmed**, and the type
enforces it: an authority that needs a native source to be constructed cannot be asked
about a session that has none. This is the shape that replaces the old two-id-space
hazard — the fix is a type that cannot express the unanswerable question, not a more
careful comparison.

### Retention runs on a schedule, and reports what it did

A pass runs at startup and every five minutes over a bounded batch of sessions that
hold blocks, ordered least-recently-checked first so successive passes rotate
through all of them rather than re-examining one prefix.

Three things make it safe to run often and safe to run at all:

* **A watermark row is written even when nothing is confirmed.** Otherwise "checked,
  and nothing is safe yet" is indistinguishable from "never checked", and those call
  for opposite responses. An empty `watermarks` table looks exactly like a correctly
  conservative one — which is how a retention path that confirmed nothing stayed
  invisible.
* **An unreachable environment is one fact about the pass, not N session failures.**
  Each environment is probed once per pass. Without it, that environment's sessions
  are left entirely alone: not pruned, and *not* recorded as checked, because "cannot
  ask" must not be stored as "asked, and nothing is safe". An install with no
  environment paired is a supported state, and reporting it per session per pass
  forever would bury it. The same discipline applies one layer down, in whatever holds
  the connection: report an absent peer once at `error` and at `debug` after, because a
  timer-driven caller otherwise reproduces the identical unbounded stream from one
  missing dependency.
* **Blob sweeping is per pass, not per session.** Reachability spans sessions — the
  same file read in two sessions is one blob — so the sweep is a global scan over
  `cas_refs`. Calling it per session would run that scan once per session for one
  pass's worth of work.

`/api/v1/diagnostics` reports the last pass: examined, confirmed, withheld,
terminal, unchanged, blocks pruned, blobs swept, and which environments were
reachable. A subsystem whose correct behaviour is usually "nothing happened" is
otherwise indistinguishable from one that is broken.

### Growth is accounted in bytes, per session

The counts above answer *was anything dropped*. They cannot answer the question an
operator has when a disk fills: **which session is responsible**. A terminal
session keeping every block is correct behaviour, so `withheld: 41` is both the
right report and completely silent about the one sandbox session that has reached a
gigabyte while forty others sit at kilobytes. Counting blocks cannot separate them
either — one 200 KB tool result and one 20-byte status are both one block.

So each pass measures the sessions it examined and records the result in
`session_bytes`:

```sql
CREATE TABLE session_bytes (
  session_id     TEXT PRIMARY KEY,
  bytes          INTEGER NOT NULL,
  blocks         INTEGER NOT NULL,
  measured_at_ms INTEGER NOT NULL,
  previous_bytes          INTEGER,   -- the last *differing* observation
  previous_measured_at_ms INTEGER
) WITHOUT ROWID;
```

`/diagnostics` then reports `bytesHeld`, `sessionsMeasured`, `bytesGrewBy`, and a
bounded `largestSessions` list, each row carrying its own `grewBy` and
`grewOverMs`.

Five decisions in that shape are load-bearing.

**A byte is the logical record, not the footprint on disk.** `text_len` is the
uncompressed length even for an externalized payload — that is why the column
exists — so this measures what the record contains. Stored size was rejected on
three counts: compression ratio varies 1–5× with content, so ranking by it ranks
partly by how compressible a session's tool output happened to be; measuring it
means a `stat` per blob per pass or a size column to keep correct through pruning
and sweeping; and a blob shared by two sessions is one file, so stored bytes summed
per session over-counts it with no well-defined share. The consequence to hold
onto: **the sum over sessions exceeds the store on disk**, and this measure answers
"which session is growing", not "what is my disk doing". `du` answers that, exactly.

**Growth needs a stored earlier observation.** Current size is a `SUM` and needs no
table. A difference does, and without one the only available answer is "large" —
which does not distinguish a session that reached its size months ago and stopped
from one adding a hundred megabytes an hour. Those need opposite responses, and the
second is why this exists.

**The baseline moves only on a differing measurement.** Re-recording an unchanged
reading would set `previous_bytes = bytes` and report a growing session as stable
the moment two passes happen to agree. Held at the last differing observation
instead, so the delta stays meaningful however often passes run.

**`grewBy` is absent, not zero, on a first measurement, and signed thereafter.**
One observation is not a trend, and reporting it as zero would claim a session is
stable on the strength of never having been looked at twice. Negative is a real and
useful reading: pruning worked. A field that only reported increases would make
working retention indistinguishable from stalled retention.

**Measurement is independent of the watermark, and this is the property an obvious
implementation gets wrong.** Bytes are recorded on every path a pass takes,
including the one where a backed session is left entirely untouched because its
environment is unreachable. Measuring alongside the watermark write would have left
the byte report **empty on exactly the install whose growth is unbounded** — one with
no environment paired, where nothing can ever be pruned. A watermark records what an authority
confirmed; bytes held are a local fact needing no authority. Hence a separate table
rather than more columns on `watermarks`.

### Why no configurable upper bound is offered

The obvious next step is a setting — "prune a terminal session above N GB" — and it
is refused rather than unbuilt.

A terminal session's record exists **only** here. There is no downstream copy, and
none is coming: that is what makes it terminal. A size-triggered prune is therefore
a size-triggered *deletion of the only copy*, which is the same trade a time-based
rule makes and is refused for the same reason — it destroys data precisely where
nothing else holds it. "Growth is visible and fixable; loss is neither" is not a
slogan here, it is the whole basis of the watermark design, and a byte threshold is
a clock with a different face.

A **reporting** threshold — warn above N — was considered and also left out. It
changes a log line's severity and nothing about what is kept, so it would be a
setting whose only effect is how loudly the server describes correct behaviour;
`/diagnostics` already carries the numbers a decision would be made from, and
`AGENTS.md` records why a setting the code does not honour is worse than one not
offered. The pass logs one line naming the single largest holder when anything
grew — one line, not one per session, because growth every pass is a terminal
session's *expected* state and a line per session would be an unbounded stream
reporting nothing wrong.

What an operator does about a session that is too large is delete the session, or
pair the environment holding its native source so it stops being terminal. Both are
decisions with an owner. Neither belongs to a background timer.

### One id space, and why the type enforces it

Identity is `(environment, agent, native_id)` throughout — the same triple the vendor's own
store keys on (`docs/architecture.md`). There is no translation layer, so a comparison
across two id spaces cannot be written here.

That constraint is worth carrying into any new subsystem that talks to an external store,
because the failure it prevents is invisible. Compare an id against a store that keys the
same thing differently and there is no error at any layer: the store answers "no such
session", the caller concludes nothing is confirmed, retention correctly keeps everything,
and every component behaves impeccably while the subsystem does nothing at all. An empty
watermark table looks exactly like correct conservatism.

**The fix is a type, not a more careful comparison.** Something that needs the downstream
identity in order to be constructed cannot be asked the unanswerable question. And any test
covering such a comparison needs a *real* external id, because a wrong one passes.
