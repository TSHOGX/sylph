-- Widen the event vocabulary to `sylph.agent.v2`.
--
-- Two CHECK constraints in 001 predate the protocol and silently rejected every
-- adapter event: `source` allowed 'runner' but not 'agent', and `kind` had no
-- 'turn' — which is the event that *ends* a turn. The failure mode was the worst
-- kind: `append_event` logged and continued, so a sequence number was consumed,
-- the event was published to live clients, and nothing reached the journal. A
-- turn looked correct while streaming and came back empty on reload.
--
-- SQLite cannot alter a CHECK constraint, so the table is rebuilt. Safe here
-- because no deployment carries real data yet (AGENTS.md, "Compatibility"); once
-- one does, this is the shape a real migration would take anyway.

CREATE TABLE events_v2 (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  -- Open-ended on purpose. The protocol says new kinds are additive, so a CHECK
  -- listing them is a promise to break on the next adapter that adds one — which
  -- is exactly what happened. Validation belongs at the wire boundary, where an
  -- unknown kind can be reported, not at the storage layer where it can only be
  -- dropped.
  kind TEXT NOT NULL,
  phase TEXT,
  stream_id TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'stable' CHECK (stability IN ('ephemeral', 'stable')),
  -- 'agent' replaces 'runner'. Kept as a closed set because it has exactly three
  -- producers and a fourth would be a design change, not an adapter's choice.
  source TEXT NOT NULL DEFAULT 'server' CHECK (source IN ('agent', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL,
  reconciled_at_ms INTEGER,
  UNIQUE(session_id, seq)
);

INSERT INTO events_v2 (
  id, session_id, turn_id, seq, kind, phase, stream_id, name, text,
  data_json, is_error, stability, source, created_at_ms, reconciled_at_ms
)
SELECT
  id, session_id, turn_id, seq, kind, phase, stream_id, name, text,
  data_json, is_error, stability,
  -- Existing rows were written by v1 runners.
  CASE source WHEN 'runner' THEN 'agent' ELSE source END,
  created_at_ms, reconciled_at_ms
FROM events;

DROP TABLE events;
ALTER TABLE events_v2 RENAME TO events;

CREATE INDEX events_session_idx ON events(session_id, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;

UPDATE app_meta SET value = '3' WHERE key = 'schema_version';
