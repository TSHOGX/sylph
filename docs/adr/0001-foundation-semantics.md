# ADR 0001: Foundation semantics for reconnectable environments

- Status: Accepted for command retry policy; trace decisions superseded by the unified trace foundation
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

Producer receipt identity remains a separate durable axis, but server admission is not
semantic reading order. Source positions, causal/history order and view revisions are
separate under [the unified foundation](../unified-trace-foundation.md#ordering-clocks-and-measurements)
and [evidence admission](../unified-trace-storage.md#admission-and-acknowledgments).

### 2. Ancestry is a typed server-owned edge

Typed ancestry remains required. The complete model distinguishes spawn, interaction,
history predecessor and context lineage, including embedded child references and branch
selection. See [the unified foundation](../unified-trace-foundation.md#ownership-topology-interaction-and-context).

### 3. Adopted user input is a fact, not an event

The bare `TurnInputFact`/turn-request-text decision is superseded by identified Message
revisions and Delivery facts. Submitted human content is immutable and independent of
vendor wrapping, queue acceptance and model inclusion. See
[the unified foundation](../unified-trace-foundation.md#messages-deliveries-and-model-visibility).

### 4. Command retry policy and ambiguity are typed

Each server-to-envagent command carries an opaque `CommandRequestId` and an
`IdempotencyPolicy` — `replayable` permits a reconnect to resend the same request id,
`at_most_once` forbids blind replay. The result is `accepted`, `rejected`, or `unknown`.

`unknown` is a first-class outcome for the crash window after a non-idempotent command may
have reached the vendor but before its result reached the server. The server must not
convert it into success or failure, and the envagent must not retry it automatically.

## Alternatives rejected

The following alternatives belong to the original decision. Input-as-turn-text and its
rejection of identified Message facts are superseded by the unified foundation.

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

This section records the implementation of the original ADR, not the target trace
contract. Replacement status lives in [the roadmap](../roadmap.md#unified-trace-foundation-review).

All four decisions are types in `crates/protocol` (`semantic.rs`, `env.rs`), consumed by
`crates/envagent/src/runtime.rs` and the server's ingest and edge store. Each is pinned by a
test named after the property it holds: `producer_and_server_sequences_are_distinct_axes`,
`session_edges_name_relation_and_refuse_self_edges`,
`event_batches_require_one_contiguous_producer_prefix`, and
`command_policy_and_unknown_outcome_are_explicit`.

The edge store has a producer: adopting a session journals the edge its vendor recorded
(`record_adopted_lineage` in `crates/server/src/api/sessions.rs`). Before that it had a table, a
route, and a wire message but nothing that wrote a row, so the decision was pinned by types
and unreached by data — and the superseded column was still being written from all three
adapters while this document claimed it held `None`. A write-only field is
indistinguishable from a working one, which is how it survived the decision that deleted it.

No raw native record becomes a public replay API here; raw retention stays an internal
re-projection aid ([ADR-0005](0005-raw-retention.md)).
