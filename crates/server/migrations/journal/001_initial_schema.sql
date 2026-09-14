-- The journal: active sessions, their turns, the live event tail, and what an
-- agent is blocked on.
--
-- `docs/database-schema.md` is authoritative for this file and every comment here
-- defers to it. What is worth stating at the top is why this is *one* migration.
--
-- **The journal's whole schema is this file, collapsed rather than chained**
-- (`docs/database-schema.md` § Schema evolution). `AGENTS.md` § Compatibility is the
-- licence: there is no other consumer, no irreplaceable installed state, and every
-- journal is re-derivable from the agents' own native stores. A chain preserves the
-- order a schema was discovered in, which is only worth keeping when some database
-- is stranded partway along it — and none is. So every column sits in the table
-- body that owns it, and a reader learns the shape from one file instead of
-- replaying edits to reach it.
--
-- The cost is stated so it is not a surprise: `sqlx` checksums each file, so editing
-- this one makes an already-migrated journal refuse to open. That is the intended
-- failure. It is loud, it happens at the one moment somebody is looking, and the
-- remedy is to discard the data directory and re-adopt — which is only safe because
-- the native sources are the authority. Verify they still exist before discarding.

CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- Authenticated actors and their environment-scoped capabilities.  The server
-- administration root is a subject flag rather than a grantable capability.
CREATE TABLE subjects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  is_admin INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
  created_at_ms INTEGER NOT NULL
);

CREATE TABLE env_grants (
  subject_id TEXT NOT NULL,
  environment_id TEXT NOT NULL,
  capability TEXT NOT NULL CHECK (capability IN ('observe', 'control')),
  issuer_subject_id TEXT,
  issued_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER,
  consumed_at_ms INTEGER,
  PRIMARY KEY (subject_id, environment_id, capability),
  FOREIGN KEY (subject_id) REFERENCES subjects(id) ON DELETE CASCADE,
  FOREIGN KEY (issuer_subject_id) REFERENCES subjects(id) ON DELETE SET NULL,
  FOREIGN KEY (environment_id) REFERENCES env_installations(environment_id) ON DELETE CASCADE
);

CREATE INDEX env_grants_environment_idx ON env_grants(environment_id, capability);

-- No keys are tracked. Two former ones are named in `docs/database-schema.md` so
-- they are not reinvented: `schema_version` was a hand-maintained copy of a number
-- `_sqlx_migrations` already keeps, and drifted four migrations behind; `instance_id`
-- was never written by any code at all. The table survives as a place to put one.

-- Identity is `(environment, agent, native_id)` and there is no Sylph-minted id
-- (`docs/architecture.md` § Identity). Stored as three columns with a composite
-- primary key, so the triple *is* the row.
--
-- A composite key rather than one encoded text column because every listing filters
-- on `environment` and most group by `workdir` within it. The encoded form
-- (`<env>:<agent>:<native>`, delimiter `:` and never `/`) exists for a URL path
-- segment and for `session_edges`' endpoints, not as the storage shape.
--
-- A row exists only once an agent has assigned the session an id, so `POST
-- /sessions` starts an agent rather than reserving one. The "draft session with no
-- work in it" concept is gone, and with it the empty-session delete endpoint.
CREATE TABLE sessions (
  -- The opaque id minted at pairing (`docs/environments.md`), never a hostname: a
  -- name in a key means renaming destroys identity.
  environment  TEXT NOT NULL,
  agent        TEXT NOT NULL,
  native_id    TEXT NOT NULL,

  -- Native stores may omit a working directory. Preserve that unknown as NULL;
  -- guessing a path would claim where arbitrary code may run.
  workdir          TEXT,
  title            TEXT,
  -- Whether `SOUL.md` is appended. Settable at creation and **refused afterwards**:
  -- a transcript records what produced it, so rewriting the flag would rewrite what
  -- the stored history means. The one behavioural remnant of the deleted
  -- `identity_mode`, which had exactly one read site.
  inject_identity  INTEGER NOT NULL,
  -- How much resolution the source had. A property of the *record*, not of who
  -- started the session: a session Sylph launched, read back from the vendor's store
  -- after the process died, is `messages` too. A renderer reads it before deciding
  -- whether it may draw a duration.
  fidelity         TEXT NOT NULL CHECK (fidelity IN ('stream','blocks','messages')),
  flags_json       TEXT NOT NULL DEFAULT '{}',
  status           TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle','running')),
  next_event_seq   INTEGER NOT NULL DEFAULT 0 CHECK (next_event_seq >= 0),
  -- Ancestry is not a column here. A delegation folds into its parent as a count
  -- while a fork replays its origin's prefix and continues past it, so a listing
  -- must not show both under one label — `session_edges` below carries the relation
  -- that distinguishes them, and one nullable parent pointer could not.
  created_at_ms    INTEGER NOT NULL,
  updated_at_ms    INTEGER NOT NULL,

  PRIMARY KEY (environment, agent, native_id)
);

