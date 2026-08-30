-- The journal: active sessions, their turns, the live event tail, and what an
-- agent is blocked on.
--
-- `docs/database-schema.md` is authoritative for this file and every comment here
-- defers to it. What is worth stating at the top is why this is *one* migration
-- rather than seven.
--
-- **`001` is rewritten in place**, and the append-only rule below is deliberately
-- suspended for this one change. `AGENTS.md` § Compatibility is the licence: there
-- is no other consumer, no irreplaceable installed state, and every existing
-- journal is re-derivable from the agents' own native stores. A chain that added
-- `span`, `origin`, and `form_json` and then dropped `origin`, `identity_mode`,
-- `lifecycle_mode`, `trace_session_id`, `incarnation_id`, `runner_exit_code`, and
-- `reconciled_at_ms` would document an intermediate state nobody should ever
-- reconstruct. So `span` and `origin` sit in the table bodies below instead of
-- appearing as `ALTER`s, and `kind` is unconstrained from the start.
--
-- The one precondition, which `AGENTS.md` also states: verify the native sources
-- still exist before discarding a journal. Nothing here discards one — a rewritten
-- `001` simply refuses to open a database migrated under the old checksum, which
-- fails loudly at the one moment somebody is looking.
--
-- From here on the append-only rule resumes: `sqlx` checksums each file, so editing
-- a deployed migration makes every existing journal refuse to open.

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
-- segment and for `parent_session`, not as the storage shape.
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
  -- One column for two different relationships, which the row it points at
  -- disambiguates: a delegation folds into its parent as a count, while a fork
  -- replays its origin's entire prefix and continues past it. A listing must not
  -- show both under one label. Holds an encoded triple.
  parent_session   TEXT,
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
