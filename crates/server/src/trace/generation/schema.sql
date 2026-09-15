-- Disposable semantic generation. Evidence receipts never depend on this writer.
CREATE TABLE generation_manifest (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  manifest_json TEXT NOT NULL,
  database_identity TEXT NOT NULL
);
CREATE TABLE projection_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  evidence_cut INTEGER NOT NULL CHECK (evidence_cut >= 0),
  view_revision INTEGER NOT NULL CHECK (view_revision >= 0),
  revision_floor INTEGER NOT NULL CHECK (revision_floor >= 0),
  fence_token INTEGER NOT NULL CHECK (fence_token > 0),
  writer_mode TEXT NOT NULL CHECK (writer_mode IN ('running', 'fenced', 'sealed'))
);
INSERT INTO projection_state VALUES (1, 0, 0, 0, 1, 'running');

CREATE TABLE projection_revisions (
  view_revision INTEGER PRIMARY KEY,
  evidence_cut INTEGER NOT NULL UNIQUE CHECK (evidence_cut >= 0)
);
INSERT INTO projection_revisions VALUES (0, 0);

CREATE TABLE semantic_claims (
  claim_ref TEXT PRIMARY KEY,
  fact_kind TEXT NOT NULL,
  mapper_build TEXT NOT NULL,
  rule_id TEXT NOT NULL,
  evidence_count INTEGER NOT NULL CHECK (evidence_count >= 0),
  claim_bytes INTEGER NOT NULL CHECK (claim_bytes >= 0),
  claim_json TEXT NOT NULL,
  first_revision INTEGER NOT NULL REFERENCES projection_revisions(view_revision)
) WITHOUT ROWID;
CREATE TABLE claim_subjects (
  subject_ref TEXT NOT NULL,
  first_revision INTEGER NOT NULL REFERENCES projection_revisions(view_revision),
  claim_ref TEXT NOT NULL REFERENCES semantic_claims(claim_ref),
  fact_kind TEXT NOT NULL,
  PRIMARY KEY (subject_ref, first_revision, claim_ref)
) WITHOUT ROWID;
CREATE INDEX claims_by_subject_kind ON claim_subjects(subject_ref, fact_kind, first_revision, claim_ref);
CREATE INDEX subjects_by_claim ON claim_subjects(claim_ref, subject_ref);
CREATE TABLE claim_evidence (
  claim_ref TEXT NOT NULL REFERENCES semantic_claims(claim_ref),
  observation_ref TEXT NOT NULL,
  PRIMARY KEY (claim_ref, observation_ref)
) WITHOUT ROWID;
CREATE INDEX claims_by_evidence ON claim_evidence(observation_ref, claim_ref);
CREATE TABLE claim_supersessions (
  claim_ref TEXT NOT NULL REFERENCES semantic_claims(claim_ref),
  superseded_ref TEXT NOT NULL,
  PRIMARY KEY (claim_ref, superseded_ref)
) WITHOUT ROWID;
CREATE INDEX supersessions_by_prior ON claim_supersessions(superseded_ref, claim_ref);

-- Stable typed object addresses are separate from their temporal interpretations.
CREATE TABLE objects (
  object_ref TEXT PRIMARY KEY,
  object_kind TEXT NOT NULL,
  first_revision INTEGER NOT NULL REFERENCES projection_revisions(view_revision)
) WITHOUT ROWID;
CREATE TABLE object_versions (
  object_ref TEXT NOT NULL REFERENCES objects(object_ref),
  valid_from INTEGER NOT NULL REFERENCES projection_revisions(view_revision),
  valid_to INTEGER REFERENCES projection_revisions(view_revision),
  object_json TEXT,
  containing_agent TEXT,
  object_bytes INTEGER NOT NULL CHECK (object_bytes >= 0),
  preview TEXT NOT NULL,
  preview_truncated INTEGER NOT NULL CHECK (preview_truncated IN (0, 1)),
  withdrawn_reason TEXT,
  PRIMARY KEY (object_ref, valid_from),
  CHECK (valid_to IS NULL OR valid_to > valid_from),
  CHECK ((object_json IS NULL) = (withdrawn_reason IS NOT NULL))
) WITHOUT ROWID;
CREATE UNIQUE INDEX current_object_version ON object_versions(object_ref) WHERE valid_to IS NULL;

CREATE TABLE query_scopes (
  scope_key TEXT PRIMARY KEY,
  scope_json TEXT NOT NULL,
  collection_kind TEXT NOT NULL,
  owner_ref TEXT NOT NULL,
  selection_ref TEXT
) WITHOUT ROWID;
CREATE TABLE memberships (
  scope_key TEXT NOT NULL REFERENCES query_scopes(scope_key),
  order_key BLOB NOT NULL,
  object_ref TEXT NOT NULL REFERENCES objects(object_ref),
  valid_from INTEGER NOT NULL REFERENCES projection_revisions(view_revision),
  valid_to INTEGER REFERENCES projection_revisions(view_revision),
  PRIMARY KEY (scope_key, order_key, object_ref, valid_from),
  CHECK (valid_to IS NULL OR valid_to > valid_from)
) WITHOUT ROWID;
CREATE UNIQUE INDEX current_scope_member ON memberships(scope_key, object_ref) WHERE valid_to IS NULL;
CREATE INDEX membership_object_revision ON memberships(scope_key, object_ref, valid_from DESC);
CREATE TABLE scope_counts (
  scope_key TEXT NOT NULL REFERENCES query_scopes(scope_key),
  valid_from INTEGER NOT NULL REFERENCES projection_revisions(view_revision),
  member_count INTEGER NOT NULL CHECK (member_count >= 0),
  PRIMARY KEY (scope_key, valid_from)
) WITHOUT ROWID;

-- Each row proves payload reachability through one containing history/action
-- object, never through its original author or an unscoped CAS digest.
CREATE TABLE payload_anchor_versions (
  anchor_key TEXT NOT NULL,
  via_ref TEXT NOT NULL REFERENCES objects(object_ref),
  containing_agent TEXT NOT NULL,
  selection_ref TEXT,
  payload_ref TEXT NOT NULL,
  body_revision TEXT NOT NULL,
  representation TEXT NOT NULL,
  valid_from INTEGER NOT NULL REFERENCES projection_revisions(view_revision),
  valid_to INTEGER REFERENCES projection_revisions(view_revision),
  ranges_json TEXT NOT NULL,
  PRIMARY KEY (anchor_key, valid_from),
  CHECK (valid_to IS NULL OR valid_to > valid_from)
) WITHOUT ROWID;
CREATE UNIQUE INDEX current_payload_anchor ON payload_anchor_versions(anchor_key) WHERE valid_to IS NULL;
CREATE INDEX anchors_by_container ON payload_anchor_versions(via_ref, containing_agent, selection_ref, valid_from);

CREATE TABLE mapper_states (
  mapper_key TEXT PRIMARY KEY,
  state_json TEXT NOT NULL,
  at_revision INTEGER NOT NULL REFERENCES projection_revisions(view_revision)
) WITHOUT ROWID;
CREATE TABLE change_batches (
  view_revision INTEGER PRIMARY KEY REFERENCES projection_revisions(view_revision),
  evidence_cut INTEGER NOT NULL,
  batch_json TEXT NOT NULL,
  batch_bytes INTEGER NOT NULL CHECK (batch_bytes >= 0)
);