CREATE INDEX sessions_updated_idx ON sessions(updated_at_ms DESC);
-- Every listing filters on environment and groups by workdir within it.
CREATE INDEX sessions_workdir_idx ON sessions(environment, workdir);

-- Every child table carries the triple, not a surrogate. A composite foreign key is
-- more verbose than `session_id TEXT REFERENCES sessions(id)`, and the verbosity is
-- the point: a surrogate key on `sessions` would have to be generated, which
-- reintroduces exactly the second id space `docs/architecture.md` § Identity
-- deletes — and with it the comparison that is silently always false. Three columns
-- that cannot disagree with the parent beat one that can.
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  -- `settling` is in flight, not terminal, and it is what makes the four terminal
  -- statuses mean something a client can act on: *the record is finished*. A turn
  -- whose outcome is known but whose settle sweep has not finished writing what it
  -- left open sits here, so no reader ever sees a terminal turn over a partial
  -- record. `crate::supervisor::TurnCloser::close` owns the two writes and states
  -- what one write cost.
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'starting', 'running', 'stopping', 'settling',
    'completed', 'failed', 'cancelled', 'interrupted'
  )),
  -- Who started this turn, and **not the same concept as the deleted session
  -- `origin`**. An `agent` turn is one nobody asked for: measured on claude 2.x, a
  -- session with backgrounded work emits the delegate's completion after its turn's
  -- `result` and then an entire exchange of its own to report it. Such a turn is
  -- real in every mechanical sense — it consolidates, it orders by sequence, it
  -- appears in the transcript where it happened.
  --
  -- Load-bearing in the supervisor, which is why it is a column rather than an
  -- inference: an agent turn never had a waiter, so `settle_turn` uses this to tell
  -- "no waiter, because there never was one" from "no waiter, because a deliberate
  -- stop already claimed it". Conflating them reported a user's stop as `LINK_LOST`.
  -- It also distinguishes an empty `request_text` here from a user sending nothing.
  origin TEXT NOT NULL DEFAULT 'user' CHECK (origin IN ('user', 'agent')),
  -- Subject provenance scopes grant revocation to work authorized by that subject.
  origin_subject TEXT,
  request_text TEXT NOT NULL DEFAULT '',
  -- Links a turn to an agent command rather than to typed text.
  command_invocation_id TEXT,
  -- Nullable, so a turn the server originates carries none while the UNIQUE below
  -- still de-duplicates every client retry.
  idempotency_key TEXT,
  error_code TEXT,
  error_message TEXT,
  close_reason TEXT CHECK (close_reason IN (
    'interrupted', 'spool_full', 'grant_revoked', 'link_lost',
    'agent_exited', 'adapter_silent'
  )),
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  stopped_at_ms INTEGER,
  UNIQUE(session_env, session_agent, session_native, idempotency_key),
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE
);

CREATE INDEX turns_session_idx
  ON turns(session_env, session_agent, session_native, created_at_ms DESC);
-- Partial, covering exactly the in-flight states, which is the query restart
-- reconciliation runs.
CREATE INDEX turns_status_idx ON turns(status)
  WHERE status IN ('queued', 'starting', 'running', 'stopping', 'settling');

