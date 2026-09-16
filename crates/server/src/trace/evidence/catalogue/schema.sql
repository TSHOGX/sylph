-- The publication catalogue shares evidence's FULL writer and authority.
CREATE TABLE trace_catalogue_clock (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  visibility_revision INTEGER NOT NULL CHECK(visibility_revision>=0)
);
INSERT INTO trace_catalogue_clock VALUES(1,0);

-- This key is private server state, never a payload, trace fact, or export row.
CREATE TABLE trace_cursor_secret (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  key_bytes BLOB NOT NULL CHECK(typeof(key_bytes)='blob' AND length(key_bytes)=32)
);

CREATE TABLE trace_generations (
  generation TEXT PRIMARY KEY,
  manifest_json TEXT NOT NULL,
  relative_file TEXT NOT NULL UNIQUE,
  database_identity TEXT,
  state TEXT NOT NULL CHECK(state IN ('building','ready','active','retained','cancelled','retiring','retired')),
  initial_cut INTEGER,
  ready_id TEXT,
  created_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE trace_active (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  generation TEXT REFERENCES trace_generations(generation),
  pointer_serial INTEGER NOT NULL CHECK(pointer_serial>=0)
);
INSERT INTO trace_active VALUES(1,NULL,0);
CREATE TABLE trace_publication_guard (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  job_id TEXT,
  guard_token TEXT
);
INSERT INTO trace_publication_guard VALUES(1,NULL,NULL);

CREATE TABLE trace_build_jobs (
  job_id TEXT PRIMARY KEY,
  generation TEXT NOT NULL UNIQUE REFERENCES trace_generations(generation),
  expected_active TEXT,
  expected_pointer_serial INTEGER NOT NULL,
  base_cut INTEGER NOT NULL,
  visibility_revision INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('building','ready','published','cancelled','failed')),
  error TEXT,
  ready_id TEXT,
  created_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX trace_build_jobs_state ON trace_build_jobs(state,created_at_ms,job_id);
CREATE TABLE trace_reprojection_jobs (
  job_id TEXT PRIMARY KEY,
  generation TEXT NOT NULL UNIQUE,
  requested_by TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('queued','running','cancelling','completed','cancelled','failed')),
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  finished_at_ms INTEGER,
  projected_cut INTEGER NOT NULL DEFAULT 0,
  snapshot_json TEXT,
  error_code TEXT
) WITHOUT ROWID;
CREATE UNIQUE INDEX trace_reprojection_one_active ON trace_reprojection_jobs((1))
  WHERE state IN ('queued','running','cancelling');
