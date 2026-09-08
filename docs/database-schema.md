# Database Schema

**This file covers `journal.db`** — the hot tier, holding sessions, turns, and the events
of turns still in flight. It states the shape and the reasoning; the DDL itself lives in
`backend/migrations/`, applied by `sqlx::migrate!` at startup.

One authoritative record is split across three stores for SQLite's sake rather than as a
data-model choice (`docs/storage.md` § Three tiers): `journal.db` here, `history.db` for
consolidated blocks and the full-text index, and `blobs/` beside them.
**`docs/storage.md` owns the other two**, because their tables only make sense alongside
the consolidation and retention rules that write them.

WAL mode, for concurrent readers against a live writer.

## Identity in the schema

`(environment, agent, native_id)` is a session's identity and its primary key — three
columns, so each is queryable, with a composite key so the triple is the row. There is no
Sylph-minted id (`docs/architecture.md` § Identity), so a row exists only once an agent has
assigned the session an id, which is why `POST /sessions` starts an agent rather than
reserving a row.

A composite key rather than one encoded text column, because every listing filters on
`environment` and most group by `workdir` within it. The encoded form
(`<env>:<agent>:<native>`, no `/`) exists for a URL path segment and for ancestry
references, not as the storage shape.

**Every child table in the journal carries the triple, not a surrogate.** A surrogate key
on `sessions` would have to be generated, which reintroduces the second id space identity
deletes — and with it the comparison that is silently always false. Three columns that
cannot disagree with the parent beat one that can.

**That rule stops at the database boundary, and `history.db` therefore keeps a
single-column encoded `session_id`.** A foreign key names a table in its own database, so
a cross-database reference is accepted at `CREATE TABLE` and fails at runtime — verified
2026-08-28: declaring `REFERENCES sessions(environment, agent, native_id)` from a table in
an attached `history` database creates cleanly, and the first insert answers `no such
table: history.sessions`. `PRAGMA foreign_key_check` reports nothing, so the declaration
reads as enforced while enforcing nothing. Since the constraint cannot exist across the
split, three columns there would be three chances to disagree with no parent to check them
against. The encode boundary sits at the journal↔history calls, which a `SessionId` type
makes explicit.

## sessions

Holds `workdir`, `title`, `inject_identity`, `fidelity`, `status` (`idle` | `running`),
ancestry, and timestamps. Indexed by recency, and by `(environment, workdir)` for
grouping.

- `environment` is the opaque id minted at pairing, never a hostname.
- `inject_identity` is settable at creation and **refused afterwards**: a transcript
  records what produced it, so rewriting the flag would rewrite what the stored history
  means.
- `fidelity` is `stream` | `blocks` | `messages`, a property of the record rather than of
  who started the session. A renderer reads it to decide whether it may draw a duration.

**There is no session class and no incarnation table** (`docs/architecture.md` § One class
of session). Nothing records who started a session, and nothing may replace a session's
native id — that id is part of the primary key, so a mid-life reset would change the row's
identity.

Ancestry is an explicit edge table rather than a column, because delegation and fork are
two relationships and one column would need the row it points at to disambiguate them. A
delegation folds into its parent as a count; a **fork** replays its origin's entire prefix
and continues past it, so the two share every early user turn and read as one conversation
resumed. A listing must not show them under one label. Events stay under the session that
owns them, so a trace reader joins the edge table rather than copying a child transcript
into its parent. Self-edges are rejected by the protocol type.

**Where a native record carries both a parent and a fork origin, the parent wins.** A
delegate may name the origin it was branched from, so the two pointers coexist and only the
parent describes how the session came to exist. An adapter resolving this the other way
produces a `fork` edge for a real delegation, which reads as a plausible tree.

## turns

A turn's status is one of `queued`, `starting`, `running`, `stopping`, `settling`,
`completed`, `failed`, `cancelled`, `interrupted`, with these transitions:

- `queued` → `starting` → `running` → `settling` → `completed` / `failed` / `cancelled`
- `running` → `stopping` → `settling` → `cancelled`
- any in-flight status, `settling` included → `interrupted` (on service restart)

**There is no per-turn exit code**, because an adapter is long-lived and a turn is a
message on its link. A turn's outcome arrives as an event and is recorded in `status` with
`error_code` / `error_message`; nothing reads a process exit.

`close_reason` answers a different question from `status`: the status says what the record
became, the reason says what cut it short (`docs/session-protocol.md` § Outcomes). It is
`NULL` exactly when a turn ran to its own end, and required whenever one stopped short — a
turn that ends early with no reason is indistinguishable from one that finished. Its
`CHECK` rejects status names, including `abandoned`, which keeps a reason from restating
its own status and keeps `abandoned` at the level that has it: a trajectory outcome, never
a `turns.status`.

`idempotency_key` is nullable, so a turn the server originates carries none while the
uniqueness constraint over `(session triple, idempotency_key)` still de-duplicates every
client retry. `command_invocation_id` links a turn to an agent command rather than to typed
text. A partial index covers exactly the in-flight statuses, which is the query restart
reconciliation runs.

