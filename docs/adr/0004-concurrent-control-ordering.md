# ADR 0004: Concurrent control requests are server-ordered

- Status: Accepted
- Date: 2026-09-07

## Decision

Observation is concurrent. Control requests from multiple authorized clients may be submitted
concurrently; the server accepts, assigns a session sequence, persists, and then dispatches them.
There is no controller owner, takeover, or handoff protocol in the initial product.

Every request has a stable request id and idempotency key. Retries cannot execute an accepted
request twice. The server defines the order of send, stop, cancel, and approval operations. Queue
support is explicit: when the adapter cannot queue a request, the server returns a typed reason
rather than pretending it was accepted. Approval resolution is first-valid-wins.

The record retains `request_id`, `server_seq`, `accepted_at`, and `origin_subject`; the subject is
audit provenance, not ownership.

## Intent and delivery

`control_requests` is the user-facing ledger for send, steer, queue, stop and approval.
Each accepted intent retains its original text, Subject, target turn, creation time,
revision, idempotency key, and a monotonic session-local server sequence. The vendor
command receipt is a separate field; a successful RPC alone does not prove that a
queued message was consumed. Reusing an idempotency key with a different intent is a
conflict. Equal text submitted with different keys is two messages.

Pending follow-ups wait in the server queue in server sequence order. This keeps
editing and cancellation under the same authority observed by every browser. Queue
support remains an explicit adapter capability. The dispatcher starts the oldest
follow-up only when the session has no active turn. Edits, cancellation and conversion
to immediate steering require the current revision and the same session command lock
as dispatch. Dispatched text is immutable. A correction after dispatch is a new intent.
The immutable submission sequence also defines queue order; arbitrary reordering is
not offered.

State transitions are `accepted -> queued -> dispatched -> confirmed`, with direct
dispatch for send, steer, stop and approval. An explicit vendor consumption fact may later mark an intent `consumed`. Rejections and failures retain their
reason; cancelled queue items remain auditable. A transport timeout after dispatch
is `unconfirmed` and cannot be retried as a fresh vendor operation automatically. Server
restart retains unsubmitted queued work and reports ambiguous in-flight delivery.

The composer clears only after the server confirms acceptance. Clients recover the
same ledger on reconnect and receive invalidations on the session notification stream.
User transcript events carry the request identity. Correlation to a vendor echo uses
vendor identifiers or explicit protocol position, never text equality.

## Turn leases and receipts

A turn id is a unique, non-reusable generation within its session. The envagent binds
that generation to the vendor turn accepted by `send`, and invalidates the binding on
adapter turn completion or stream loss. An active journal row is only a request target;
it does not authorize delivery. Steer must match the envagent's lease and the adapter
must enforce the captured vendor turn identity atomically at the vendor boundary.
Vendors without that primitive expose their native priority queue as QueueSteering
instead of advertising turn-scoped Steer. A local open-state check cannot
make an unconditional vendor queue insertion safe.

Send and steer intents enter the ledger before dispatch. Their text enters the transcript
only after a receipt confirms acceptance for the targeted generation. A refusal retains
only the rejected ledger entry; an unknown outcome remains ambiguous and is never retried
automatically. Receipt persistence and transcript publication precede lifecycle settlement
under the same session command sequencer, also used by stop and queue changes. Receipt
frames are read independently of event ingestion so a lifecycle event waiting on that
sequencer cannot prevent the command holding it from receiving its acknowledgement.

Native priority and follow-up queues retain the harness's own scheduling. Their receipts
confirm queue insertion only and have no target turn. The ledger labels consumption as
unconfirmed when the vendor supplies no correlatable receipt; text equality is never used
to infer it. User messages observed on the vendor stream enter their actual arrival turn.
Native queue entries are immutable here after dispatch; server-owned queue editing and
cancellation apply only before Sylph dispatches a new turn.

## Consequences

Clients consume the same session resource and event stream and converge on accepted, queued,
running, or rejected state. Concurrency tests must use independent browser contexts and cover
retries, stop/send ordering, and simultaneous approvals.
