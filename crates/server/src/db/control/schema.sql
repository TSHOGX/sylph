CREATE TABLE IF NOT EXISTS control_producer (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    issuer_json TEXT NOT NULL,
    producer_id TEXT NOT NULL,
    producer_epoch TEXT NOT NULL,
    next_seq INTEGER NOT NULL CHECK (next_seq >= 0),
    pending_count INTEGER NOT NULL CHECK (pending_count >= 0),
    pending_bytes INTEGER NOT NULL CHECK (pending_bytes >= 0)
);

CREATE TABLE IF NOT EXISTS trace_commands (
    command_id TEXT PRIMARY KEY,
    target_ref TEXT NOT NULL,
    command_kind TEXT NOT NULL,
    environment_id TEXT NOT NULL,
    ingestion_epoch TEXT NOT NULL,
    origin_subject TEXT NOT NULL,
    idempotency_key TEXT,
    request_digest TEXT NOT NULL,
    acceptance_json BLOB NOT NULL,
    current_input_json BLOB NOT NULL,
    current_revision INTEGER NOT NULL DEFAULT 0 CHECK(current_revision>=0),
    dispatched_revision TEXT,
    accepted_seq INTEGER NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK (state IN
        ('accepted', 'dispatching', 'receiver_accepted', 'unknown', 'rejected', 'consumed', 'cancelled', 'conflicting')),
    dispatch_attempts INTEGER NOT NULL DEFAULT 0 CHECK (dispatch_attempts >= 0),
    bound_agent_json BLOB,
    bound_seq INTEGER,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (origin_subject, target_ref, idempotency_key),
    CHECK ((bound_agent_json IS NULL) = (bound_seq IS NULL))
);
CREATE TRIGGER IF NOT EXISTS trace_bound_identity_immutable
BEFORE UPDATE OF bound_agent_json, bound_seq ON trace_commands WHEN OLD.bound_agent_json IS NOT NULL
BEGIN SELECT RAISE(ABORT, 'native launch identity is immutable'); END;
CREATE INDEX IF NOT EXISTS trace_commands_dispatch ON trace_commands(state, command_kind, accepted_seq);
CREATE INDEX IF NOT EXISTS trace_commands_queue_target ON trace_commands(target_ref, state, command_kind, accepted_seq);
CREATE INDEX IF NOT EXISTS trace_commands_environment ON trace_commands(environment_id,origin_subject,state,command_id);
CREATE INDEX IF NOT EXISTS trace_commands_bound_agent ON trace_commands(bound_agent_json,command_id) WHERE bound_agent_json IS NOT NULL;
CREATE TABLE IF NOT EXISTS deleted_commands(
 command_id TEXT PRIMARY KEY,
 origin_subject TEXT NOT NULL,
 target_ref TEXT NOT NULL,
 idempotency_key TEXT,
 ingestion_epoch TEXT NOT NULL,
 deleted_at_ms INTEGER NOT NULL
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS control_erase_jobs(agent_ref TEXT PRIMARY KEY,through_seq INTEGER NOT NULL,created_at_ms INTEGER NOT NULL) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS control_outbox (
    observation_seq INTEGER PRIMARY KEY CHECK (observation_seq > 0),
    command_id TEXT NOT NULL REFERENCES trace_commands(command_id),
    event_key TEXT NOT NULL,
    capture_json BLOB NOT NULL,
    payload BLOB NOT NULL,
    manifest_json BLOB,
    encoded_bytes INTEGER NOT NULL CHECK (encoded_bytes >= 0),
    created_at_ms INTEGER NOT NULL,
    exported_disposition TEXT CHECK (exported_disposition IN ('retained', 'discarded_by_deletion')),
    admission_cut INTEGER,
    UNIQUE (command_id, event_key),
    CHECK ((exported_disposition = 'retained' AND admission_cut IS NOT NULL)
        OR (exported_disposition IS NULL AND admission_cut IS NULL)
        OR (exported_disposition = 'discarded_by_deletion' AND admission_cut IS NULL))
);
CREATE INDEX IF NOT EXISTS control_outbox_pending ON control_outbox(observation_seq)
    WHERE exported_disposition IS NULL;
CREATE INDEX IF NOT EXISTS control_outbox_age ON control_outbox(created_at_ms)
    WHERE exported_disposition IS NULL;

-- Submitted revisions and source occurrences cannot change when delivery progresses.
CREATE TRIGGER IF NOT EXISTS trace_command_immutable
BEFORE UPDATE OF command_id, target_ref, command_kind, environment_id, ingestion_epoch, origin_subject, idempotency_key,
    request_digest, acceptance_json, accepted_seq, created_at_ms ON trace_commands
BEGIN SELECT RAISE(ABORT, 'accepted command is immutable'); END;

CREATE TRIGGER IF NOT EXISTS dispatched_input_revision_immutable
BEFORE UPDATE OF current_input_json,current_revision ON trace_commands WHEN OLD.state!='accepted'
BEGIN SELECT RAISE(ABORT,'dispatched input revision is immutable'); END;

CREATE TRIGGER IF NOT EXISTS control_occurrence_immutable
BEFORE UPDATE OF observation_seq, command_id, event_key, capture_json, payload,
    manifest_json, encoded_bytes, created_at_ms ON control_outbox
BEGIN SELECT RAISE(ABORT, 'outbox occurrence is immutable'); END;

CREATE TRIGGER IF NOT EXISTS control_unexported_retained
BEFORE DELETE ON control_outbox WHEN OLD.exported_disposition IS NULL
BEGIN SELECT RAISE(ABORT, 'unexported command evidence cannot be deleted'); END;