`origin` distinguishes a turn a user dispatched from one **the agent started itself**. An
agent turn is a real turn — it consolidates, orders by sequence, and appears in the
transcript where it happened — and its `request_text` is empty, which would otherwise be
indistinguishable from a user sending nothing. It is load-bearing in the supervisor too: an
agent turn never had a waiter, so `settle_turn` uses `origin` to tell "no waiter, because
there never was one" from "no waiter, because a deliberate stop already claimed it".
Conflating those reported a user's stop as `LINK_LOST`.

### A terminal status means the record is finished

**`settling` is what makes the four terminal statuses worth acting on.** A turn passes
through it holding a known outcome and an unfinished record: the settle sweep still has to
close whatever trajectories the turn left open, journal them, and flush the writer's
buffered deltas. Only then does the status become terminal.

So a client that reads `completed` may refetch history and find everything — no grace
period, no waiting to see whether more arrives, no second signal to correlate. That is a
guarantee about this column, and `settling` is the only reason it can be one.

It replaced a single write placed *before* the sweep, which made `status` answer two
questions at once — *what did this turn produce* and *is its record finished* — with no
field that could separate them. Every reader in that window saw a terminal turn over a
partial record and had no way to know it. Measured on the wire: a client ended its claim
there, tore down the stream before the sweep's own events arrived, and refetched a journal
the sweep had not written; a delegation the server had closed rendered as running forever.
At natural speed the sweep usually won, so the failure presented as one test in three
flaking on a *different* assertion each run.

`crate::supervisor::TurnCloser::close` owns the ordering and
`a_terminal_turn_always_has_its_trajectories_closed` pins it. The cost is that a crash
between the two writes leaves a turn whose outcome is known and unrecorded; the restart
sweep reports it `interrupted`, keeping the end time the claim stamped.

## events

One append-only stream per session, carrying `kind`, `phase`, `stream_id`, `span`,
`interior_state`, text, a JSON payload, an error flag, `stability`, `source`, and three
time axes. Indexed by session sequence, by turn, and by `span`.

**`kind` carries no CHECK, deliberately.** A closed list is a promise to break on the next
adapter that adds a kind, and the breakage is silent in the worst way: the insert fails
while the sequence number is already consumed and the event has already reached live
clients, so a turn streams correctly and comes back empty on reload. Kinds are additive
(`docs/session-protocol.md`), so validation belongs at the wire boundary where an unknown
kind can be *reported* rather than at the storage layer where it can only be dropped.

**`source` stays a closed set**, unlike `kind`, because it has exactly two producers —
`agent` and `server` — and a third would be a design change rather than an adapter's
choice.

`span` is which delegated trajectory an event ran in; NULL means the root turn. Nullable
rather than `''`, because absent and "the root" are the same fact. That one pointer
expresses a subagent, a nested subagent, and a workflow's parallel phases, since those
differ only in the shape of the resulting tree, and its index is what keeps focus mode and
single-subtree replay from scanning the session. `docs/session-protocol.md` § Delegated
trajectories is authoritative.

`ephemeral` events (`started`, `delta`) exist only in live projection; `stable` events are
journalled. **`stability` has one derivation, the column** — `StreamEvent::is_stable()`
reads the field rather than recomputing it from `phase`. The two were equivalent until
`subagent` became stable at *every* phase for structural reasons, after which the method
disagreed with the field beside it and a trajectory's `started` was silently not
journalled, flattening every delegated subtree on reload.

Uniqueness of `seq` within a session is what makes SSE resume-by-sequence safe: a replayed
frame cannot be journalled twice. `seq` is per session rather than per turn, so ordering is
by sequence rather than by timestamp — two events written in the same millisecond still
have one defined order.

## pending_requests

What an agent is blocked on, waiting for a human. A resource rather than a stream message,
because the agent waits until someone answers: a client closing its laptop must not strand
it, and the answer has to be possible from a frontend that was not attached when the
request was raised.

A row carries the request's kind (`approval` | `dialog` | `elicitation` | `choice`), the
tool and tool-use id it concerns, the declared options and form, a default, a risk marker,
an expiry, and its resolution. A partial index covers open rows only, since resolved rows
accumulate without bound and never appear in the hot "what is this session waiting on"
query.

`adapter_request_id` is the adapter's own id, scoped to one link and never serialized to a
client — a client-supplied adapter id would be a way to answer a request that was never
raised. Clients address a request by `id`.

**`status = 'pending'` in the resolve UPDATE's WHERE clause is the entire concurrency
control.** Two clients answering at once both run it, SQLite serializes them, and exactly
one gets a row back. A restart marks every still-pending row `abandoned` rather than
leaving it answerable, because no link survives a restart — and the row is kept, so the
timeline can explain why a turn stopped instead of showing a gap.

