-- Durable capture registry and its ack-gated lexical queue share one writer.
CREATE TABLE spool_usage(singleton INTEGER PRIMARY KEY CHECK(singleton=1),bytes INTEGER NOT NULL CHECK(bytes>=0));
INSERT INTO spool_usage VALUES(1,0);

-- Source progress and queued captures share this writer. No complete cursor can
-- advance without the exact lexical payload and its fixed authorization epoch.
CREATE TABLE capture_sources (
  source_ref TEXT PRIMARY KEY,
  issuer_json TEXT NOT NULL,
  namespace TEXT NOT NULL,
  locator TEXT NOT NULL,
  generation INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL DEFAULT 0,
  absent INTEGER NOT NULL DEFAULT 0 CHECK(absent IN (0,1)),
  UNIQUE(issuer_json, namespace, locator)
);
CREATE TABLE capture_generations (
  source_ref TEXT NOT NULL REFERENCES capture_sources(source_ref),
  generation INTEGER NOT NULL,
  predecessor INTEGER,
  reason TEXT NOT NULL,
  file_identity TEXT,
  complete_offset INTEGER NOT NULL DEFAULT 0,
  next_ordinal INTEGER NOT NULL DEFAULT 0,
  prefix_digest TEXT NOT NULL,
  PRIMARY KEY(source_ref,generation)
);
CREATE TABLE capture_epoch_progress (
  source_ref TEXT NOT NULL,
  generation INTEGER NOT NULL,
  ingestion_epoch TEXT NOT NULL,
  complete_offset INTEGER NOT NULL DEFAULT 0,
  next_ordinal INTEGER NOT NULL DEFAULT 0,
  tail BLOB NOT NULL DEFAULT X'',
  PRIMARY KEY(source_ref,generation,ingestion_epoch),
  FOREIGN KEY(source_ref,generation) REFERENCES capture_generations(source_ref,generation)
);
CREATE TABLE capture_producers (
  issuer_json TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  next_seq INTEGER NOT NULL DEFAULT 1,
  acked_through INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(issuer_json,producer_id,producer_epoch)
);
CREATE INDEX capture_pending_tails ON capture_epoch_progress(source_ref,generation,ingestion_epoch)
  WHERE length(tail)>0;
CREATE TABLE capture_queue (
  issuer_json TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  producer_seq INTEGER NOT NULL,
  source_ref TEXT NOT NULL,
  generation INTEGER NOT NULL,
  ingestion_epoch TEXT NOT NULL,
  position_key TEXT NOT NULL,
  capture_json BLOB NOT NULL,
  manifest_json BLOB,
  encoded_bytes INTEGER NOT NULL,
  metadata_bytes INTEGER NOT NULL,
  chunk_count INTEGER NOT NULL,
  PRIMARY KEY(issuer_json,producer_id,producer_epoch,producer_seq),
  UNIQUE(ingestion_epoch,source_ref,generation,position_key),
  FOREIGN KEY(issuer_json,producer_id,producer_epoch)
    REFERENCES capture_producers(issuer_json,producer_id,producer_epoch)
);
CREATE TABLE capture_payload_chunks (
  issuer_json TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  producer_seq INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  digest TEXT NOT NULL,
  bytes BLOB NOT NULL,
  PRIMARY KEY(issuer_json,producer_id,producer_epoch,producer_seq,ordinal),
  FOREIGN KEY(issuer_json,producer_id,producer_epoch,producer_seq)
    REFERENCES capture_queue(issuer_json,producer_id,producer_epoch,producer_seq) ON DELETE CASCADE
);
CREATE TRIGGER capture_queue_usage_insert AFTER INSERT ON capture_queue BEGIN
  UPDATE spool_usage SET bytes = bytes + NEW.encoded_bytes WHERE singleton = 1;
END;
CREATE TRIGGER capture_queue_usage_delete AFTER DELETE ON capture_queue BEGIN
  UPDATE spool_usage SET bytes = bytes - OLD.encoded_bytes WHERE singleton = 1;
END;
CREATE TRIGGER capture_tail_usage_insert AFTER INSERT ON capture_epoch_progress BEGIN
  UPDATE spool_usage SET bytes = bytes + length(NEW.tail) WHERE singleton = 1;
END;
CREATE TRIGGER capture_tail_usage_update AFTER UPDATE OF tail ON capture_epoch_progress BEGIN
  UPDATE spool_usage SET bytes = bytes + length(NEW.tail) - length(OLD.tail) WHERE singleton = 1;
END;
CREATE TRIGGER capture_tail_usage_delete AFTER DELETE ON capture_epoch_progress BEGIN
  UPDATE spool_usage SET bytes = bytes - length(OLD.tail) WHERE singleton = 1;
END;
CREATE TABLE capture_receipts (
  issuer_json TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  producer_seq INTEGER NOT NULL,
  receipt_json TEXT NOT NULL,
  PRIMARY KEY(issuer_json,producer_id,producer_epoch,producer_seq)
);

CREATE TABLE capture_source_proofs(source_ref TEXT NOT NULL,generation INTEGER NOT NULL,observation_json TEXT NOT NULL,PRIMARY KEY(source_ref,generation));

-- Operational command deduplication is independent of lexical capture receipt identity.
-- A NULL outcome records an attempt that may already have reached the vendor.
CREATE TABLE command_attempts (
  request_id TEXT PRIMARY KEY,
  fingerprint TEXT NOT NULL,
  outcome_json TEXT
);

CREATE TABLE capture_snapshot_revisions(source_ref TEXT NOT NULL,generation INTEGER NOT NULL,ingestion_epoch TEXT NOT NULL,capture_json TEXT NOT NULL,PRIMARY KEY(source_ref,generation,ingestion_epoch));
CREATE TABLE capture_attachment_scans(observation_key TEXT PRIMARY KEY);
CREATE TABLE capture_attachment_watches(parent_key TEXT NOT NULL,locator TEXT NOT NULL,parent_slot TEXT NOT NULL,watch_json TEXT NOT NULL,last_state TEXT,last_checked INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(parent_key,locator,parent_slot));

CREATE TABLE native_source_inventory(session_key TEXT NOT NULL,ingestion_epoch TEXT NOT NULL,locator TEXT NOT NULL,source_json TEXT NOT NULL,PRIMARY KEY(session_key,ingestion_epoch,locator));
CREATE TABLE captured_source_states(session_key TEXT NOT NULL,source_key TEXT NOT NULL,ingestion_epoch TEXT NOT NULL,state_json TEXT NOT NULL,authorization_json TEXT NOT NULL,PRIMARY KEY(session_key,source_key,ingestion_epoch));
