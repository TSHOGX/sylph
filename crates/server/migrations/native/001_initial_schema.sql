-- Lossless native retention has its own writer, independent of history and FTS.
-- Complete vendor-native records used to re-project history after an adapter change.
-- Unlike folded blocks these are never pruned by event watermarks: the native source is
-- the only copy from which an ignored field can be recovered, so their retention class is
-- explicitly `raw`.
CREATE TABLE native_records (
  session_id     TEXT NOT NULL,
  source         TEXT NOT NULL,
  -- Revisions are opaque u64 bit patterns, stored by reversible integer cast.
  version        INTEGER NOT NULL,
  retained_seq   INTEGER NOT NULL,
  payload_json   BLOB NOT NULL,
  created_at_ms  INTEGER NOT NULL,
  retention_class TEXT NOT NULL DEFAULT 'raw' CHECK (retention_class = 'raw'),
  PRIMARY KEY (session_id, source, version),
  UNIQUE (session_id, retained_seq)
) WITHOUT ROWID;

CREATE TABLE native_blobs (
  session_id TEXT NOT NULL,
  source     TEXT NOT NULL,
  version    INTEGER NOT NULL,
  name       TEXT NOT NULL,
  hash       TEXT,
  size       INTEGER NOT NULL,
  error      TEXT,
  PRIMARY KEY (session_id, source, version, name),
  FOREIGN KEY (session_id, source, version)
    REFERENCES native_records(session_id, source, version) ON DELETE CASCADE
) WITHOUT ROWID;

-- Bounded sidecar chunks staged by the envagent before the complete native record
-- arrives. Chunks are retained durably and folded into CAS atomically with that record.
CREATE TABLE native_blob_chunks (
  session_id TEXT NOT NULL,
  source TEXT NOT NULL,
  version INTEGER NOT NULL,
  name TEXT NOT NULL,
  size INTEGER NOT NULL,
  offset INTEGER NOT NULL,
  data BLOB NOT NULL,
  PRIMARY KEY (session_id, source, version, name, offset)
) WITHOUT ROWID;

-- The UNIQUE(session_id, retained_seq) index already serves ordered replay.
CREATE INDEX native_blobs_by_hash ON native_blobs(hash) WHERE hash IS NOT NULL;


-- Transactional counters make accounting proportional to sessions, not record bytes.
CREATE TABLE session_usage (
  session_id TEXT PRIMARY KEY,
  bytes INTEGER NOT NULL CHECK (bytes >= 0),
  blocks INTEGER NOT NULL CHECK (blocks >= 0)
) WITHOUT ROWID;

CREATE TRIGGER native_records_usage_insert AFTER INSERT ON native_records BEGIN
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, length(NEW.payload_json), 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
CREATE TRIGGER native_records_usage_delete AFTER DELETE ON native_records BEGIN
  UPDATE session_usage SET bytes = bytes - (length(OLD.payload_json)), blocks = blocks - 0 WHERE session_id = OLD.session_id;
END;
CREATE TRIGGER native_records_usage_update AFTER UPDATE ON native_records BEGIN
  UPDATE session_usage SET bytes = bytes - (length(OLD.payload_json)), blocks = blocks - 0 WHERE session_id = OLD.session_id;
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, length(NEW.payload_json), 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;

CREATE TRIGGER native_blobs_usage_insert AFTER INSERT ON native_blobs BEGIN
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, CASE WHEN NEW.hash IS NOT NULL THEN NEW.size ELSE 0 END, 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
CREATE TRIGGER native_blobs_usage_delete AFTER DELETE ON native_blobs BEGIN
  UPDATE session_usage SET bytes = bytes - (CASE WHEN OLD.hash IS NOT NULL THEN OLD.size ELSE 0 END), blocks = blocks - 0 WHERE session_id = OLD.session_id;
END;
CREATE TRIGGER native_blobs_usage_update AFTER UPDATE ON native_blobs BEGIN
  UPDATE session_usage SET bytes = bytes - (CASE WHEN OLD.hash IS NOT NULL THEN OLD.size ELSE 0 END), blocks = blocks - 0 WHERE session_id = OLD.session_id;
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, CASE WHEN NEW.hash IS NOT NULL THEN NEW.size ELSE 0 END, 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;

CREATE TRIGGER native_blob_chunks_usage_insert AFTER INSERT ON native_blob_chunks BEGIN
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, length(NEW.data), 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
CREATE TRIGGER native_blob_chunks_usage_delete AFTER DELETE ON native_blob_chunks BEGIN
  UPDATE session_usage SET bytes = bytes - (length(OLD.data)), blocks = blocks - 0 WHERE session_id = OLD.session_id;
END;
CREATE TRIGGER native_blob_chunks_usage_update AFTER UPDATE ON native_blob_chunks BEGIN
  UPDATE session_usage SET bytes = bytes - (length(OLD.data)), blocks = blocks - 0 WHERE session_id = OLD.session_id;
  INSERT INTO session_usage(session_id, bytes, blocks) VALUES(NEW.session_id, length(NEW.data), 0)
  ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, blocks = blocks + excluded.blocks;
END;
