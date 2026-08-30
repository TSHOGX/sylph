# Database Schema

**This file covers `journal.db`** — the hot tier, holding sessions, turns, and the events
of turns still in flight.

One authoritative record is split across three stores for SQLite's sake, not as a
data-model choice (`docs/storage.md` § Three tiers says why): `journal.db` here,
`history.db` for consolidated blocks and the full-text index, and `blobs/` beside them.
**`docs/storage.md` § Schema owns the other two**, because their tables only make sense
alongside the consolidation and retention rules that write them.

WAL mode, for concurrent readers against a live writer.

## Tables

### app_meta

Application metadata and migration tracking.

```sql
CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

No keys are tracked; the table survives as a place to put one.

**Do not put a schema version here.** `sqlx` maintains one in `_sqlx_migrations`, and a
second hand-written copy is what drifts — see § Migrations.

### sessions

```sql
CREATE TABLE sessions (
  -- (environment, agent, native_id) is the identity. Stored as three columns so
  -- each is queryable, with a composite primary key so the triple is the row.
  environment  TEXT NOT NULL,
  agent        TEXT NOT NULL,
  native_id    TEXT NOT NULL,

  workdir          TEXT NOT NULL,
  title            TEXT,
  inject_identity  INTEGER NOT NULL,   -- 0/1; a default at creation, never patched
  fidelity         TEXT NOT NULL CHECK (fidelity IN ('stream','blocks','messages')),
  flags_json       TEXT NOT NULL DEFAULT '{}',
  status           TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle','running')),
  parent_session   TEXT,               -- encoded triple; delegation or fork ancestor
  created_at_ms    INTEGER NOT NULL,
  updated_at_ms    INTEGER NOT NULL,

  PRIMARY KEY (environment, agent, native_id)
);

CREATE INDEX sessions_updated_idx ON sessions(updated_at_ms DESC);
CREATE INDEX sessions_workdir_idx ON sessions(environment, workdir);
```

**There is no Sylph-minted id.** `docs/architecture.md` § Identity carries the
argument and the measurement; the consequence for this table is that a row exists only
once an agent has assigned the session an id, so `POST /sessions` starts an agent
rather than reserving a row. The "draft session with no work in it" concept is gone,
and with it the empty-session delete endpoint.

**A composite primary key rather than one encoded text column**, because every listing
filters on `environment` and most group by `workdir` within it. The encoded form
(`<env>:<agent>:<native>`, no `/` — see `docs/architecture.md`) exists for a URL path
segment and for `parent_session`, not as the storage shape.

Fields worth a note:

- `environment` — the opaque id minted at pairing (`docs/environments.md`), never a
  hostname. A name in a key means renaming destroys identity.
- `inject_identity` — whether `SOUL.md` is appended. Settable at creation, **refused
  afterwards**: a transcript records what produced it, so rewriting the flag would
  rewrite what the stored history means.
- `fidelity` — how much resolution the source had, `stream` | `blocks` | `messages`.
  A property of the record, not of who started the session: a session Sylph launched,
  read back from the vendor's store after the process died, is `messages` too. A
  renderer reads it to decide whether it may draw a duration.
- `parent_session` — one column for two different relationships, which the row it
  points at disambiguates. A delegation folds into its parent as a count; a **fork**
  replays its origin's entire prefix and continues past it, so the two share every
  early user turn and read as one conversation resumed. A listing must not show both
  under one label.

**There is no session class and no incarnation table**, which is a design decision
rather than an omission: `docs/architecture.md` § One class of session is authoritative.
The consequence for this table is that nothing records who started a session, and nothing
may replace a session's agent-native id — that id is part of the primary key, so a
mid-life reset would change the row's identity.

### turns

**Every child table carries the triple, not a surrogate.** A composite foreign key is
more verbose than `session_id TEXT REFERENCES sessions(id)`, and the verbosity is the
point: a surrogate key on `sessions` would have to be generated, which reintroduces
exactly the second id space `docs/architecture.md` § Identity deletes — and with it the
comparison that is silently always false. Three columns that cannot disagree with the
parent beat one that can.

**This rule stops at the database boundary, and `history.db` therefore keeps a
single-column encoded `session_id`** (`docs/storage.md` § Schema). The reason is a
property of SQLite rather than a preference: **a foreign key names a table in its own
database, so a cross-database reference is accepted at `CREATE TABLE` and then fails at
runtime.** Verified 2026-08-28 — declaring `REFERENCES sessions(environment, agent,
native_id)` on a table in an attached `history` database creates cleanly, and the first
insert answers `no such table: history.sessions`, because resolution looked for
`sessions` beside the child rather than in the journal. `PRAGMA foreign_key_check`
reports nothing, so the declaration reads as enforced while enforcing nothing.

That is worse than an unavailable feature, because the DDL looks correct. Since the
constraint cannot exist across the split, the verbosity buys nothing there and the three
columns would be three chances to disagree with no parent to check them against. The
encode boundary sits at the journal↔history calls instead, which is a small number of
sites that a `SessionId` type makes explicit.


Turn execution records.

```sql
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'starting', 'running', 'stopping',
    'completed', 'failed', 'cancelled', 'interrupted'
  )),
  origin TEXT NOT NULL DEFAULT 'user' CHECK (origin IN ('user', 'agent')),
  request_text TEXT NOT NULL DEFAULT '',
  command_invocation_id TEXT,
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
CREATE INDEX turns_status_idx ON turns(status)
  WHERE status IN ('queued', 'starting', 'running', 'stopping');
