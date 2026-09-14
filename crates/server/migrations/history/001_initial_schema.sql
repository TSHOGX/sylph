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
--
-- Like the journal, this is the whole schema in one file rather than a chain
-- (`docs/database-schema.md` § Schema evolution), and for the same reason: an order
-- of discovery is only worth preserving while some database is stranded partway
-- along it. Editing it makes an already-migrated `history.db` refuse to open, which
-- is the intended loud failure — re-projection from the native store is what
-- makes accepting it cheap.

CREATE TABLE blocks (
  id           INTEGER PRIMARY KEY,
  session_id   TEXT NOT NULL,
  -- The completed event's sequence number, which orders the whole session stream.
  seq          INTEGER NOT NULL,
  turn_id      TEXT NOT NULL,
  kind         TEXT NOT NULL,
  phase        TEXT,
  stream_id    TEXT,
  -- The delegated trajectory this block ran in; NULL is the root turn. See
  -- `docs/session-protocol.md` § Delegated trajectories. Carried here and not
  -- only in the journal because this tier is the durable record: without it,
  -- consolidating a session would discard the shape of every delegation and
  -- replay would reconstruct a workflow's parallel phases as one flat sequence.
  span         TEXT,
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
  text_preview TEXT NOT NULL,

  -- Delta reconstruction: packed little-endian u32 arrays. NULL when the block
  -- never streamed — a tool result arrives complete.
  delta_offsets BLOB,
  delta_timings BLOB,

  data_json    BLOB,
  data_hash    TEXT,
  data_len     INTEGER NOT NULL DEFAULT 0,
  data_encoding TEXT NOT NULL DEFAULT 'raw',
  data_preview_json TEXT,
  body_truncated INTEGER NOT NULL DEFAULT 0,
  -- Stored separately because data_json can be compressed or externalized.
  reconciled   INTEGER NOT NULL DEFAULT 0,

  is_error     INTEGER NOT NULL DEFAULT 0,
  source       TEXT NOT NULL DEFAULT 'agent',
  -- The latest producer-authored interior state for this trajectory, carried through
  -- consolidation. Folding a session must not lose it: the frontend's inference from
  -- observed children cannot run against a tier that no longer holds the children.
  interior_state TEXT,
  -- When the block's first delta arrived. Without it the timing array is relative
  -- to nothing and a replay cannot be paced.
  started_at_ms INTEGER NOT NULL,
  started_recorded_at_ms INTEGER,
  -- The same two clocks the journal's events carry, preserved rather than collapsed:
  -- `recorded_at_ms` is the vendor's own and is NULL when it supplied nothing usable,
  -- `arrival_at_ms` is the server's receipt clock and always present.
  arrival_at_ms INTEGER NOT NULL,
  recorded_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,

  UNIQUE (session_id, seq)
);

-- The integer key is also the FTS rowid. Keep it stable on upsert so maintaining
-- search never scans the UNINDEXED session/sequence columns.
CREATE INDEX blocks_by_turn ON blocks(turn_id, seq);

-- Which blocks reference which blobs.
--
-- Reachability lives here rather than in a count on the blob, because a blob
-- shared by a pruned block and a retained one must survive. A bare refcount
-- cannot express which references are still live after a partial prune.
CREATE TABLE cas_refs (
  hash       TEXT NOT NULL,
  session_id TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  PRIMARY KEY (hash, session_id, seq)
) WITHOUT ROWID;

CREATE INDEX cas_refs_by_block ON cas_refs(session_id, seq);

-- A durable queue makes an idle GC pass independent of the total live catalogue.
-- Queue before forgetting/replacing a block. The reference rows remain until GC
-- has rechecked both databases and successfully removed any unreferenced object.
CREATE TABLE cas_gc_candidates (hash TEXT PRIMARY KEY) WITHOUT ROWID;
CREATE TRIGGER blocks_gc_delete AFTER DELETE ON blocks BEGIN
  INSERT OR IGNORE INTO cas_gc_candidates SELECT OLD.text_hash WHERE OLD.text_hash IS NOT NULL;
  INSERT OR IGNORE INTO cas_gc_candidates SELECT OLD.data_hash WHERE OLD.data_hash IS NOT NULL;
