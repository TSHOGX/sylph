# ADR 0001: Durable local runtime boundaries

## Status

Accepted for the backend foundation.

## Decisions

1. A product session is distinct from an agent-native backend incarnation. A session may therefore have multiple incarnations after a Sylph lifecycle reset while native sessions retain their original identity and lifecycle policy.
2. A turn is a durable resource owned by `TurnSupervisor`, not by the HTTP request that created it. Creation returns `202`; clients consume a replayable SSE stream and may reconnect after browser failure.
3. `live.sqlite3` is a bounded runtime journal. TraceDB remains the stable-history authority and is accessed only through its public JSON API.
4. Internal runner NDJSON remains additive to the legacy final envelope. Browsers consume normalized SSE; vendor event shapes do not cross the API boundary.
5. The service remains loopback-first with a process-local HttpOnly/SameSite cookie, Host validation, and mutation Origin checks.

## Consequences

The schema uses numbered transactional migrations and explicitly records turn interruption on service restart. The current compatibility routes are retained while `/api/v1` becomes the frontend contract. Trace reconciliation, attachment lifecycle, commands, approvals, and release packaging remain subsequent roadmap work; they must preserve these boundaries.