```

**There is no per-turn exit code**, because an adapter is long-lived and a turn is a
message on its link. A turn's outcome arrives as an event and is recorded in `status` with
`error_code`/`error_message`; nothing reads a process exit.

`idempotency_key` is nullable, so a turn the server originates carries none while
the `UNIQUE(session_env, session_agent, session_native, idempotency_key)` constraint
still de-duplicates every client retry. `command_invocation_id` links a turn to an agent command rather than
to typed text. The partial `turns_status_idx` covers exactly the in-flight states,
which is the query restart reconciliation runs.

`origin` distinguishes a turn a user dispatched from one **the agent started itself**,
which is a real turn rather than a special case — it consolidates, orders by sequence,
and appears in the transcript where it happened. The column exists because a turn
nobody asked for has to be able to say so, and because an agent turn's `request_text`
is empty, which would otherwise be indistinguishable from a user sending nothing. It
is also load-bearing in the supervisor: an agent turn never had a waiter, so
`settle_turn` uses `origin` to tell "no waiter, because there never was one" from "no
waiter, because a deliberate stop already claimed it" — conflating them reported a
user's stop as `LINK_LOST`. `docs/session-protocol.md` § An agent may start a turn.

Status transitions:
- `queued` → `starting` → `running` → `completed` / `failed` / `cancelled`
- `running` → `stopping` → `cancelled`
- Any in-flight status → `interrupted` (on service restart)

### events

Event stream journal (bounded live storage).

```sql
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,                      -- deliberately unconstrained; see below
  phase TEXT,
  stream_id TEXT,
  span TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'stable'
    CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'server'
    CHECK (source IN ('agent', 'server')),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(session_env, session_agent, session_native, seq),
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE
);