-- The live event tail. Bounded: consolidation moves a settled turn's rows into
-- `history.db` and deletes them here (`docs/storage.md` § Workloads and database boundaries).
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  -- **Deliberately unconstrained, and that is the fix rather than an omission.**
  -- The original `001` listed ten values, which is a promise to break on the next
  -- adapter that adds one — and it broke exactly that way: `append_event` logged and
  -- continued, so a sequence number was consumed and the event reached live clients
  -- while nothing reached the journal. A turn streamed correctly and came back empty
  -- on reload. The protocol says kinds are additive, so validation belongs at the
  -- wire boundary where an unknown kind can be *reported*, not here where it can
  -- only be dropped.
  kind TEXT NOT NULL,
  phase TEXT,
  stream_id TEXT,
  -- Which delegated trajectory this event ran in; NULL means the root turn. That one
  -- pointer expresses a subagent, a nested subagent, and a workflow's parallel
  -- phases, since those differ only in the shape of the resulting tree. Nullable
  -- rather than `''` because absent and "the root" are the same fact.
  span TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  -- One derivation, in this column. `StreamEvent::is_stable()` reads the field
  -- rather than re-computing it from `phase`: the two were equivalent until
  -- `subagent` became stable at *every* phase for structural reasons, after which a
  -- trajectory's `started` was silently not journaled and every delegated subtree
  -- flattened on reload.
  stability TEXT NOT NULL DEFAULT 'stable'
    CHECK (stability IN ('ephemeral', 'stable')),
  -- A closed set, unlike `kind`, because it has exactly two producers and a third would
  -- be a design change rather than an adapter's choice.
  source TEXT NOT NULL DEFAULT 'server'
    CHECK (source IN ('agent', 'server')),
  -- Whether this trajectory's interior can be reached, authored by the producer that
  -- knows. Nullable because a producer may not say, and the frontend then infers from
  -- observed children — an inference that must stay a *fallback*, since "no child
  -- events arrived" and "this trajectory forwards nothing" look identical from a
  -- client that attached late (`docs/session-protocol.md` § Delegated trajectories).
  interior_state TEXT,
  -- Three clocks, because one cannot answer both questions a renderer asks. A vendor's
  -- own timestamp orders what happened; the server's receipt clock orders what arrived,
  -- and only the second is guaranteed monotonic under replay.
  --
  -- `recorded_at_ms` is NULL when the vendor supplied nothing usable — absent rather
  -- than guessed, since a placeholder in durable data cannot be withdrawn by fixing
  -- the producer (`AGENTS.md` § Rules That Keep Costing Time).
  arrival_at_ms INTEGER NOT NULL,
  recorded_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  -- `seq` is unique per *session*, not per turn, which is what makes SSE
  -- resume-by-sequence safe: a replayed frame cannot be journalled twice. Two events
  -- written in the same millisecond still have one defined order.
  UNIQUE(session_env, session_agent, session_native, seq),
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE
);

-- The cursor survives journal eviction and advances atomically for every writer,
-- including batched appends, session promotion, and producer receipt transactions.
CREATE TRIGGER events_advance_sequence AFTER INSERT ON events
BEGIN
  UPDATE sessions SET next_event_seq = NEW.seq + 1
  WHERE environment = NEW.session_env AND agent = NEW.session_agent
    AND native_id = NEW.session_native AND next_event_seq <= NEW.seq;
END;