CREATE INDEX trace_reprojection_recent ON trace_reprojection_jobs(created_at_ms DESC,job_id DESC);
CREATE TABLE trace_ready_manifests (
  ready_id TEXT PRIMARY KEY,
  generation TEXT NOT NULL REFERENCES trace_generations(generation),
  job_id TEXT NOT NULL REFERENCES trace_build_jobs(job_id),
  ready_json TEXT NOT NULL,
  sealed_cut INTEGER NOT NULL,
  view_revision INTEGER NOT NULL,
  database_identity TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX trace_seals_by_generation ON trace_ready_manifests(generation,sealed_cut,view_revision);

-- Deletion invalidates matching epoch proofs without an unbounded mass UPDATE.
CREATE TABLE trace_scope_deletions (
  visibility_revision INTEGER PRIMARY KEY,
  scope_ref TEXT NOT NULL,
  ingestion_epoch TEXT NOT NULL,
  tombstone_id TEXT NOT NULL REFERENCES deletion_tombstones(tombstone_id),
  deleted_at_ms INTEGER NOT NULL
);
CREATE INDEX trace_deleted_scope_epoch ON trace_scope_deletions(scope_ref,ingestion_epoch);
CREATE TABLE trace_generation_scopes (
  generation TEXT NOT NULL REFERENCES trace_generations(generation),
  scope_ref TEXT NOT NULL REFERENCES ingestion_scopes(scope_ref),
  ingestion_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  first_evidence_cut INTEGER NOT NULL,
  PRIMARY KEY(generation,scope_ref,ingestion_epoch)
) WITHOUT ROWID;
CREATE INDEX trace_generations_by_scope ON trace_generation_scopes(scope_ref,ingestion_epoch,generation);

CREATE TABLE trace_view_leases (
  lease_id TEXT PRIMARY KEY,
  subject TEXT NOT NULL,
  generation TEXT NOT NULL REFERENCES trace_generations(generation),
  pending_nonce TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','active','cancelled','expired')),
  evidence_cut INTEGER,
  view_revision INTEGER,
  visibility_revision INTEGER NOT NULL,
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER NOT NULL,
  cancel_reason TEXT,
  CHECK(expires_at_ms>created_at_ms),
  CHECK((state!='active') OR (evidence_cut IS NOT NULL AND view_revision IS NOT NULL))
) WITHOUT ROWID;
CREATE INDEX trace_lease_generation_expiry ON trace_view_leases(generation,state,expires_at_ms);
CREATE INDEX trace_lease_subject_expiry ON trace_view_leases(subject,state,expires_at_ms);
CREATE INDEX trace_lease_state_expiry ON trace_view_leases(state,expires_at_ms);
CREATE TABLE trace_lease_scopes (
  lease_id TEXT NOT NULL REFERENCES trace_view_leases(lease_id),
  scope_ref TEXT NOT NULL,
  ingestion_epoch TEXT NOT NULL,
  PRIMARY KEY(lease_id,scope_ref,ingestion_epoch)
) WITHOUT ROWID;
CREATE INDEX trace_leases_by_scope ON trace_lease_scopes(scope_ref,ingestion_epoch,lease_id);

-- Search/export work pins semantic membership as well as payload bytes. Reader
-- leases are independent roots, so replacing an index pin preserves result jobs.
CREATE TABLE trace_revision_pins (
  owner_kind TEXT NOT NULL CHECK(owner_kind IN ('search','export')),
  owner_id TEXT NOT NULL,
  generation TEXT NOT NULL REFERENCES trace_generations(generation),
  evidence_cut INTEGER NOT NULL CHECK(evidence_cut>=0),
  view_revision INTEGER NOT NULL CHECK(view_revision>=0),
  PRIMARY KEY(owner_kind,owner_id)
) WITHOUT ROWID;
CREATE INDEX trace_revision_pins_by_generation ON trace_revision_pins(generation,view_revision);

CREATE TABLE trace_payload_pins (
  owner_kind TEXT NOT NULL CHECK(owner_kind IN ('generation','search','export')),
  owner_id TEXT NOT NULL,
  payload_ref TEXT NOT NULL REFERENCES payloads(payload_ref),
  physical_digest TEXT NOT NULL,
  scope_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  PRIMARY KEY(owner_kind,owner_id,payload_ref,scope_epoch)
) WITHOUT ROWID;
CREATE INDEX trace_pins_by_physical_digest ON trace_payload_pins(physical_digest,owner_kind,owner_id);
CREATE TABLE trace_pending_uploads (
  upload_id TEXT PRIMARY KEY,
  owner_kind TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  payload_ref TEXT NOT NULL,
  physical_digest TEXT NOT NULL,
  scope_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  state TEXT NOT NULL CHECK(state IN ('pending','pinned','cancelled')),
  created_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE INDEX trace_uploads_by_digest ON trace_pending_uploads(physical_digest,state);
CREATE INDEX trace_pending_upload_ids ON trace_pending_uploads(upload_id) WHERE state='pending';
CREATE INDEX observation_payload_roots_by_payload ON observation_payload_roots(payload_ref,admission_seq);

-- The GC final eligibility reservation fences concurrent reference publication.
-- Actual unlink/collection is intentionally a later worker over this protocol.
CREATE TABLE trace_blob_gc (
  physical_digest TEXT PRIMARY KEY,
  state TEXT NOT NULL CHECK(state IN ('candidate','deleting'))
) WITHOUT ROWID;

CREATE TABLE trace_blob_inventory (
  physical_digest TEXT PRIMARY KEY,
  collected INTEGER NOT NULL DEFAULT 0 CHECK(collected IN (0,1))
) WITHOUT ROWID;
CREATE INDEX trace_blob_inventory_candidates ON trace_blob_inventory(collected,physical_digest);
CREATE TABLE trace_blob_writes (
  write_id TEXT PRIMARY KEY,
  physical_digest TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX trace_writes_by_digest ON trace_blob_writes(physical_digest,write_id);

CREATE TABLE trace_capture_uploads (
  upload_id TEXT PRIMARY KEY,
  observation_ref TEXT NOT NULL UNIQUE,
  issuer_json TEXT NOT NULL,
  ingestion_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  capture_json TEXT NOT NULL,
  capture_digest TEXT NOT NULL,
  declarations_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('open','admitted','discarded','cancelled'))
) WITHOUT ROWID;
CREATE INDEX trace_capture_upload_states ON trace_capture_uploads(state,upload_id);
CREATE TABLE trace_capture_payloads (
  upload_id TEXT NOT NULL REFERENCES trace_capture_uploads(upload_id),
  payload_ref TEXT NOT NULL,
  physical_digest TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('bytes','manifest')),
  expected_length INTEGER,
  manifest_json TEXT,
  received INTEGER NOT NULL DEFAULT 0 CHECK(received IN (0,1)),
  reused_occurrence INTEGER REFERENCES occurrences(occurrence_id),
  PRIMARY KEY(upload_id,payload_ref)
) WITHOUT ROWID;
CREATE INDEX trace_capture_payload_roots ON trace_capture_payloads(physical_digest,upload_id);
CREATE TABLE trace_capture_upload_slots (
  upload_id TEXT PRIMARY KEY REFERENCES trace_capture_uploads(upload_id),
  ingestion_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch)
) WITHOUT ROWID;
CREATE INDEX trace_upload_slots_by_epoch ON trace_capture_upload_slots(ingestion_epoch,upload_id);

CREATE TABLE trace_physical_roots (
  root_kind TEXT NOT NULL,
  root_id TEXT NOT NULL,
  ingestion_epoch TEXT NOT NULL REFERENCES ingestion_epochs(epoch),
  physical_digest TEXT NOT NULL,
  PRIMARY KEY(root_kind,root_id,ingestion_epoch,physical_digest)
) WITHOUT ROWID;
CREATE INDEX trace_roots_by_digest ON trace_physical_roots(physical_digest);
CREATE INDEX trace_roots_by_epoch ON trace_physical_roots(ingestion_epoch,root_kind,root_id,physical_digest);
CREATE TABLE trace_gc_cursor(singleton INTEGER PRIMARY KEY CHECK(singleton=1),epoch_order INTEGER NOT NULL);
INSERT INTO trace_gc_cursor VALUES(1,0);
CREATE TABLE trace_gc_guard(singleton INTEGER PRIMARY KEY CHECK(singleton=1),guard_token TEXT);
INSERT INTO trace_gc_guard VALUES(1,NULL);