CREATE INDEX events_session_idx
  ON events(session_env, session_agent, session_native, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;
CREATE INDEX idx_events_span
  ON events(session_env, session_agent, session_native, span, seq);
```

**`kind` carries no CHECK, deliberately.** A closed list here is a promise to break on
the next adapter that adds a kind, and the breakage is silent in the worst way: the insert
fails while the sequence number is already consumed and the event has already reached live
clients, so a turn streams correctly and comes back empty on reload. The protocol says
kinds are additive (`docs/session-protocol.md`), so validation belongs at the wire boundary
where an unknown kind can be *reported* — not at the storage layer, where it can only be
dropped.

**`source` stays a closed set**, unlike `kind`, because it has exactly two producers —
`agent` and `server` — and a third would be a design change rather than an adapter's
choice.

`span` is which delegated trajectory an event ran in; NULL means the root turn. It is
nullable rather than `''` because absent and "the root" are the same fact. That one
pointer expresses a subagent, a nested subagent, and a workflow's parallel phases,
since those differ only in the shape of the resulting tree — and `idx_events_span` is
what keeps focus mode and single-subtree replay from scanning the session.
`docs/session-protocol.md` § Delegated trajectories is authoritative.

Event lifecycle:
- `ephemeral` events (`started`, `delta`) exist only in live projection
- `stable` events (`completed`) are persisted to journal

Note that `stability` has one derivation, in the `stability` column — not two.
`StreamEvent::is_stable()` reads the field rather than re-computing it from `phase`.
They were equivalent until `subagent` became stable at *every* phase for structural
reasons, after which the method disagreed with the field beside it and a trajectory's
`started` was silently not journaled, flattening every delegated subtree on reload.

The uniqueness of `seq` within a session is what makes SSE resume-by-sequence safe: a
replayed frame cannot be journalled twice. `seq` is per session, not per turn, so
`events_session_idx` orders by it rather than by timestamp — two events written in
the same millisecond still have one defined order.

### pending_requests

What an agent is blocked on, waiting for a human. A resource rather than a stream
message, because the agent waits until someone answers: a client closing its laptop
must not strand it, and the answer has to be possible from a frontend that was not
attached when the request was raised.

```sql
CREATE TABLE pending_requests (
  id TEXT PRIMARY KEY,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  adapter_request_id TEXT NOT NULL,
  kind TEXT NOT NULL,                  -- approval | dialog | elicitation | choice
  tool TEXT,
  tool_use_id TEXT,
  title TEXT NOT NULL DEFAULT '',
  detail_json TEXT NOT NULL DEFAULT '{}',
  options_json TEXT NOT NULL DEFAULT '[]',
  form_json TEXT,                      -- the declared input, when there is one
  default_option TEXT,
  risk TEXT,
  allow_persistent INTEGER NOT NULL DEFAULT 1,
  expires_at_ms INTEGER,
  status TEXT NOT NULL DEFAULT 'pending',  -- pending | resolved | abandoned
  chosen_option TEXT,
  resolved_input_json TEXT,
  resolved_by TEXT,
  created_at_ms INTEGER NOT NULL,
  resolved_at_ms INTEGER,
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE
);

CREATE INDEX pending_requests_open
  ON pending_requests(session_env, session_agent, session_native, created_at_ms)
  WHERE status = 'pending';
CREATE INDEX pending_requests_by_adapter_id
  ON pending_requests(adapter_request_id, status);
```

`pending_requests_open` is partial because resolved rows accumulate without bound and
never appear in the hot "what is this session waiting on" query.

`adapter_request_id` is the adapter's own id, scoped to one link and never serialized
to a client — a client-supplied adapter id would be a way to answer a request that
was never raised. Clients address a request by `id`.

`status = 'pending'` in the resolve UPDATE's WHERE clause is the entire concurrency
control: two clients answering at once both run it, SQLite serializes them, and
exactly one gets a row back. A restart marks every still-pending row `abandoned`
rather than leaving it answerable, because no link survives a restart — but the row
is kept, so the timeline can explain why a turn stopped instead of showing a gap.

`form_json` is the input the request declared it wants collected, and absent is
meaningful rather than missing: a request with no form is an approval, whose subject
is the one proposed tool input in `detail_json`. See `docs/session-protocol.md`
§ Request forms.

## Search lives beside `history.db`

Full-text search is one FTS5 table beside `history.db`, over the same rows the timeline
renders. Native history is parsed by an adapter into the same events a live turn emits, so
it is indexed by the same table — there is no second store and no id translation
(`docs/architecture.md` § One authority).

That is what makes search's coverage statable as a contract: **everything the server
holds is searchable**, including sessions read out of an agent's native files, whether or
not the environment that produced them is still reachable. An index over a subset would
cover whichever sessions some parser happened to reach, and "usually covered" is not a
contract.

`docs/storage.md` § Schema owns the table, § "`blocks_fts` is standalone" owns why it cannot
be an external-content index, and § Tokenization owns the `jieba` tokenizer that makes
Chinese searchable — including why it is linked in rather than loaded.

Two rules about reading any store that is not this one:

**Never query another component's tables.** The vendors' native stores are read through
the agent module that owns that vendor (`docs/agents.md`), never with SQL or ad-hoc file
parsing elsewhere.

**A path filter must be a path match, not a substring match.** Compiling one to
`LIKE '%' || ? || '%'` answers `/w/app-old`, and everything beneath it, when asked for
`/w/app`. Anything meaning "this one directory" compares for equality after normalizing
trailing separators — `sylph_protocol::normalize_workdir` is that comparison, shared so
the adapters and the server cannot disagree.

The same shape starves a paged scan: a parent's pattern matches every path beneath it,
and those sort by recency rather than by depth, so a directory's own first exact match can
land a thousand rows in and an early page cap reports it empty. **Bound such a scan in
rows examined, not pages.**

## Migrations

Numbered SQL migrations applied by `sqlx::migrate!` on startup, in two sets — one per
database:

```
backend/migrations/                 journal.db
  001_initial_schema.sql            everything above: sessions, turns, events, requests

backend/migrations/history/         history.db
  001_history.sql                   blocks, cas_refs, watermarks, session_bytes, blocks_fts
```

`docs/storage.md` § Schema owns the `history/` tables, because they only make sense
alongside the consolidation and retention rules that write them.

**One migration per database, stating the current shape — not a chain that adds columns and
later drops them.** `AGENTS.md` § Compatibility is why: there is no other consumer and no
irreplaceable installed state, so a chain would document intermediate states nobody should
reconstruct. The one precondition before collapsing a chain is to verify a store's data is
re-derivable — for the journal, that means the agents' native stores still hold the
sessions in it.

Attachments and commands need no migration of their own: attachments are staged on disk
under the data directory rather than in a table, and a command invocation is a column on
`turns`.

**`/api/v1/health` reports `MAX(version)` from `_sqlx_migrations`, never a stored key.**

**A number the migration runner already maintains must not have a second hand-written
copy.** A hand-bumped `app_meta.schema_version` drifts the first time a migration forgets
to bump it, and it drifts *silently* because nothing compares the two — so the one question
the field exists to answer, whether a client understands this database's shape, is answered
wrongly with nothing reporting it.

For the same reason the test counts the migrations directory rather than pinning a literal:
a literal in the test is a third copy, and it would agree with a wrong value as happily as
the code did.

Migration rules:
- Each migration is idempotent (use `IF NOT EXISTS`)
- Migrations are append-only (never edit a deployed migration)
- Before deployment, schemas are disposable (change in place). Treat installed
  state, not a date, as the signal that this has flipped

The append-only rule is not merely tidiness: `sqlx` checksums each migration file, so
editing a *deployed* one makes every existing database refuse to open. **"Before
deployment, schemas are disposable" is therefore a genuinely different regime**, not a
softer version of the same rule — and the signal that it has flipped is the presence of
installed state that cannot be re-derived, never a date. While it holds, collapse a chain
rather than growing one.

There are no `sqlx::query!` macros here, only runtime `sqlx::query`, so there is no
`DATABASE_URL` and no `.sqlx` offline directory. A schema mistake surfaces in the
store tests rather than at compile time, which is why those tests matter.