END;
CREATE TRIGGER blocks_gc_update AFTER UPDATE OF text_hash, data_hash ON blocks BEGIN
  INSERT OR IGNORE INTO cas_gc_candidates SELECT OLD.text_hash
    WHERE OLD.text_hash IS NOT NULL AND OLD.text_hash IS NOT NEW.text_hash;
  INSERT OR IGNORE INTO cas_gc_candidates SELECT OLD.data_hash
    WHERE OLD.data_hash IS NOT NULL AND OLD.data_hash IS NOT NEW.data_hash;
END;

-- How far a downstream authority has confirmed each session.
--
-- Retention asks exactly one question — what is safe to drop — and never asks how
-- old anything is. A time rule deletes data because a clock advanced, which is
-- precisely the case where it may not exist anywhere else yet.
--
-- The only downstream copy is the **vendor's own native store**, so a confirmation can
-- only come from the environment holding it (`docs/storage.md` § Two retention classes).
CREATE TABLE watermarks (
  session_id    TEXT PRIMARY KEY,
  -- 'envagent': the environment holding this session's native store confirmed how far
  -- that store extends, so blocks below the confirmed seq are safe to drop.
  -- 'terminal': no reachable native source — a sandbox whose store died with it — so this
  -- record is the only copy and nothing is ever dropped.
  authority     TEXT NOT NULL,
  confirmed_seq INTEGER,
  checked_at_ms INTEGER NOT NULL,
  -- What the environment reported about the native source, for the audit row:
  -- 'present' | 'absent' | 'unreachable'.
  source        TEXT,
  -- Why nothing was confirmed, when nothing was; NULL when something was.
  --
  -- Load-bearing, because **"cannot ask" must never be recorded as "asked, and nothing
  -- is safe".** A watermark row is a claim that a check happened, and inferring
  -- withholding from `confirmed_seq IS NULL` cannot distinguish "the environment
  -- answered, and nothing is safe yet" from "no envagent was connected, so nobody was
  -- asked" — which call for opposite responses.
  withheld      TEXT
  -- Both nullable: a terminal session has no native source to describe and nothing to
  -- withhold for.
) WITHOUT ROWID;

-- How much record each session holds, and how much it held last time.
--
-- Retention's counts answer "did anything get dropped". They cannot answer the
-- question an operator actually has when a disk fills: *which* session is
-- responsible. A terminal session is supposed to keep everything forever, so
-- a withheld count is both the correct report and completely silent about the one
-- sandbox session that has grown to a gigabyte while forty others sit at a few
-- kilobytes. Counting sessions cannot distinguish those, and neither can counting
-- blocks — one 200 KB tool result and one 20-byte status are both one block.
--
-- # Why a table rather than a query
--
-- Current size is maintained by session_usage triggers. Growth is a difference
-- between two observations, and without a recorded
-- earlier one the only available answer is "large", which does not distinguish a
-- session that reached its size months ago and stopped from one adding a hundred
-- megabytes an hour. Those need opposite responses, and the second is the whole
-- reason this exists.
--
-- # Why not on `watermarks`
--
-- A watermark records what an *authority confirmed*, and is deliberately not written
-- when no environment can be reached. Bytes held are a local fact needing no authority,
-- true whether or not anything answers. Putting them on that row would make byte
-- reporting empty on exactly the install where growth is unbounded and the report
-- matters most: one with no environment paired, where nothing can ever be pruned.
CREATE TABLE session_bytes (
  session_id     TEXT PRIMARY KEY,
  -- Bytes of record currently held. See `HistoryStore::measure_session_bytes`
  -- for what is and is not counted, which is stated there rather than here
  -- because the SQL is the definition.
  bytes          INTEGER NOT NULL,
  blocks         INTEGER NOT NULL,
  measured_at_ms INTEGER NOT NULL,
  -- The previous observation, carried forward so growth is readable from one row
  -- without a history table. A full time series would answer more questions and
  -- costs unbounded rows to answer a question nobody asked: the actionable
  -- signal is "this session is growing now", not its shape over a month.
  previous_bytes          INTEGER,
  previous_measured_at_ms INTEGER
) WITHOUT ROWID;

