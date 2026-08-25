-- How much record each session holds, and how much it held last time.
--
-- Retention's counts answer "did anything get dropped". They cannot answer the
-- question an operator actually has when a disk fills: *which* session is
-- responsible. A terminal session is supposed to keep everything forever, so
-- "withheld: 41" is both the correct report and completely silent about the one
-- sandbox session that has grown to a gigabyte while forty others sit at a few
-- kilobytes. Counting sessions cannot distinguish those, and neither can counting
-- blocks — one 200 KB tool result and one 20-byte status are both one block.
--
-- # Why a table rather than a query
--
-- The size is a `SUM` over `blocks`, so *current* size needs no storage. Growth
-- does: it is a difference between two observations, and without a recorded
-- earlier one the only available answer is "large", which does not distinguish a
-- session that reached its size months ago and stopped from one adding a hundred
-- megabytes an hour. Those need opposite responses, and the second is the whole
-- reason this exists.
--
-- # Why not on `watermarks`
--
-- A watermark records what an *authority confirmed*, and it is deliberately not
-- written when the archive cannot be reached — "cannot ask" must never be stored
-- as "asked, and nothing is safe". Bytes held are a local fact that needs no
-- authority and is true whether or not TraceDB answers. Putting them on that row
-- would have made byte reporting empty on exactly the installs where growth is
-- unbounded and the report matters most: one with no archive, where nothing can
-- ever be pruned.
CREATE TABLE IF NOT EXISTS session_bytes (
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
CREATE INDEX IF NOT EXISTS session_bytes_by_size ON session_bytes(bytes DESC);
