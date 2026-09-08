# ADR 0003: Use the envagent for every product execution path

- Status: Accepted
- Date: 2026-09-07

## Decision

The formal product path is always:

`Client → Server → EnvBridge → Envagent → Adapter → Vendor CLI/native store`.

The machine hosting the server is not a privileged exception: using it as an execution
environment means running an envagent there and pairing it, so a single-machine install is
two processes with two roots. There is no in-process product path — an adapter linked into
the server exists only as a unit-test seam, never as a route a session can reach.

A server with no envagent connected serves records and runs nothing. Session creation fails
naming the missing envagent, and the link reports an unavailable capability with a typed
reason; it never falls back to a local adapter.

## Rationale

One path makes reconnect, acknowledgement, offline state, filesystem policy, and vendor-local
history observable under the same contract. A local shortcut would make tests pass while the
remote protocol remained unexercised, and would create a third path for sandboxes.

It also settles where a machine-local fact lives. Path canonicalization, `browse_roots`, and
which agent CLIs exist at which versions are answerable only on the machine holding them, so
a privileged in-process environment would need every one of those answered twice.

## Acceptance

The cutover is complete when all session, turn, watch/adopt, files, health, and control routes
dispatch through `EnvBridge`, and restart/reconnect tests show no loss or duplicate canonical
events.
