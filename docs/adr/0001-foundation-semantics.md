# ADR 0001: Foundation semantics for reconnectable environments

- Status: Accepted
- Date: 2026-09-04
- Scope: `crates/protocol`, and the server ingest and schema that consume it

## Context

An envagent can disconnect, reconnect, replay a bounded spool, and host more than one
source. Four things a single-process adapter host could leave implicit become ambiguous
under that: which sequence axis a delivery cursor speaks, whether an ancestry pointer means
delegation or fork, where an adopted turn's request text lives, and what a command's outcome
is when a crash lands between dispatch and result.

Each is settled below as a type in `crates/protocol`, so the ambiguity cannot be
re-introduced by a runtime that guesses.

## Decisions

### 1. Two sequence axes

Every producer item carries `producer_id` and `producer_seq`, allocated and persisted by the
envagent **before** send; the server deduplicates on `(environment, producer_id,
producer_seq)`. `server_seq` is allocated transactionally by the server after durable
ingest, and is the sole canonical order for the journal, SSE replay, history, and search.

The envagent acknowledges only a contiguous accepted producer prefix, so a reconnect starts
at the first unacknowledged item and a duplicate is an idempotent no-op rather than a second
canonical event. A source cursor may accompany an item as provenance and never substitutes
for the producer sequence. Timestamps are provenance too and never decide ordering.

### 2. Ancestry is a typed server-owned edge

`SessionEdge` has typed `parent`, `child`, and `relation`, where `relation` is `delegation`
or `fork`. Self-edges are rejected at construction. Events are stored exactly once under
their owning session, so a parent trace is a graph read model assembled with source-session
provenance rather than a copied child transcript.

An edge is never inferred from an id suffix and never silently reversed. A missing endpoint
may remain an unresolved read-model state until the corresponding native source is adopted.

The `sessions.parent_session` column is residue from the single-pointer model and is to be
deleted, not read: a string cannot carry relation type, provenance, and multiple edges at
once, which is the conflation this decision removes.

### 3. Adopted user input is a fact, not an event

A native adapter may emit a `TurnInputFact` in the ordered session message stream, carrying
the request text, an optional vendor-recorded time, and an optional source cursor. It is not
an `Event`, has no `EventKind`, and is not rendered as a timeline block; the server
associates it with the native turn container and persists `turns.request_text`.

Live turns obtain input from the server request path, so `run`, `watch`, and `adopt` stay
aligned without fabricating a `user` event the vendor never recorded.

### 4. Command retry policy and ambiguity are typed

Each server-to-envagent command carries an opaque `CommandRequestId` and an
`IdempotencyPolicy` — `replayable` permits a reconnect to resend the same request id,
`at_most_once` forbids blind replay. The result is `accepted`, `rejected`, or `unknown`.

`unknown` is a first-class outcome for the crash window after a non-idempotent command may
have reached the vendor but before its result reached the server. The server must not
convert it into success or failure, and the envagent must not retry it automatically.

## Alternatives rejected

- **One sequence allocated by the envagent.** Independent producers cannot merge it into a
  deterministic journal order, and a server restart could not distinguish a replay from a
  new item.
- **One server sequence echoed back as the delivery cursor.** An envagent cannot know a
  server sequence before sending, so a server-side gap would force it to ask a remote
  process to infer local source progress.
- **One ancestry field with a boolean kind.** A boolean eventually conflates delegation and
  fork again, which is the failure that motivated the edge.
- **A synthetic `user` event for adopted input.** It invents timeline history and makes
  replay differ between `run` and `adopt`.
- **Retrying every command with the same request id.** Safe for an idempotent read, but it
  can duplicate a turn or a human answer after an ambiguous crash, so the policy travels
  with the command rather than being guessed by the runtime.

## Where this landed

All four decisions are types in `crates/protocol` (`semantic.rs`, `env.rs`), consumed by
`crates/envagent/src/runtime.rs` and the server's ingest and edge store. Each is pinned by a
test named after the property it holds: `producer_and_server_sequences_are_distinct_axes`,
`session_edges_name_relation_and_refuse_self_edges`,
`event_batches_require_one_contiguous_producer_prefix`, and
`command_policy_and_unknown_outcome_are_explicit`.

`sessions.parent_session` still exists in `001_initial_schema.sql` and is written as `None`.
It is residue awaiting deletion, not a second ancestry source.

No raw native record becomes a public replay API here; raw retention stays an internal
re-projection aid ([ADR-0005](0005-raw-retention.md)).