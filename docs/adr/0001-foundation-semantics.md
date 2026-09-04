# ADR 0001: Foundation semantics for reconnectable environments

- Status: Accepted for the next protocol revision
- Date: 2026-09-04
- Scope: `crates/protocol`; transport and storage implementations follow in later phases

## Context

The current server is an in-process adapter host.  It allocates one `seq` counter per
session (`backend/src/events/seq.rs`) and infers the current turn from the journal when an
adapter event arrives.  A session row also carries one overloaded `parent_session` string,
and adopted native streams have no message carrying the user's original request text.  These
choices work while one process owns every producer, but they are insufficient once an
envagent can disconnect, reconnect, replay a bounded spool, and host more than one source.

The next platform must preserve one server authority without making the envagent a second
history store.  It also has to distinguish a delegation edge from a fork, retain an adopted
turn's input without inventing an event kind, and make the ambiguity after a non-idempotent
command explicit.

## Decisions

### 1. Keep two sequence axes

Every producer item carries:

- `producer_id` and `producer_seq`: allocated and persisted by the envagent producer before
  send.  The server deduplicates by the typed tuple
  `(environment, producer_id, producer_seq)`.  A source cursor may accompany the item, but it
  is provenance and never substitutes for the producer sequence.
- `server_seq`: allocated transactionally by the server after durable ingest.  It is the sole
  canonical order used by the journal, SSE replay, history, and search.

The envagent acknowledges only a contiguous accepted producer prefix.  A reconnect starts at
the first unacknowledged producer sequence; a duplicate is an idempotent no-op, not a second
canonical event.  Timestamps remain provenance (`recorded_at_ms`, `arrival_at_ms`) and never
decide ordering.

This replaces the current single `seq` as a transport contract.  During migration, existing
server sequence values may be re-numbered when the disposable Sylph store is rebuilt; no
compatibility alias is required.

### 2. Model session relationships as explicit server-owned edges

`SessionEdge` has typed `parent`, `child`, and `relation`, where `relation` is `delegation` or
`fork`.  Events are stored exactly once under their owning session.  A parent trace is a graph
read model assembled with source-session provenance; it is not a copied child transcript.

Self-edges are rejected at construction.  Missing endpoints may remain an unresolved read-model
state until the corresponding native rollout is adopted, but an edge must never be silently
reversed or inferred from an id suffix.  The current `parent_session` column is therefore a
temporary compatibility surface inside the pre-wire server only; the protocol and eventual
schema use an edge relation.

### 3. Carry adopted user input as a fact, not an event

Native adapters may emit a `TurnInputFact` in the ordered session message stream.  It carries
the user's request text, optional vendor-recorded time, and optional source cursor.  It is not an
`Event`, has no `EventKind`, and is not rendered as a timeline block.  The server associates it
with the native turn container and persists `turns.request_text`.

Live turns continue to obtain input from the server request path.  This keeps `run`, `watch`, and
`adopt` semantically aligned without fabricating a `user` event that the vendor did not record.

### 4. Make command retry policy and ambiguity typed

Each server-to-envagent command carries an opaque `CommandRequestId` and an
`IdempotencyPolicy`:

- `replayable` permits a reconnect to resend the same request id;
- `at_most_once` forbids blind replay.

The result is one of `accepted`, `rejected`, or `unknown`.  `unknown` is a first-class outcome
for the crash window after a non-idempotent command may have reached the vendor but before its
result reached the server.  The server must not turn that state into either success or failure,
and the envagent must not retry it automatically.

## Alternatives rejected

- **One sequence allocated by the envagent.** Independent producers cannot merge it into a
  deterministic journal order, and a server restart would have no safe way to distinguish a
  replay from a new item.
- **One server sequence echoed back as the delivery cursor.** An envagent cannot know a server
  sequence before sending, and a server-side gap would force it to ask a remote process to infer
  local source progress.
- **One `parent_session` field with a boolean kind.** A string cannot express relation type,
  source provenance, and multiple edges without inference; a boolean would eventually conflate
  delegation and fork again.
- **A synthetic `user` event for adopted input.** It invents timeline history and makes replay
  differ between `run` and `adopt`. A container fact preserves the text while leaving the event
  vocabulary honest.
- **Retry every command with the same request id.** This is safe for an idempotent read but can
  duplicate a turn or a human answer after an ambiguous crash. The policy must travel with the
  command so the runtime cannot guess.

## Consequences

The pure protocol crate now exposes the semantic primitives and their serde forms, while the
wire framing remains a later decision.  The server will need a durable producer-deduplication
record and a transactional server-sequence allocation before the envagent bridge is enabled.
The session graph migration must be serialized with that schema work; Codex and Pi adapters can
continue independently as long as they emit source-specific cursors through the shared shape.

The current in-process path remains valid as a fixture until the local development deployment
is cut over to a real envagent connection.  No raw native record is made a public replay API by
these decisions; raw retention remains an internal diagnostic/re-projection aid.

## Rollback and verification

Before any wire consumer depends on these values, the module can be removed without changing
the existing adapter or server path.  The acceptance gate for this ADR is:

1. serde round trips preserve independent producer and server sequence values, source cursor
   generation/offset, edge relation, adopted input, and command outcomes;
2. empty producer/request/source ids and self-edges are refused;
3. a command with `at_most_once` policy cannot be classified as retryable, and `unknown` is not
   terminal;
4. the full Rust workspace and `cargo doc --workspace --no-deps` pass after each subsequent
   consumer is added.
