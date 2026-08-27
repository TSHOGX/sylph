# Database Schema

Sylph keeps one authoritative record, split across three files for SQLite's sake.

**This file covers `journal.db` only.** The data directory holds three stores, not
one: `journal.db` (hot, unconsolidated turns — below), `history.db` (warm,
consolidated blocks, and the FTS index over them) and `blobs/` beside them. `history.db`'s tables, its migrations
under `backend/migrations/history/`, and the watermark and byte-accounting rows live in
**`docs/storage.md` § Schema**, because they only make sense alongside the
consolidation and retention rules that write them. Splitting one store into three is
SQLite's requirement rather than a data-model choice; `docs/storage.md` § Three tiers
says why.

## Journal (journal.db)

The live journal stores active sessions, turns, and recent events. It uses WAL mode for concurrent access and numbered migrations for schema evolution.

### Tables

#### app_meta

Application metadata and migration tracking.

```sql
CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

No keys are tracked. The table survives as a place to put one, and two former keys
are worth naming so they are not reinvented: `schema_version` was a hand-maintained
copy of a number `sqlx` already keeps in `_sqlx_migrations` and drifted four
migrations behind before anyone noticed, and `instance_id` was never written by any
code at all.

#### sessions

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
  rewrite what the stored history means. This is the one behavioural remnant of the
  deleted `identity_mode`.
- `fidelity` — how much resolution the source had, `stream` | `blocks` | `messages`.
  A property of the record, not of who started the session: a session Sylph launched,
  read back from the vendor's store after the process died, is `messages` too. A
  renderer reads it to decide whether it may draw a duration.
- `parent_session` — one column for two different relationships, which the row it
  points at disambiguates. A delegation folds into its parent as a count; a **fork**
  replays its origin's entire prefix and continues past it, so the two share every
  early user turn and read as one conversation resumed. A listing must not show both
  under one label.

**Four things were deleted here, and the deletion is the design.** `origin`,
`identity_mode`, `lifecycle_mode`, and the whole `backend_incarnations` table are
gone, along with `store::end_incarnation`. *Measured 2026-08-27:* `lifecycle_mode` had
four write sites and **zero** reads, `origin` had zero backend branches, and
`identity_mode` had exactly one — the `SOUL`-path choice now carried by
`inject_identity`. `docs/architecture.md` § One class of session is authoritative for
why a spec had hardened a distinction the implementation never made.

`backend_incarnations` existed to track an agent-native id being replaced by a
lifecycle reset. Nothing scheduled that reset, and under the identity rule above a
reset would replace a session's *primary key* mid-life — so the table is not merely
unused, it describes something the data model no longer permits.

#### turns

**Every child table carries the triple, not a surrogate.** A composite foreign key is
more verbose than `session_id TEXT REFERENCES sessions(id)`, and the verbosity is the
point: a surrogate key on `sessions` would have to be generated, which reintroduces
exactly the second id space `docs/architecture.md` § Identity deletes — and with it the
comparison that is silently always false. Three columns that cannot disagree with the
parent beat one that can.


Turn execution records.

```sql
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE,
  incarnation_id TEXT REFERENCES backend_incarnations(id),
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'starting', 'running', 'stopping',
    'completed', 'failed', 'cancelled', 'interrupted'
  )),
  request_text TEXT NOT NULL DEFAULT '',
  command_invocation_id TEXT,
  idempotency_key TEXT,
  error_code TEXT,
  error_message TEXT,
  runner_exit_code INTEGER,
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  stopped_at_ms INTEGER,
  UNIQUE(session_id, idempotency_key)
);

-- Added by 006_turn_origin.sql
ALTER TABLE turns ADD COLUMN origin TEXT NOT NULL DEFAULT 'user'
  CHECK (origin IN ('user', 'agent'));

CREATE INDEX turns_session_idx ON turns(session_id, created_at_ms DESC);
CREATE INDEX turns_status_idx ON turns(status)
  WHERE status IN ('queued', 'starting', 'running', 'stopping');
