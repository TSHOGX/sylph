# Roadmap

This roadmap orders user-visible product milestones and owns project status. Finished
implementation belongs in git history; architecture decisions belong in `docs/adr/`. An old
issue never outranks this order.

## Current baseline

The server, web frontend, protocol crate, three adapters, SQLite journal and history store,
and an envagent foundation exist. The envagent has framing, credentials, path policy, WAL
spooling, and reconnect primitives.

## What remains

Four boundaries are specified in `docs/` and not yet true in code. They are the difference
between reading a spec and reading the source.

| Boundary | Specified | State |
| --- | --- | --- |
| **The envagent is the only execution path** | [ADR-0003](adr/0003-unified-envagent-execution.md), `docs/environments.md` | most routes still reach an in-process adapter seam, and envagent ingress is opt-in rather than always bound |
| **The server is `crates/server`** | `AGENTS.md` § Orientation | still `backend/`; the crate splits out with the cutover above |
| **Transport carries TLS and pairs from another host** | Milestone 3 | loopback only, so no off-host environment can dial in |
| **Multi-user authentication and per-environment authorization** | [ADR-0006](adr/0006-environment-authority-and-capability-grants.md) | a single bootstrap Subject, no grants |

Two schema residues go with the first: `sessions.parent_session`, superseded by the typed
edge ([ADR-0001](adr/0001-foundation-semantics.md) § 2), and the in-process adapter
registration that lets a route bypass the bridge.

## Milestone 1 — the envagent is the only path

Run every machine, the server's own included, through
`Client → Server → EnvBridge → Envagent → Adapter`. Route session creation, turns,
watch/adopt, files, health, and control through the bridge; bind envagent ingress
unconditionally; delete the in-process route rather than leaving it reachable. A server with
no envagent paired serves records, refuses session creation with a message naming the
missing envagent, and reports the link unavailable with a typed reason.

Agent availability becomes per environment and time-varying: discovered in `hello` against
each adapter's version floor, re-reported when a machine's inventory changes, and read from
`GET /api/v1/environments` rather than from a server-wide list
(`docs/environments.md` § Agent availability).

**Exit gate:** no route can reach an adapter except through the bridge; a single-machine
install runs two processes and pairs; creation with no envagent fails with the naming error;
upgrading a vendor CLI on a connected machine changes what that environment offers without a
re-pair or a server restart; browser and fault-injection tests cover restart and reconnect
with no duplicate or missing canonical event.

## Milestone 2 — environment and workspace context

Persist environment facts, envagent identity, OS-user and home metadata, and Workspace
resources. Canonicalize paths only in the owning envagent. Associate sessions with a
workspace when the vendor provides a reliable path, and preserve an unknown path as unknown.
Add multi-environment listing, native enumeration, and workspace filtering.

**Exit gate:** session identity remains `(environment, agent, native_id)`; live and reload
produce the same tree; no remote path check happens in the server.

## Milestone 3 — authentication, environment access, and transport

Replace the bootstrap cookie with registered-user Subjects, login and logout, and explicit
grants under [ADR-0006](adr/0006-environment-authority-and-capability-grants.md):
`grant_authority` per environment, `observe` and `control` as separate grants, `administer`
reserved to the authority root, an invite issued locally on the machine, and the five
redemption checks. Filter sessions, search, workspaces, health, files, and raw records by
capability. Harden `sylph.env.v1` with TLS so an envagent can pair from another machine.

Two pieces the authority model needs and nothing else provides: **`subjects.is_admin`**, the
authority root for every `grant_authority = server` environment, without which that root is
unimplemented and its routes stay gated by whatever the bootstrap allowed; and the **two
authority-root-gated routes**, grant revocation and unpairing, since ungrantable is not the
same as unreachable and `DELETE /api/v1/environments/{id}` exists today with no gate defined.

**Exit gate:** unauthenticated requests cannot read business data; two independent browser
contexts observe the same authorized environment; an unauthorized environment is absent from
every read and control surface; an `observe` grant reaches no control route; revoking a grant
refuses new control and lands in-flight vendor work as `abandoned` on a turn carrying
`close_reason = grant_revoked`; two Subjects with different grants on one environment receive
**identical** `link` facets and differing `controlBlock` values; an `is_admin` Subject can
revoke a grant and unpair a `server` environment while reading no session it holds no
`observe` grant on; a `machine` environment refuses both routes to a browser session with no
local confirmation code; unpairing leaves every session listable, readable, and searchable;
an envagent pairs and reconnects over TLS from a second host.

## Milestone 4 — lossless native ingestion

Implement raw vendor records, sidecar and blob references, interior observability states,
event-time axes, re-projection, and retention classes under
[ADR-0005](adr/0005-raw-retention.md). Complete Claude, Codex, and Pi parity as separate
adapter issues. Large bodies stay out of default timeline payloads, and a missing or
unreadable source is an explicit state.

**Exit gate:** raw round-trip and blob integrity pass; retention never prunes raw; an absent
vendor time is not rendered as a measured zero; live, reload, and vendor-specific tests pass.

## Milestone 5 — multi-client control convergence

Allow authorized clients to submit control concurrently. The server assigns request ids and
sequence, persists accepted / queued / running / rejected state, enforces idempotency, and
broadcasts one authoritative stream. Define send and stop ordering, unsupported-queue
behaviour, and first-valid-wins approval resolution. Controller ownership and handoff need a
new ADR.

**Exit gate:** retries never duplicate prompts; concurrent clients converge;
independent-browser E2E covers ordering, queue rejection, and simultaneous approval.

## Milestone 6 — Khora sandbox and delegated execution

Provision sandboxes as ordinary environments with envagents, on `server` grant authority
since no OS-user sits beside one. Add lifecycle, pause and resume, health, cost and retention
policy, and completion notification. No sandbox-specific Session or Workspace model.

Depends on the Milestone 3 transport: a sandbox is off-host, so it cannot dial in until the
wire carries TLS.

## Milestone 7 — optional administration and finer permissions

Only after real requirements: raw deletion and audit, capabilities finer than the three in
[ADR-0006](adr/0006-environment-authority-and-capability-grants.md), sharing UI, directory
scopes, delegable granting, and controller handoff. Each addition needs its own ADR and
threat-model review.

## Issue #1 disposition

Issue #1 is a parent research topic rather than the project gate. Split it into native
correlation, dynamic source reading, raw and blob retention, interior state, event-time axes,
and Codex parity. Each child owns one implementation boundary, tests, review, and rollback.
Schedule children from the milestone that needs them rather than executing the old issue plan
linearly.