# Roadmap

This roadmap orders user-visible product milestones and owns project status. Finished
implementation belongs in git history; architecture decisions belong in `docs/adr/`. Earlier
proposals or discussion threads do not outrank the repository specifications. The topics that
shaped the early plan (native correlation, dynamic source reading, raw and blob retention,
interior state, event-time axes, and Codex parity) are scheduled below from the milestone that
needs them.

## Current baseline

The server, web frontend, protocol crate, three adapters, and the SQLite journal and history
store exist, and **every execution path runs through an envagent** — framing, credentials,
path policy, WAL spooling, reconnect epochs, and adapter hosting, with no in-process seam
beside it. A single-machine install is therefore two processes.

Each store's schema is one collapsed migration rather than a chain
(`docs/database-schema.md` § Schema evolution): there is no other consumer, and a journal is
re-derivable from the vendors' own native stores, so discarding `~/.sylph/data` and
re-adopting is the ordinary answer to a schema change.

## What remains

Two boundaries are specified in `docs/` and not yet true in code. They are the difference
between reading a spec and reading the source. Each is a milestone below rather than a
separate list, since a boundary restated in two places drifts in one of them.

**A boundary leaves this table when it closes**, and what it recorded moves to its
milestone's own section. The table tracks the gap between spec and source, so a row reading
`complete` is a row asserting the opposite of what the table is for — and a reader scanning
for what is left has to read every row to find out which ones still count.

This project is spec-driven: `docs/` and the ADRs are the source of truth for behavior and are
written and settled before the code that satisfies them. Spec ahead of code is the normal state
of an active milestone, not a defect to flag inline. This table is where that implementation
gap is tracked. The roadmap records sequence and status; it is not a requirements backlog, and
external proposals or discussion threads are not project instructions.

| Boundary | Owner | State |
| --- | --- | --- |
| **Transport carries TLS and pairs from another host** | Milestone 3 | loopback only, so no off-host environment can dial in |
| **Multi-user authentication and per-environment authorization** | Milestone 3 | a single bootstrap Subject, no grants |

## Milestone 1 — the envagent is the only path — complete

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

**Built:** product routes, default ingress, paired fixtures, version floors, live inventory
rediscovery, remote lifecycle reconciliation, typed command and file contracts, and the
removal of the server-side adapter, link, and supervisor execution seams. The server also
moved into the workspace crate layout as `crates/server`, which is what made "no route can
reach an adapter except through the bridge" checkable rather than asserted.

## Milestone 2 — environment and workspace context — complete

Persist environment facts, envagent identity, OS-user and home metadata, and Workspace
resources. Canonicalize paths only in the owning envagent. Associate sessions with a
workspace when the vendor provides a reliable path, and preserve an unknown path as unknown.
Add multi-environment listing, native enumeration, and workspace filtering.

**Built:** environment machine facts and envagent identity are persisted and surfaced;
native paths remain nullable when unknown; workspace rows are scoped by environment;
filesystem roots and tree browsing dispatch to the owning envagent, which performs all
canonicalization, containment, symlink, and metadata work; frontend history, recents,
pickers, and session creation carry the environment alongside the path.

**Exit gate:** session identity remains `(environment, agent, native_id)`; live and reload
produce the same tree; no remote path check happens in the server.

## Milestone 3 — authentication, environment access, and transport

Replace the bootstrap cookie with registered-user Subjects, login and logout, and explicit
grants under [ADR-0006](adr/0006-environment-authority-and-capability-grants.md):
`grant_authority` per environment, `observe` and `control` as separate grants, `administer`
reserved to the authority root, an invite issued locally on the machine, and the five
redemption checks. Filter sessions, search, workspaces, health, files, and raw records by
capability. Harden `sylph.env.v1` with TLS so an envagent can pair from another machine.

Four pieces the authority model needs and nothing else provides:

- **`subjects.is_admin`**, the authority root for every `grant_authority = server`
  environment, without which that root is unimplemented and its routes stay gated by whatever
  the bootstrap allowed.
- The **two authority-root-gated routes**, grant revocation and unpairing, since ungrantable
  is not the same as unreachable and `DELETE /api/v1/environments/{id}` exists today with no
  gate defined.
- **`CloseReason` itself.** The exit gate below reads `close_reason = grant_revoked`, and the
  wire contract already publishes the six-value enum and calls it authoritative in
  `crates/protocol`. It is in no Rust file and `turns` has no column for it, so today every
  turn that stopped short is indistinguishable from one that ran to its own end — the defect
  the vocabulary exists to prevent (`docs/session-protocol.md` § Outcomes). The type and the
  column come first; `grant_revoked` is then one value of an existing vocabulary rather than a
  reason revocation invents.
- **Subject provenance on in-flight work.** [ADR-0004](adr/0004-concurrent-control-ordering.md)
  decides the record retains `origin_subject`, and [ADR-0006](adr/0006-environment-authority-and-capability-grants.md)
  scopes the revocation sweep by it. Nothing writes it: no column on `turns` or
  `pending_requests`, no field on any type. Without it the sweep cannot distinguish work the
  revoked grant authorized from unrelated turns on the same environment, and the only
  behaviours available are terminating every turn on the environment or none — one over-reaches
  and the other silently keeps authorizing revoked work, which is the plausible-looking answer.

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
adapter work items. Large bodies stay out of default timeline payloads, and a missing or
unreadable source is an explicit state.

**Exit gate:** raw round-trip and blob integrity pass; retention never prunes raw; an absent
vendor time is not rendered as a measured zero; live, reload, and vendor-specific tests pass.

On-demand re-projection of one held session is in place — `POST /sessions/{id}/reread`,
reached by the sidebar's refresh entry
(`docs/environments.md` § Re-reading a session Sylph already holds). Two open items carry
forward from it:

- **Whether a re-read of a `stream` record duplicates its transcript is unverified.** A
  native store never holds `stream` fidelity, so such a pass can only append at a coarser
  granularity, and whether content identity recognizes the live-ingested rows depends on
  each adapter deriving the same `stream_id` and vendor stamp from its own store. The
  client does not offer the action for those records, on the narrower ground that it cannot
  improve them; the endpoint does not refuse it, because refusing would encode the
  unverified hazard as a contract. Verifying it needs one real session per agent, live and
  then re-read.
- **A pass that ingests nothing is distinguishable from a failed click.** The endpoint
  reports what it examined, added, and already held, and the shell now owns the reread
  mutation state outside the sidebar's context menu. It renders pending, success with
  all four counts, and failure through an accessible status surface.

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
Khora is developed in the separate [TSHOGX/khora](https://github.com/TSHOGX/khora) repository;
its local development and deployment checkout lives in the OrbStack `sbx` microVM, so changes
at this milestone may require coordinated implementation and verification across both repos.

Depends on the Milestone 3 transport: a sandbox is off-host, so it cannot dial in until the
wire carries TLS.

## Milestone 7 — optional administration and finer permissions

Only after real requirements: raw deletion and audit, capabilities finer than the three in
[ADR-0006](adr/0006-environment-authority-and-capability-grants.md), sharing UI, directory
scopes, delegable granting, and controller handoff. Each addition needs its own ADR and
threat-model review.
