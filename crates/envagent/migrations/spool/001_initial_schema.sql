-- Disposable spool schema, checked on every open. No compatibility path.
CREATE TABLE spool_events (
  producer_id TEXT NOT NULL,
  producer_seq INTEGER NOT NULL,
  kind TEXT NOT NULL DEFAULT 'event',
  session_id TEXT NOT NULL,
  arrival_turn_id TEXT,
  payload BLOB NOT NULL,
  source_cursor TEXT,
  recorded_at_ms INTEGER,
  PRIMARY KEY (producer_id, producer_seq)
);
-- The primary key already indexes producer order.
CREATE TABLE spool_usage (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  bytes INTEGER NOT NULL CHECK(bytes >= 0)
);
INSERT INTO spool_usage VALUES(1, 0);
CREATE TRIGGER spool_usage_insert AFTER INSERT ON spool_events BEGIN
  UPDATE spool_usage SET bytes = bytes + length(NEW.payload) WHERE singleton = 1;
END;
CREATE TRIGGER spool_usage_delete AFTER DELETE ON spool_events BEGIN
  UPDATE spool_usage SET bytes = bytes - length(OLD.payload) WHERE singleton = 1;
END;
CREATE TABLE spool_acks (
  producer_id TEXT PRIMARY KEY,
  acked_through INTEGER NOT NULL DEFAULT -1
);
CREATE TABLE spool_sequences (
  producer_id TEXT PRIMARY KEY,
  next_seq INTEGER NOT NULL DEFAULT 1
);
