-- Durable capture is independent of semantic interpretation. All foreign keys
-- below stay in this FULL-durable evidence database.
CREATE TABLE ingestion_scopes (
  scope_ref TEXT PRIMARY KEY,
  issuer_ref TEXT NOT NULL,
  current_epoch TEXT NOT NULL,
  suppressed INTEGER NOT NULL CHECK (suppressed IN (0, 1))
) WITHOUT ROWID;

CREATE TABLE ingestion_epochs (
  epoch TEXT PRIMARY KEY,
  scope_ref TEXT NOT NULL REFERENCES ingestion_scopes(scope_ref),
  opened_at_ms INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE inventory_subjects (
  epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  subject_ref TEXT NOT NULL,
  PRIMARY KEY (epoch, subject_ref)
) WITHOUT ROWID;

CREATE TABLE deletion_tombstones (
  tombstone_id TEXT PRIMARY KEY,
  epoch TEXT NOT NULL UNIQUE REFERENCES ingestion_epochs(epoch),
  scope_ref TEXT NOT NULL REFERENCES ingestion_scopes(scope_ref),
  deleted_at_ms INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE sources (
  source_ref TEXT PRIMARY KEY,
  issuer_ref TEXT NOT NULL,
  source_instance TEXT NOT NULL,
  UNIQUE (issuer_ref, source_instance)
) WITHOUT ROWID;

CREATE TABLE source_generations (
  source_ref TEXT NOT NULL REFERENCES sources(source_ref),
  generation INTEGER NOT NULL CHECK (generation >= 0),
  PRIMARY KEY (source_ref, generation)
) WITHOUT ROWID;

-- A staged payload is not an admitted observation. Chunk and manifest durability
-- precedes every receipt; unrooted staged objects remain identifiable for GC.
CREATE TABLE payloads (
  payload_ref TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('bytes', 'manifest')),
  stored_length INTEGER NOT NULL CHECK (stored_length >= 0),
  decoded_length INTEGER CHECK (decoded_length >= 0),
  decoded_digest TEXT,
  complete INTEGER NOT NULL CHECK (complete IN (0, 1)),
  manifest_json TEXT,
  staged_at_ms INTEGER NOT NULL,
  CHECK ((kind = 'manifest') = (manifest_json IS NOT NULL)),
  CHECK (complete = 0 OR (decoded_length IS NOT NULL AND decoded_digest IS NOT NULL))
) WITHOUT ROWID;

CREATE TABLE payload_chunks (
  manifest_ref TEXT NOT NULL REFERENCES payloads(payload_ref),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  chunk_ref TEXT NOT NULL REFERENCES payloads(payload_ref),
  chunk_offset INTEGER NOT NULL CHECK (chunk_offset >= 0),
  length INTEGER NOT NULL CHECK (length >= 0),
  payload_offset INTEGER NOT NULL CHECK (payload_offset >= 0),
  PRIMARY KEY (manifest_ref, ordinal)
) WITHOUT ROWID;
CREATE INDEX payload_chunks_by_chunk ON payload_chunks(chunk_ref, manifest_ref);
CREATE INDEX payload_chunks_by_range ON payload_chunks(manifest_ref, payload_offset);

CREATE TABLE occurrences (
  occurrence_id INTEGER PRIMARY KEY,
  ingestion_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  source_ref TEXT NOT NULL,
  generation INTEGER NOT NULL,
  locator_json TEXT NOT NULL,
  payload_ref TEXT NOT NULL REFERENCES payloads(payload_ref),
  decoded_digest TEXT NOT NULL,
  canonical_observation_ref TEXT NOT NULL,
  UNIQUE (ingestion_epoch, source_ref, generation, locator_json),
  FOREIGN KEY (source_ref, generation) REFERENCES source_generations(source_ref, generation)
);

CREATE TABLE observations (
  admission_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  observation_ref TEXT NOT NULL UNIQUE,
  occurrence_id INTEGER NOT NULL REFERENCES occurrences(occurrence_id),
  envelope_json TEXT NOT NULL,
  envelope_bytes INTEGER NOT NULL CHECK (envelope_bytes >= 0),
  admitted_at_ms INTEGER NOT NULL
);
CREATE INDEX observations_by_occurrence ON observations(occurrence_id, admission_seq);

CREATE TABLE observation_payload_roots (
  admission_seq INTEGER NOT NULL REFERENCES observations(admission_seq),
  payload_ref TEXT NOT NULL REFERENCES payloads(payload_ref),
  PRIMARY KEY (admission_seq, payload_ref)
) WITHOUT ROWID;

CREATE TABLE producer_prefixes (
  issuer_ref TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  accepted_through INTEGER NOT NULL CHECK (accepted_through >= 0),
  PRIMARY KEY (issuer_ref, producer_id, producer_epoch)
) WITHOUT ROWID;

-- Receipt identity stays global across deletion and re-adoption. Deletion can
-- release a spool prefix without claiming that its bytes entered the archive.
CREATE TABLE producer_receipts (
  observation_ref TEXT PRIMARY KEY,
  issuer_ref TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_epoch TEXT NOT NULL,
  producer_seq INTEGER NOT NULL CHECK (producer_seq > 0),
  envelope_digest TEXT NOT NULL,
  disposition TEXT NOT NULL CHECK (disposition IN ('retained', 'discarded_by_deletion')),
  admission_seq INTEGER REFERENCES observations(admission_seq),
  tombstone_id TEXT REFERENCES deletion_tombstones(tombstone_id),
  UNIQUE (issuer_ref, producer_id, producer_epoch, producer_seq),
  CHECK ((disposition = 'retained') = (admission_seq IS NOT NULL)),
  CHECK ((disposition = 'discarded_by_deletion') = (tombstone_id IS NOT NULL))
) WITHOUT ROWID;