CREATE INDEX events_session_idx
  ON events(session_env, session_agent, session_native, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;
-- What keeps focus mode and single-subtree replay from scanning the session.
CREATE INDEX idx_events_span
  ON events(session_env, session_agent, session_native, span, seq);

-- What an agent is blocked on, waiting for a human.
--
-- A durable resource rather than a stream message, because the agent waits until
-- someone answers: a client closing its laptop must not strand it, and the answer
-- has to be possible from a frontend that was not attached when it was raised.
CREATE TABLE pending_requests (
  id TEXT PRIMARY KEY,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  -- The adapter's own id, scoped to one link and never serialized to a client — a
  -- client-supplied adapter id would be a way to answer a request never raised.
  -- Clients address a request by `id`.
  adapter_request_id TEXT NOT NULL,
  kind TEXT NOT NULL,                  -- approval | dialog | elicitation | choice
  tool TEXT,
  tool_use_id TEXT,
  title TEXT NOT NULL DEFAULT '',
  detail_json TEXT NOT NULL DEFAULT '{}',
  options_json TEXT NOT NULL DEFAULT '[]',
  -- The input the request declared it wants collected, and absent is meaningful
  -- rather than missing: a request with no form is an approval, whose subject is the
  -- one proposed tool input in `detail_json` (`docs/session-protocol.md` § Request
  -- forms).
  form_json TEXT,
  default_option TEXT,
  risk TEXT,
  -- An agent can mark an ask where granting "always" would write a permission rule
  -- *broader than the ask itself* — claude's own schema says a host "should omit any
  -- persistent-rule affordance when set". Offering it anyway converts a narrow yes
  -- into a wide one the user never read.
  allow_persistent INTEGER NOT NULL DEFAULT 1,
  -- Advisory and never acted on here: auto-applying a default on lapse would answer
  -- on the user's behalf in exactly the decision this channel exists to give them.
  expires_at_ms INTEGER,
  -- Resolved rows are kept rather than deleted: what was approved, by whom, and with
  -- what edit is the audit trail a permission decision needs.
  status TEXT NOT NULL DEFAULT 'pending',  -- pending | resolved | abandoned
  chosen_option TEXT,
  resolved_input_json TEXT,
  resolved_by TEXT,
  created_at_ms INTEGER NOT NULL,
  resolved_at_ms INTEGER,
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE
);

-- Server-authoritative ledger for every user control intent.  Vendor command
-- receipts are transport facts; this row is the user-visible lifecycle that
-- survives retries, reconnects, and a second browser.
CREATE TABLE control_requests (
  request_id TEXT PRIMARY KEY,
  session_env TEXT NOT NULL,
  session_agent TEXT NOT NULL,
  session_native TEXT NOT NULL,
  origin_subject TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('send', 'steer', 'queue', 'native_steer', 'native_queue', 'stop', 'approval')),
  text TEXT NOT NULL DEFAULT '',
  target_turn_id TEXT,
  server_seq INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('accepted', 'queued', 'dispatched', 'confirmed', 'unconfirmed', 'consumed', 'rejected', 'failed', 'cancelled')),
  idempotency_key TEXT,
  vendor_receipt TEXT,
  error_message TEXT,
  created_at_ms INTEGER NOT NULL,
  consumed_at_ms INTEGER,
  failed_at_ms INTEGER,
  revision INTEGER NOT NULL DEFAULT 0,
  UNIQUE(session_env, session_agent, session_native, idempotency_key),
  UNIQUE(session_env, session_agent, session_native, server_seq)
);
CREATE INDEX control_requests_session_idx
  ON control_requests(session_env, session_agent, session_native, server_seq);

-- Partial, because resolved rows accumulate without bound and never appear in the
-- hot "what is this session waiting on" query.
CREATE INDEX pending_requests_open
  ON pending_requests(session_env, session_agent, session_native, created_at_ms)
  WHERE status = 'pending';

-- Resolving arrives with the adapter's id, so that lookup needs its own path.
CREATE INDEX pending_requests_by_adapter_id
  ON pending_requests(adapter_request_id, status);

