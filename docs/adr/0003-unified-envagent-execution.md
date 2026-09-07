# ADR 0003: Use the envagent for every product execution path

- Status: Accepted
- Date: 2026-09-07

## Decision

The formal product path is always:

`Client → Server → EnvBridge → Envagent → Adapter → Vendor CLI/native store`.

The machine hosting the server is not a privileged exception and runs an envagent when used as
an execution environment. In-process adapters remain only as fixtures, unit-test seams, and
explicit protocol simulations. If the envagent is unavailable, the server reports an unavailable
capability; it never silently falls back to a local adapter.

## Rationale

One path makes reconnect, acknowledgement, offline state, filesystem policy, and vendor-local
history observable under the same contract. A local shortcut would make tests pass while the
remote protocol remained unexercised and would create a third path for sandboxes.

## Acceptance

The cutover is complete when all session, turn, watch/adopt, files, health, and control routes
dispatch through `EnvBridge`, and restart/reconnect tests show no loss or duplicate canonical
events.