```

`idempotency_key` is nullable, so a turn the server originates carries none while
the `UNIQUE(session_id, idempotency_key)` constraint still de-duplicates every
client retry. `command_invocation_id` links a turn to an agent command rather than
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

`runner_exit_code` is a **legacy spelling kept for compatibility**, not evidence the
v1 runner protocol still exists. Under the current protocol the adapter is long-lived
process and a turn is a message on its link, so there is no per-turn exit code to
record; the column is written `NULL` on every current path.

Status transitions:
- `queued` → `starting` → `running` → `completed` / `failed` / `cancelled`
- `running` → `stopping` → `cancelled`
- Any in-flight status → `interrupted` (on service restart)

#### events

Event stream journal (bounded live storage).

```sql
-- As rebuilt by 003_agent_event_vocabulary.sql. 001's shape is not what is on disk:
-- it constrained `kind` to a closed list and spelled the adapter source 'runner'.
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_env    TEXT NOT NULL,
  session_agent  TEXT NOT NULL,
  session_native TEXT NOT NULL,
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL,                      -- deliberately unconstrained; see below
  phase TEXT,
  stream_id TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'stable'
    CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'server'
    CHECK (source IN ('agent', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL,
  reconciled_at_ms INTEGER,
  UNIQUE(session_id, seq)
);

-- Added by 005_event_span.sql
ALTER TABLE events ADD COLUMN span TEXT;

CREATE INDEX events_session_idx ON events(session_id, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;
CREATE INDEX idx_events_span ON events(session_id, span, seq);
```

**`kind` carries no CHECK, and that is the fix rather than an omission.** 001
constrained it to ten values, which is a promise to break on the next adapter that
adds one — and it broke exactly that way: `append_event` logged and continued, so a
sequence number was consumed and the event reached live clients while nothing reached
the journal. A turn streamed correctly and came back empty on reload. The protocol says
kinds are additive, so validation belongs at the wire boundary where an unknown kind
can be *reported*, not at the storage layer where it can only be dropped.
`docs/session-protocol.md` owns the vocabulary.

**`source` is `agent`, not `runner`.** 003 renamed it with the v1 protocol and migrated
existing rows. It stays a closed set because it has exactly three producers and a
fourth would be a design change rather than an adapter's choice.

`span` is which delegated trajectory an event ran in; NULL means the root turn. It is
nullable rather than `''` because absent and "the root" are the same fact. That one
pointer expresses a subagent, a nested subagent, and a workflow's parallel phases,
since those differ only in the shape of the resulting tree — and `idx_events_span` is
what keeps focus mode and single-subtree replay from scanning the session.
`docs/session-protocol.md` § Delegated trajectories is authoritative.

Event lifecycle:
- `ephemeral` events (`started`, `delta`) exist only in live projection
- `stable` events (`completed`) are persisted to journal
- `reconciled_at_ms` is a per-row column that **nothing writes**. Retention is gated on
  a per-session watermark in `history.db` instead (`docs/storage.md` § Retention is
  watermark-driven), so pruning is built and this column is vestigial rather than a
  placeholder for future work

Note that `stability` has one derivation, in the `stability` column — not two.
`StreamEvent::is_stable()` reads the field rather than re-computing it from `phase`.
They were equivalent until `subagent` became stable at *every* phase for structural
reasons, after which the method disagreed with the field beside it and a trajectory's
`started` was silently not journaled, flattening every delegated subtree on reload.

`UNIQUE(session_id, seq)` is what makes SSE resume-by-sequence safe: a replayed
frame cannot be journalled twice. `seq` is per session, not per turn, so
`events_session_idx` orders by it rather than by timestamp — two events written in
the same millisecond still have one defined order.

#### pending_requests

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
  FOREIGN KEY (session_env, session_agent, session_native)
    REFERENCES sessions(environment, agent, native_id) ON DELETE CASCADE,
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
  resolved_at_ms INTEGER
);

CREATE INDEX pending_requests_open
  ON pending_requests(session_id, created_at_ms) WHERE status = 'pending';
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

## Search lives beside `history.db`, not in a second store

There is no `archive.db`. One vocabulary and one store (`docs/architecture.md` § One
authority) means native history is parsed by an adapter into the same events a live
turn emits, lands in the same journal → history pipeline, and is indexed by one FTS5
table beside `history.db`.

`docs/storage.md` § Schema owns that table. What belongs here is what it replaced and
why the replacement is not merely tidier:

- **The old open question is answered by construction.** Full-text search used to
  cover only what the separate archive had parsed, so a session whose environment had
  gone away was searchable nowhere and one whose vendor file happened to be local was
  searchable by coincidence. "Usually covered" is not a contract.
- **`backend/src/tracedb/mod.rs` goes away whole** — 1440 lines, including
  per-operation JSON casing pins that existed because the same archive spelled `show`
  in snake_case and `list`/`search`/`stats` in camelCase.
- **The two-id-space hazard cannot be written.** See `docs/storage.md` § One id space.

Two rules that outlive the separate store, because they are about reading *any*
external store rather than about that one:

**Never query another component's tables.** The parsers now live in Sylph's own agent
modules (`docs/agents.md`), so this applies to the vendors' native stores: read them
through the agent module, never with SQL or ad-hoc file parsing elsewhere.

**A path filter must be a path match, not a substring match.** The old archive
compiled a `cwd` filter to `LIKE '%' || ? || '%'`, so asking for `/w/app` also
returned `/w/app-old` and everything beneath it. Anything meaning "this one directory"
compares for equality after normalizing trailing slashes. The same shape starved a
paged scan: because a parent's pattern matched every path beneath it and those sorted
by recency rather than depth, a directory with 88 sessions of its own reported empty
because its first exact match landed ~1300 rows in. Bound such a scan in **rows
examined**, not pages.

## Migrations

Numbered SQL migrations in `backend/migrations/`, applied by `sqlx::migrate!` on
startup:

```
001_initial_schema.sql         -- rebuilt: the identity triple, no origin/lifecycle
002_pending_requests.sql       -- what an agent is blocked on
003_agent_event_vocabulary.sql
004_request_affordances.sql    -- allow_persistent, expires_at_ms
005_event_span.sql             -- span, for a delegated trajectory's parent
006_turn_origin.sql            -- origin, for an agent-initiated turn
007_request_form.sql           -- form_json
```

**`001` is rewritten in place rather than followed by a migration that drops columns**,
which the append-only rule below would normally forbid. `AGENTS.md` § Compatibility is
why: there is no other consumer, no irreplaceable installed state, and every existing
journal is re-derivable from the agents' own native stores. A migration chain that
adds three columns and then deletes them documents an intermediate state nobody should
reconstruct. Verify the native sources still exist before discarding a journal — that
is the one precondition, and it is the same one `AGENTS.md` states.

Attachments and commands need no migration of their own: attachments are staged on
disk under the data directory rather than in a table, and a command invocation is a
column on `turns`.

**`/api/v1/health` reports `MAX(version)` from `_sqlx_migrations`, not a stored key.**
It used to read `app_meta.schema_version`, which migrations bumped by hand: `001`
wrote `1`, `003` wrote `3`, and `004` through `007` forgot. So a fully-migrated
journal published `3` while seven migrations had run, and the one question the field
exists to answer — whether a client understands this journal's shape — was answered
wrongly for four migrations without anything reporting it.

The rule this settles: a number the migration runner already maintains must not have
a second hand-written copy. The copy is what drifts, and it drifts silently because
nothing compares the two. The integration test now counts the `migrations/` directory
rather than pinning a literal, since a literal in the test is a third copy and would
have agreed with the wrong value just as happily as the code did.

Migration rules:
- Each migration is idempotent (use `IF NOT EXISTS`)
- Migrations are append-only (never edit a deployed migration)
- Before deployment, schemas are disposable (change in place). Treat installed
  state, not a date, as the signal that this has flipped

The append-only rule is not merely tidiness here: `sqlx` checksums each migration
file, so editing a deployed one makes every existing journal refuse to open. That is
why `004` and `005` add columns to the table `002` created rather than amending it.

There are no `sqlx::query!` macros here, only runtime `sqlx::query`, so there is no
`DATABASE_URL` and no `.sqlx` offline directory. A schema mistake surfaces in the
store tests rather than at compile time, which is why those tests matter.
