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

  workdir          TEXT NOT NULL,
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
  request_text TEXT NOT NULL DEFAULT '',
  -- Links a turn to an agent command rather than to typed text.
  command_invocation_id TEXT,
  -- Nullable, so a turn the server originates carries none while the UNIQUE below
  -- still de-duplicates every client retry.
  idempotency_key TEXT,
  error_code TEXT,
  error_message TEXT,
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
-- `history.db` and deletes them here (`docs/storage.md` § Three tiers).
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
