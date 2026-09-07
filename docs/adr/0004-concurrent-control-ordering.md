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

## Consequences

Clients consume the same session resource and event stream and converge on accepted, queued,
running, or rejected state. Concurrency tests must use independent browser contexts and cover
retries, stop/send ordering, and simultaneous approvals.
