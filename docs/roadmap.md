# Roadmap

This roadmap orders user-visible product milestones. Finished implementation belongs in git
history; architecture decisions belong in `docs/adr/`. An old issue never outranks this order.

## Current baseline

The server, web frontend, protocol crate, three adapters, SQLite journal/history store, and an
envagent foundation exist. The envagent has framing, credentials, path policy, WAL spooling, and
reconnect primitives. The formal product cutover is incomplete: most API paths still use the
in-process adapter seam, and multi-user authentication and environment authorization do not yet
exist.

## Milestone 1 — unified local envagent path

Run the local machine through `Client → Server → EnvBridge → Envagent → Adapter`. Route session
creation, turns, watch/adopt, files, health, and control through the bridge. Keep in-process
adapters only for fixtures and protocol tests. Verify pairing, offline behavior, reconnect,
ack/replay, server restart, and envagent restart without duplicate or missing canonical events.

**Exit gate:** no silent local fallback; all formal routes use the bridge; browser and fault-
injection tests cover the restart and reconnect contract.

## Milestone 2 — environment and workspace context

Persist environment facts, envagent identity, OS-user/home metadata, and Workspace resources.
Canonicalize paths only in the owning envagent. Associate sessions with a workspace when the
vendor provides a reliable path; preserve unknown paths as unknown. Add multi-environment listing,
native enumeration, and workspace filtering. Pull in only the Issue #1 children needed for this
context: native correlation, dynamic source reading, and Codex watcher parity.

**Exit gate:** session identity remains `(environment, agent, native_id)`; live and reload produce
the same tree; remote path checks never occur in the server.

## Milestone 3 — authentication, environment access, and envagent transport

Replace the bootstrap cookie with registered-user Subjects, login/logout, and explicit
`environment_access` grants under [ADR-0006](adr/0006-environment-authority-and-capability-grants.md):
`grant_authority` per environment, `observe`/`control` as separate grants, `administer` reserved to
the authority root, invite issued locally on the machine, and the five redemption checks. Filter
sessions, search, workspaces, health, files, and raw records by capability. Harden the
`sylph.env.v1` transport with TLS so an envagent can pair from another machine.

Two pieces the authority model needs and nothing else provides. **The `subjects.is_admin` flag**,
which is the authority root for every `grant_authority = server` environment — ADR-0006 assigns a
whole class of power to it, so a milestone that ships grants without it leaves the `server` root
unimplemented and its routes gated by whatever the bootstrap allowed
(`docs/api-design.md` § The server administration capability). And the **two authority-root-gated
routes**, grant revocation and unpairing: ungrantable is not the same as unreachable, and
`DELETE /api/v1/environments/{id}` exists today with no gate defined for it.

**Exit gate:** unauthenticated requests cannot read business data; two independent browser contexts
observe the same authorized environment; unauthorized environments are absent from every read and
control surface; an `observe` grant reaches no control route; revoking a grant refuses new control
and lands in-flight vendor work as `abandoned` on a turn carrying `close_reason = grant_revoked`;
two Subjects with different grants on one environment receive **identical** `link` facets and
differing `controlBlock` values; an `is_admin` Subject can revoke a grant and unpair a `server`
environment and can read no session it holds no `observe` grant on; a `machine` environment refuses
both routes to a browser session with no local confirmation code; an envagent pairs and reconnects
over TLS from a second host.

## Milestone 4 — lossless native ingestion

Implement raw vendor records, sidecar/blob references, interior observability states, event-time
axes, re-projection, and retention classes under ADR-0005. Complete Claude, Codex, and Pi parity
as separate adapter issues. Large bodies remain out of default timeline payloads; missing or
unreadable sources are explicit states.

**Exit gate:** raw round-trip and blob integrity pass; retention never prunes raw; absent vendor
time is not rendered as a measured zero; live/reload and vendor-specific tests pass.

## Milestone 5 — multi-client control convergence

Allow authorized clients to submit control concurrently. The server assigns request ids and
sequence, persists accepted/queued/running/rejected state, enforces idempotency, and broadcasts
one authoritative stream. Define send/stop ordering, unsupported queue behavior, and first-valid-
wins approval resolution. Do not add controller ownership or handoff without a new ADR.

**Exit gate:** retries never duplicate prompts; concurrent clients converge; independent-browser
E2E covers ordering, queue rejection, and simultaneous approval.

## Milestone 6 — Khora sandbox and delegated execution

Provision sandboxes as ordinary Environments with envagents, on `server` grant authority since no
OS-user sits beside one. Add lifecycle, pause/resume, health, cost/retention policy, and completion
notification. Do not introduce a sandbox-specific Session or Workspace model.

Depends on the Milestone 3 transport: a sandbox is off-host, so it cannot dial in until the wire
carries TLS.

## Milestone 7 — optional administration and finer permissions

Only after real requirements: raw deletion and audit, capabilities finer than the three in
[ADR-0006](adr/0006-environment-authority-and-capability-grants.md), sharing UI, directory scopes,
delegable granting, and controller handoff. Each addition requires its own ADR and threat-model
review.

## Issue #1 disposition

Issue #1 is a parent research topic, not the project gate. Split it into native correlation,
dynamic source reading, raw/blob retention, interior state, event-time axes, and Codex parity.
Each child owns one implementation boundary, tests, review, and rollback. Schedule children from
the milestone that needs them rather than executing the old issue plan linearly.