-- The report reads "largest first" and "fastest growing first", both over the
-- whole table, so the ordering column is worth an index once a store holds
-- thousands of sessions.
CREATE INDEX session_bytes_by_size ON session_bytes(bytes DESC);

-- Cross-session full-text search over the same rows the timeline renders, so coverage is
-- a contract rather than a function of what some parser reached
-- (`docs/database-schema.md` § Search lives beside `history.db`).
--
-- **It cannot be an external-content FTS5 table**, which is the obvious shape and the
-- wrong one here. External content re-reads the column from the base table at query
-- time, and `blocks.text_inline` is not text: `text_encoding` says it may be `zstd`,
-- and when the text is externalized to CAS the column is **NULL** and the bytes live
-- under `blobs/`. So an external-content index would index compressed bytes for some
-- rows and nothing at all for the rows most worth finding — the large ones.
-- Consolidation therefore inserts the *decoded* text, which costs a second copy of
-- it and is the price of the tier split (`docs/storage.md` § The index is standalone).
--
-- `session_id` and `seq` ride along `UNINDEXED` so a hit resolves to a block without
-- a join when rendering hits. Mutations always seek by blocks.id / FTS rowid.
--
-- **The tokenizer is `jieba`, because `unicode61` cannot search Chinese.** `unicode61`
-- splits on non-alphanumerics, so an entire run of Han characters becomes one token and
-- a query for any substring of it matches nothing — and this project's own transcripts
-- are substantially Chinese. `crates/fts5-jieba` segments CJK, folds diacritics, and
-- stems English, so one tokenizer serves both languages (`docs/storage.md`
-- § Tokenization).
--
-- **It is linked into the binary rather than loaded as an extension**, so there is no
-- per-platform `.dylib` to install and no "extension missing" state to degrade into.
-- That matters more than convenience: an index built with one tokenizer and queried with
-- another silently returns nothing, so a fallback would answer *wrongly* rather than
-- worse. Registration happens per connection in `HistoryStore::open`, and a failure
-- there is fatal.
CREATE VIRTUAL TABLE blocks_fts USING fts5(
  text,
  session_id UNINDEXED,
  seq        UNINDEXED,
  tokenize = 'jieba'
);

-- Transactional counters make accounting proportional to sessions, not record bytes.
CREATE TABLE session_usage (
  session_id TEXT PRIMARY KEY,
  bytes INTEGER NOT NULL CHECK (bytes >= 0),
  blocks INTEGER NOT NULL CHECK (blocks >= 0)
) WITHOUT ROWID;

CREATE TRIGGER blocks_usage_insert AFTER INSERT ON blocks BEGIN
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, NEW.text_len + NEW.data_len, 1)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
CREATE TRIGGER blocks_usage_delete AFTER DELETE ON blocks BEGIN
  UPDATE session_usage SET bytes = bytes - (OLD.text_len + OLD.data_len), blocks = blocks - 1 WHERE session_id = OLD.session_id;
END;
CREATE TRIGGER blocks_usage_update AFTER UPDATE ON blocks BEGIN
  UPDATE session_usage SET bytes = bytes - (OLD.text_len + OLD.data_len), blocks = blocks - 1 WHERE session_id = OLD.session_id;
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, NEW.text_len + NEW.data_len, 1)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