`form_json` is the input the request declared it wants collected, and absent is meaningful
rather than missing: a request with no form is an approval, whose subject is the one
proposed tool input (`docs/session-protocol.md` § Request forms).

## Subjects, environments, and grants

`subjects` holds one row per authenticated actor
([ADR-0002](adr/0002-environment-workspace-subject.md)); the bootstrap holds exactly one.

`is_admin` is the server administration authority — the party ADR-0006 assigns as the
authority root for every `grant_authority = server` environment. It is a column here rather
than a grant row for two independent reasons: grants are keyed per environment and this
authority is not, and a grantable `administer` would restore the escalation chain ADR-0006
removes. Nothing sets it over the wire, so an admin cannot mint another admin. It confers no
`observe` or `control` on any environment — an admin who wants to read a session grants
themselves one, which leaves a grant row, since bundling reads into the administration
authority would make the audit surface the largest unlogged reader.

**Environment installations and pairings** are server-owned. Pairing codes are stored as
hashes and consumed atomically; the bearer token itself is never written to the journal. A
reconnect is accepted only for the current connection epoch. An installation carries
`grant_authority`, `machine` or `server`, derived from pairing provenance and immutable
thereafter — it records where authorization legitimately originates, and a value that could
be re-pointed later would record nothing.

Unpairing removes the installation and its lease **while leaving every recorded session
readable** (`docs/environments.md` § Unpairing is the terminal operation).

**Grants** are one row per `(subject, environment)` with a capability of `observe` or
`control`, plus the provenance of the local signing act that issued it, its expiry, and its
consumption state. `administer` never appears, because a row asserting it would be a grant
of the power to grant.

Expiry is what makes the link's `expired` distinguishable from `not_granted`: an expired
row is still present and still proves the grant was once legitimate, so the remedy is a
renewal rather than a first issuance. Revocation deletes the row, and a deleted row cannot
be distinguished from one never issued — which is why "revoked" survives on the turn it cut
short, as `close_reason = grant_revoked`, rather than in the grant table.

Invite codes share the pairing code's hashing and atomic consumption, and are validated on
five properties: environment scope, capability, expiry, single consumption, and issuer
provenance. Scope is stored rather than inferred from the redeeming connection, so a grant
asserts only over the environment that issued it.

**Event receipts** are the durable deduplication boundary for envagent delivery.
`producer_seq` is allocated and spooled on the environment; `server_seq` is allocated by the
server in the same transaction that inserts the canonical event. A payload hash makes reuse
of a producer sequence with different bytes a hard error rather than a second
interpretation of one event.

## Search lives beside history.db

Full-text search is one FTS5 table beside `history.db`, over the same rows the timeline
renders. Native history is parsed by an adapter into the same events a live turn emits, so
it is indexed by the same table.

That is what makes coverage statable as a contract: **everything the server holds is
searchable**, including sessions read out of an agent's native files, whether or not the
environment that produced them is still reachable. `docs/storage.md` owns the table, why it
cannot be an external-content index, and the `jieba` tokenizer that makes Chinese
searchable.

Two rules about reading any store that is not this one:

**Never query another component's tables.** The vendors' native stores are read through the
agent module that owns that vendor (`docs/agents.md`), never with SQL or ad-hoc file
parsing elsewhere.

**A path filter must be a path match, not a substring match.** Compiling one to
`LIKE '%' || ? || '%'` answers `/w/app-old`, and everything beneath it, when asked for
`/w/app`. Anything meaning "this one directory" compares for equality after normalizing
trailing separators — `sylph_protocol::normalize_workdir` is that comparison, shared so the
adapters and the server cannot disagree.

The same shape starves a paged scan: a parent's pattern matches every path beneath it, and
those sort by recency rather than by depth, so a directory's own first exact match can land
a thousand rows in and an early page cap reports it empty. **Bound such a scan in rows
examined, not pages.**

## Schema evolution

Numbered SQL files, one set per database, applied by `sqlx::migrate!` at startup:
`backend/migrations/` for the journal and `backend/migrations/history/` for history.

**Schemas change in place.** The store is disposable: a build that cannot read one refuses
to start and names it, and the answer is to discard `data/` and re-adopt from the vendors'
own native stores. So the chain gets collapsed rather than grown. `sqlx` checksums each
file, so editing one in a chain a running database has already applied makes that database
refuse to open — collapse the chain rather than editing a member of it, and use
`IF NOT EXISTS` where the operation supports it.

Attachments and commands need no table of their own: attachments are staged on disk under
the data directory, and a command invocation is a column on `turns`.

**`/api/v1/health` reports `MAX(version)` from `_sqlx_migrations`, never a stored key**, and
the test counts the directory rather than pinning a literal. A number the runner already
maintains cannot have a second copy, since nothing would compare the two.

There are no `sqlx::query!` macros here, only runtime `sqlx::query`, so there is no
`DATABASE_URL` and no `.sqlx` offline directory. A schema mistake surfaces in the store
tests rather than at compile time, which is why those tests matter.