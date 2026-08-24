-- The warm tier: consolidated blocks, read-mostly, grows without bound.
--
-- A separate database from the journal, and the reason is SQLite's rather than
-- taste (`docs/storage.md`): a long-running read transaction blocks WAL
-- checkpointing. With one file, a browser scrolling a large archived session holds
-- a read open while a live turn appends, the WAL cannot checkpoint, and every
-- subsequent append pays a longer WAL to walk. Streaming latency then degrades
-- with unrelated browsing — a symptom that reads as a network problem.
--
-- Its own pool rather than an attached database, because the two have opposite
-- durability settings: the journal runs `synchronous = NORMAL` so an fsync never
-- lands in the token path, and this runs FULL because it is the record. A crash
-- between the two writes leaves a turn in both, which consolidation makes
-- idempotent by keying on (session_id, seq) — history is made durable *before* the
-- journal forgets, never the reverse.

CREATE TABLE IF NOT EXISTS blocks (
  session_id   TEXT NOT NULL,
  -- The completed event's sequence number, which orders the whole session stream.
  seq          INTEGER NOT NULL,
  turn_id      TEXT NOT NULL,
  kind         TEXT NOT NULL,
  phase        TEXT,
  stream_id    TEXT,
  name         TEXT,

  -- Exactly one of these carries the text: `text_inline` when it is small enough
  -- to sit in the row, `text_hash` when it was externalized.
  text_inline  BLOB,
  text_hash    TEXT,
  -- Always the *uncompressed* length, even when externalized. A timeline can then
  -- lay out and truncate without touching the blob store — fetching 200 KB to
  -- learn it is large is the read amplification that makes a list view slow.
  text_len     INTEGER NOT NULL DEFAULT 0,
  -- 'raw' | 'zstd' | 'external'. Recorded per row rather than assumed globally,
  -- so a row stays readable after a threshold changes.
  text_encoding TEXT NOT NULL DEFAULT 'raw',

  -- Delta reconstruction: packed little-endian u32 arrays. NULL when the block
  -- never streamed — a tool result arrives complete.
  delta_offsets BLOB,
  delta_timings BLOB,

  data_json    BLOB,
  data_hash    TEXT,

  is_error     INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT 'agent',
  -- When the block's first delta arrived. Without it the timing array is relative
  -- to nothing and a replay cannot be paced.
  started_at_ms INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,

  PRIMARY KEY (session_id, seq)
-- The primary key *is* the access path — every read is by session and seq — so the
-- rowid indirection buys nothing and costs a B-tree level.
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS blocks_by_turn ON blocks(turn_id, seq);

-- Which blocks reference which blobs.
--
-- Reachability lives here rather than in a count on the blob, because a blob
-- shared by a pruned block and a retained one must survive. A bare refcount
-- cannot express which references are still live after a partial prune.
CREATE TABLE IF NOT EXISTS cas_refs (
  hash       TEXT NOT NULL,
  session_id TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  PRIMARY KEY (hash, session_id, seq)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS cas_refs_by_block ON cas_refs(session_id, seq);

-- How far a downstream authority has confirmed each session.
--
-- Retention asks exactly one question — what is safe to drop — and never asks how
-- old anything is. A time rule deletes data because a clock advanced, which is
-- precisely the case where it may not exist anywhere else yet.
CREATE TABLE IF NOT EXISTS watermarks (
  session_id    TEXT PRIMARY KEY,
  -- 'tracedb': an agent-native store exists and is ingested, so blocks below the
  -- confirmed seq are safe to drop.
  -- 'terminal': no ingestion path — a sandbox whose store died with it — so this
  -- record is the only copy and nothing is ever dropped.
  authority     TEXT NOT NULL,
  confirmed_seq INTEGER,
  checked_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
