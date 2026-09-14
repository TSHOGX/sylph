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

The boundaries below are specified in `docs/` and not yet true in code. They are the difference
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
| Bounded timeline bodies | Milestone 4 | Deferred to the next implementation stage; CAS retention is complete |
| Atomic retained re-projection publication | Milestone 4 | Native projectors exist; the replacement endpoint remains unavailable until publication validates atomically |
| Concurrent native follower and explicit reread deduplication | Milestone 4 | Sequence reservations are shared; projected-event deduplication still needs coordination with an already-running per-session follower |

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

## Milestone 3 — authentication, environment access, and transport — complete

Add registered-user Subjects alongside the bootstrap development identity, with login and logout, and explicit
grants under [ADR-0006](adr/0006-environment-authority-and-capability-grants.md):
`grant_authority` per environment, `observe` and `control` as separate grants, `administer`
reserved to the authority root, an invite issued locally on the machine, and the five
redemption checks. Filter sessions, search, workspaces, health, files, and raw records by
capability. Harden `sylph.env.v1` with TLS so an envagent can pair from another machine.

Four pieces the authority model needs and nothing else provides:

- **`subjects.is_admin`**, the authority root for every `grant_authority = server`
  environment. The schema, provisioning, and route gates now exist.
- The **two authority-root-gated routes**, grant revocation and unpairing, since ungrantable
  is not the same as unreachable and `DELETE /api/v1/environments/{id}` exists today with no
  gate defined.
- **`CloseReason` itself.** The exit gate below reads `close_reason = grant_revoked`, and the
  wire contract publishes the six-value enum and calls it authoritative in `crates/protocol`.
  The `turns.close_reason` column and `CloseReason` type now exist; revocation writes
  `grant_revoked` on affected in-flight turns (`docs/session-protocol.md` § Outcomes).
- **Subject provenance on in-flight work.** [ADR-0004](adr/0004-concurrent-control-ordering.md)
  decides the record retains `origin_subject`, and [ADR-0006](adr/0006-environment-authority-and-capability-grants.md)
  scopes the revocation sweep by it. The `turns.origin_subject` column, model field, authenticated
  population, and scoped revocation sweep now exist.

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

**Built:** registered Subject authentication with login/logout, authority-root grants and
revocation, one-shot invites with scope/expiry/consumption/provenance checks, capability
filtering across reads and control, subject provenance and grant-revoked turn closure, and
rustls transport for off-host envagent connections. Draft handles now carry their creator
Subject and re-check ownership plus the current grant on every metadata, capability,
configuration, file, and promotion operation. The bootstrap Subject remains the explicit
development identity used by local fixtures; the first registered Subject is provisioned as
the server authority root, and bootstrap never overwrites a valid registered cookie.

The transport and contract gates are executable: pairing covers HTTPS URL normalization and
IPv6 literals, all registered routes are present in `docs/api-v1.openapi.json`, and mutation
Origin checks apply consistently to auth, pairing, grants, and application writes.

## Milestone 4 — lossless native ingestion

Implement raw vendor records, sidecar and blob references, interior observability states,
event-time axes, re-projection, and retention classes under
[ADR-0005](adr/0005-raw-retention.md). Complete Claude, Codex, and Pi parity as separate
adapter work items. Large bodies stay out of default timeline payloads, and a missing or
unreadable source is an explicit state.

**Exit gate:** raw round-trip and blob integrity pass; retention never prunes raw; an absent
vendor time is not rendered as a measured zero; live, reload, vendor-specific, and
filesystem-free retained re-projection tests pass.
Claude, Codex, and Pi now emit lossless `NativeRecord` values before projection. Retention
confirmation for ordinary blocks remains conservative when no native-record correspondence
exists, so those blocks are retained until a durable correspondence is available.

On-demand native refresh and filesystem-free native projectors are implemented. Native
records preserve first-seen order and full revisions; all three adapters retain user input,
timestamps, delegation context, and live-envelope projections. Re-read and re-project use
bounded disk-backed transfers, so frame size does not impose a session-size limit. History
is cursor-paged with a snapshot sequence, timeline bodies are lazy with integrity-checked
fetches, durations honor fidelity and clock domains, notification streams reconnect, and
refresh reports pending, success, and concrete failure states. Storage corruption now fails
the read instead of becoming an empty successful transcript, and sidecar availability can
recover without colliding with the retained identity.

Finite imports acknowledge only after the complete transcript has been ingested. Bulk
sync subscriptions survive restart, share discovery, and refresh only changed source
revisions. Native reads have separate bounded command capacity from interactive work;
sidecar and source-close delivery wait for a fixed producer watermark. File roots come
from the server's durable record. Transcript and file queries are cancelled on navigation,
and failed capability reads no longer masquerade as an offline environment.

The M4 acceptance suite covers raw ordering and unsigned revisions, sidecar transitions,
large summaries and bodies, native user-input provenance, all three adapter reprojections,
bounded transfer framing, pagination, reconnect recovery, and mobile focus/refresh behavior.

## Milestone 5 — multi-client control convergence

Control intent is separate from transcript delivery. Send and guarded steer receipts name
a turn lease; adapter lifecycle ingestion shares the session command sequencer. Stop holds
the slot until the adapter settles. Native priority and follow-up queues remain available
with explicit delivery promises in the composer and environment settings; an uncorrelated
vendor queue receipt never asserts consumption. Live and historical sections expose durable
turn status, and fixture delegate closes author their terminal interior facts.


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