-- Durable state for paired envagents and inbound delivery.
--
-- The environment registry is the server's authority. An envagent's spool only remembers
-- what has not been acknowledged, so these tables are what survives a server restart:
-- pairing, connection leases, command outcomes, and producer receipts.
CREATE TABLE env_installations (
  -- The opaque id minted at pairing, which is also `sessions.environment`. Not a
  -- hostname: a name in a key means renaming destroys identity.
  environment_id TEXT PRIMARY KEY,
  installation_id TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  origin TEXT NOT NULL,
  grant_authority TEXT NOT NULL DEFAULT 'machine'
    CHECK (grant_authority IN ('machine', 'server')),
  -- A hash, never the token. A stolen table must not be a stolen credential.
  token_hash TEXT NOT NULL,
  connected INTEGER NOT NULL DEFAULT 0,
  -- Bumped per reconnect, so a frame from a previous connection is identifiable as
  -- stale rather than applied out of order after a reconnect.
  connection_epoch INTEGER NOT NULL DEFAULT 0,
  last_seen_at_ms INTEGER,
  reachable_via TEXT,
  -- What this machine can run, discovered by that envagent. Per-environment and
  -- time-varying, never a server-wide list (`docs/environments.md` § Agent availability).
  agents_json TEXT NOT NULL DEFAULT '[]',
  -- Machine facts reported by the envagent (OS, user, home, and other metadata).
  -- The envagent is the authority because only it can inspect the owning machine.
  facts_json TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE env_pairings (
  code_hash TEXT PRIMARY KEY,
  expires_at_ms INTEGER NOT NULL,
  used_at_ms INTEGER
);

CREATE INDEX env_pairings_live_idx
  ON env_pairings(expires_at_ms)
  WHERE used_at_ms IS NULL;

-- One-shot capability invitations issued by a local envagent.  The bearer code
-- is stored only as a hash; installation_id preserves issuer provenance and
-- the foreign key makes unpairing invalidate every outstanding invite.
CREATE TABLE env_invites (
  code_hash TEXT PRIMARY KEY,
  environment_id TEXT NOT NULL,
  installation_id TEXT NOT NULL,
  capability TEXT NOT NULL CHECK (capability IN ('observe', 'control')),
  expires_at_ms INTEGER NOT NULL,
  consumed_at_ms INTEGER,
  FOREIGN KEY (environment_id) REFERENCES env_installations(environment_id) ON DELETE CASCADE,
  FOREIGN KEY (installation_id) REFERENCES env_installations(installation_id) ON DELETE CASCADE
);

CREATE INDEX env_invites_live_idx
  ON env_invites(environment_id, expires_at_ms)
  WHERE consumed_at_ms IS NULL;

-- What a command already answered, so a replay returns the first outcome instead of
-- acting twice. `at_most_once` is the class where that distinction is the whole point:
-- re-running it is not equivalent to returning what it returned.
CREATE TABLE env_command_results (
  environment_id TEXT NOT NULL,
  request_id TEXT NOT NULL,
  idempotency TEXT NOT NULL CHECK (idempotency IN ('replayable', 'at_most_once')),
  outcome_json TEXT NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  PRIMARY KEY (environment_id, request_id)
);

-- Which producer frames have been accepted, keyed by the producer's own sequence, so a
-- spool replaying after reconnect cannot journal an event twice. `payload_hash` makes a
-- re-delivery that disagrees with what was stored detectable rather than silently
-- ignored.
CREATE TABLE env_event_receipts (
  environment_id TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  producer_seq INTEGER NOT NULL CHECK (producer_seq > 0),
  session_env TEXT NOT NULL,
  session_agent TEXT NOT NULL,
  session_native TEXT NOT NULL,
  server_seq INTEGER NOT NULL,
  payload_hash TEXT NOT NULL,
  accepted_at_ms INTEGER NOT NULL,
  PRIMARY KEY (environment_id, producer_id, producer_seq)
);

CREATE INDEX env_event_receipts_session_idx
  ON env_event_receipts(session_env, session_agent, session_native, server_seq);

-- The contiguous producer prefix is updated with receipts, not reconstructed by
-- scanning every receipt for every frame. Filling a gap walks only the newly
-- contiguous suffix; ordinary ordered ingestion seeks one next sequence.
CREATE TABLE env_producer_cursors (
  environment_id TEXT NOT NULL,
  producer_id TEXT NOT NULL,
  accepted_through INTEGER NOT NULL CHECK(accepted_through >= 0),
  PRIMARY KEY(environment_id, producer_id)
) WITHOUT ROWID;

CREATE TRIGGER receipts_advance_cursor AFTER INSERT ON env_event_receipts BEGIN
  INSERT INTO env_producer_cursors(environment_id, producer_id, accepted_through)
    VALUES(NEW.environment_id, NEW.producer_id, 0) ON CONFLICT DO NOTHING;
  UPDATE env_producer_cursors SET accepted_through = (
    WITH RECURSIVE contiguous(seq) AS (
      SELECT env_producer_cursors.accepted_through
      UNION ALL
      SELECT seq + 1 FROM contiguous WHERE EXISTS (
        SELECT 1 FROM env_event_receipts r WHERE r.environment_id = NEW.environment_id
          AND r.producer_id = NEW.producer_id AND r.producer_seq = contiguous.seq + 1)
    ) SELECT MAX(seq) FROM contiguous
  ) WHERE environment_id = NEW.environment_id AND producer_id = NEW.producer_id
      AND NEW.producer_seq = accepted_through + 1;
END;

-- Server-owned graph edges. Events remain stored once under their owning session; this
-- is a read model for delegation and fork provenance, which is why `relation` is a
-- closed set: the two differ in how a listing must present them, not in degree.
CREATE TABLE session_edges (
  parent_env TEXT NOT NULL,
  parent_agent TEXT NOT NULL,
  parent_native TEXT NOT NULL,
  child_env TEXT NOT NULL,
  child_agent TEXT NOT NULL,
  child_native TEXT NOT NULL,
  relation TEXT NOT NULL CHECK (relation IN ('delegation', 'fork')),
  source TEXT NOT NULL DEFAULT 'server',
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY (
    parent_env, parent_agent, parent_native,
    child_env, child_agent, child_native,
    relation
  ),
  -- No session is its own parent, checked here rather than trusted from a caller: a
  -- self-edge is a cycle every ancestry walk would have to defend against forever.
  CHECK (
    parent_env <> child_env OR
    parent_agent <> child_agent OR
    parent_native <> child_native
  )
);

CREATE INDEX session_edges_child_idx
  ON session_edges(child_env, child_agent, child_native);

CREATE TABLE import_jobs (
  id TEXT PRIMARY KEY,
  environment_id TEXT NOT NULL,
  origin_subject TEXT NOT NULL,
  workdir TEXT,
  agents_json TEXT,
  mode TEXT NOT NULL CHECK (mode IN ('import_once', 'import_and_watch')),
  idempotency_key TEXT NOT NULL,
  request_json TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued','running','completed','cancelled','failed')),
  scan_complete INTEGER NOT NULL DEFAULT 0,
  cancel_requested INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(environment_id, origin_subject, idempotency_key)
);
CREATE INDEX import_jobs_environment_idx ON import_jobs(environment_id, created_at_ms DESC);

CREATE TABLE import_items (
  job_id TEXT NOT NULL REFERENCES import_jobs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL,
  order_tie INTEGER NOT NULL DEFAULT 0,
  agent TEXT NOT NULL,
  native_id TEXT NOT NULL,
  title TEXT,
  workdir TEXT,
  status TEXT NOT NULL CHECK (status IN ('pending','running','imported','refreshed','skipped','failed')),
  reason TEXT,
  error_code TEXT,
  source_revision TEXT,
  PRIMARY KEY(job_id, agent, native_id),
  UNIQUE(job_id, ordinal)
);

CREATE INDEX import_items_native_idx ON import_items(agent, native_id, job_id);

-- Reconstructable, body-free execution nodes; independent of archive eviction.
CREATE TABLE timeline_clock (id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL);
INSERT INTO timeline_clock VALUES (1, 0);
CREATE TABLE timeline_nodes (
  session_id TEXT NOT NULL,
  id TEXT NOT NULL,
  parent_id TEXT NOT NULL DEFAULT '',
  turn_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  order_tie INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  body_revision INTEGER NOT NULL DEFAULT 0,
  body_start_seq INTEGER NOT NULL DEFAULT 0,
  event_json TEXT NOT NULL,
  trajectory_json TEXT,
  stream_id TEXT,
  kind TEXT NOT NULL DEFAULT '',
  structure_error TEXT,
  children INTEGER NOT NULL DEFAULT 0,
  tools INTEGER NOT NULL DEFAULT 0,
  reads INTEGER NOT NULL DEFAULT 0,
  commands INTEGER NOT NULL DEFAULT 0,
  thinking INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,
  running INTEGER NOT NULL DEFAULT 0,
  waiting INTEGER NOT NULL DEFAULT 0,
  activity TEXT NOT NULL DEFAULT '',
  own_running INTEGER NOT NULL DEFAULT 0,
  scope_id TEXT NOT NULL DEFAULT '',
  unfinished INTEGER NOT NULL DEFAULT 0,
  own_unfinished INTEGER NOT NULL DEFAULT 0,
  own_activity TEXT NOT NULL DEFAULT '',
  child_running INTEGER NOT NULL DEFAULT 0,
  own_failed INTEGER NOT NULL DEFAULT 0,
  declared INTEGER NOT NULL DEFAULT 1,
  PRIMARY KEY(session_id, id)
);
CREATE INDEX timeline_streams ON timeline_nodes(session_id,stream_id,kind,ordinal DESC);
CREATE INDEX timeline_children ON timeline_nodes(session_id, parent_id, ordinal, order_tie, id);
CREATE INDEX timeline_changes ON timeline_nodes(session_id, parent_id, revision, id);
CREATE INDEX timeline_active ON timeline_nodes(session_id, parent_id, ordinal, id) WHERE running > 0;
CREATE TABLE timeline_lanes (
  session_id TEXT NOT NULL, turn_id TEXT NOT NULL, parent_id TEXT NOT NULL,
  group_id TEXT NOT NULL,
  PRIMARY KEY(session_id, turn_id, parent_id)
);
CREATE TRIGGER sessions_delete_timeline AFTER DELETE ON sessions BEGIN
  DELETE FROM timeline_nodes WHERE session_id = OLD.environment || ':' || OLD.agent || ':' || OLD.native_id;
  DELETE FROM timeline_lanes WHERE session_id = OLD.environment || ':' || OLD.agent || ':' || OLD.native_id;
END;
CREATE INDEX events_stream_body ON events(session_env, session_agent, session_native, stream_id, seq);

-- Requests remain operable resources. Their projection points to the durable id;
-- ancestor counts change in the same transaction as create/resolve/abandon.
CREATE TRIGGER requests_project_insert AFTER INSERT ON pending_requests BEGIN
  UPDATE timeline_clock SET revision=revision+1 WHERE id=1;
  INSERT INTO timeline_nodes(session_id,id,parent_id,turn_id,ordinal,revision,event_json,waiting,order_tie)
  VALUES(NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native,'q:'||NEW.id,
    COALESCE((SELECT id FROM timeline_nodes WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND stream_id=NEW.tool_use_id AND kind='tool_call' ORDER BY ordinal DESC LIMIT 1),''),
    NEW.turn_id, (SELECT next_event_seq*2 FROM sessions WHERE environment=NEW.session_env AND agent=NEW.session_agent AND native_id=NEW.session_native),
    (SELECT revision FROM timeline_clock WHERE id=1),
    json_object('seq',-1,'kind','request','phase','started','text',substr(NEW.title,1,256),
      'data',json_object('requestId',NEW.id),'stability','stable','source','server',
      'arrivalAtMs',NEW.created_at_ms,'createdAtMs',NEW.created_at_ms), NEW.status='pending',(SELECT revision FROM timeline_clock WHERE id=1));
  UPDATE timeline_nodes SET waiting=waiting+(NEW.status='pending'),
    children=children+(id=(SELECT parent_id FROM timeline_nodes WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND id='q:'||NEW.id)),
    revision=(SELECT revision FROM timeline_clock WHERE id=1)
  WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND id IN (
    WITH RECURSIVE ancestors(id) AS (
      SELECT parent_id FROM timeline_nodes WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND id='q:'||NEW.id
      UNION SELECT n.parent_id FROM timeline_nodes n JOIN ancestors a ON n.id=a.id WHERE n.session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native
    ) SELECT id FROM ancestors WHERE id<>''
  );
END;
CREATE TRIGGER requests_project_status AFTER UPDATE OF status ON pending_requests WHEN OLD.status<>NEW.status BEGIN
  UPDATE timeline_clock SET revision=revision+1 WHERE id=1;
  UPDATE timeline_nodes SET waiting=waiting+(NEW.status='pending')-(OLD.status='pending'),
    revision=(SELECT revision FROM timeline_clock WHERE id=1),
    event_json=CASE WHEN id='q:'||NEW.id THEN json_set(event_json,'$.phase',CASE WHEN NEW.status='pending' THEN 'started' ELSE 'completed' END) ELSE event_json END
  WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND id IN (
    WITH RECURSIVE ancestors(id) AS (
      SELECT 'q:'||NEW.id
      UNION SELECT n.parent_id FROM timeline_nodes n JOIN ancestors a ON n.id=a.id WHERE n.session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native
    ) SELECT id FROM ancestors WHERE id<>''
  );
END;

-- At most one removal per former parent/address. Unlike an append-only patch
-- log, repeated corrections of the same relationship do not grow this table.
CREATE TABLE timeline_removals (
  session_id TEXT NOT NULL, parent_id TEXT NOT NULL, id TEXT NOT NULL,
  revision INTEGER NOT NULL, node_json TEXT NOT NULL,
  PRIMARY KEY(session_id,parent_id,id)
);
CREATE INDEX timeline_removed_changes ON timeline_removals(session_id,parent_id,revision,id);
CREATE TRIGGER sessions_delete_timeline_removals AFTER DELETE ON sessions BEGIN
  DELETE FROM timeline_removals WHERE session_id=OLD.environment||':'||OLD.agent||':'||OLD.native_id;
END;

-- Block stream ids are turn-scoped. Only delegated trajectories have a session
-- scope; a promoted tool and its trajectory share a projection address.
CREATE TABLE timeline_trajectories (
  session_id TEXT NOT NULL, stream_id TEXT NOT NULL, node_id TEXT NOT NULL,
  PRIMARY KEY(session_id,stream_id)
);
CREATE TRIGGER sessions_delete_timeline_trajectories AFTER DELETE ON sessions BEGIN
  DELETE FROM timeline_trajectories WHERE session_id=OLD.environment||':'||OLD.agent||':'||OLD.native_id;
END;

CREATE TABLE timeline_event_nodes (
  session_id TEXT NOT NULL, seq INTEGER NOT NULL, node_id TEXT NOT NULL,
  PRIMARY KEY(session_id,seq)
);
CREATE TRIGGER sessions_delete_timeline_event_nodes AFTER DELETE ON sessions BEGIN
  DELETE FROM timeline_event_nodes WHERE session_id=OLD.environment||':'||OLD.agent||':'||OLD.native_id;
END;


CREATE INDEX timeline_turn_order ON timeline_nodes(session_id,turn_id,ordinal);
CREATE TRIGGER turns_project_prompt_insert AFTER INSERT ON turns WHEN NEW.request_text<>'' BEGIN
  UPDATE timeline_clock SET revision=revision+1 WHERE id=1;
  INSERT INTO timeline_nodes(session_id,id,parent_id,turn_id,ordinal,revision,event_json,kind,order_tie)
  VALUES(NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native,'p:'||NEW.id,'',NEW.id,
    (SELECT next_event_seq*2-1 FROM sessions WHERE environment=NEW.session_env AND agent=NEW.session_agent AND native_id=NEW.session_native),
    (SELECT revision FROM timeline_clock WHERE id=1),
    json_object('seq',-1,'kind','user','phase','completed','text',substr(NEW.request_text,1,1024),'stability','stable','source','user',
      'arrivalAtMs',NEW.created_at_ms,'createdAtMs',NEW.created_at_ms,
      'bodyRef',CASE WHEN length(NEW.request_text)>1024 THEN json_object('seq',-1,'textBytes',length(CAST(NEW.request_text AS BLOB)),'dataBytes',0) ELSE NULL END),'user',(SELECT revision FROM timeline_clock WHERE id=1));
END;
CREATE TRIGGER turns_project_prompt_update AFTER UPDATE OF request_text ON turns WHEN OLD.request_text<>NEW.request_text BEGIN
  UPDATE timeline_clock SET revision=revision+1 WHERE id=1;
  INSERT INTO timeline_nodes(session_id,id,parent_id,turn_id,ordinal,revision,event_json,kind,order_tie)
  VALUES(NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native,'p:'||NEW.id,'',NEW.id,
    COALESCE((SELECT MIN(ordinal)-1 FROM timeline_nodes WHERE session_id=NEW.session_env||':'||NEW.session_agent||':'||NEW.session_native AND turn_id=NEW.id),
      (SELECT next_event_seq*2-1 FROM sessions WHERE environment=NEW.session_env AND agent=NEW.session_agent AND native_id=NEW.session_native)),
    (SELECT revision FROM timeline_clock WHERE id=1),
    json_object('seq',-1,'kind','user','phase','completed','text',substr(NEW.request_text,1,1024),'stability','stable','source','user',
      'arrivalAtMs',NEW.created_at_ms,'createdAtMs',NEW.created_at_ms,
      'bodyRef',CASE WHEN length(NEW.request_text)>1024 THEN json_object('seq',-1,'textBytes',length(CAST(NEW.request_text AS BLOB)),'dataBytes',0) ELSE NULL END),'user',(SELECT revision FROM timeline_clock WHERE id=1))
  ON CONFLICT(session_id,id) DO UPDATE SET revision=excluded.revision,body_revision=excluded.revision,event_json=excluded.event_json;
END;

CREATE INDEX timeline_body_addresses ON timeline_event_nodes(session_id,node_id,seq);

CREATE INDEX timeline_open_scopes ON timeline_nodes(session_id,scope_id,turn_id,id) WHERE own_running>0;
CREATE INDEX timeline_open_turns ON timeline_nodes(turn_id,scope_id) WHERE own_running>0;
