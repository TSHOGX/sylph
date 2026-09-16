# API — Sessions and commands

The authenticated API exposes native Session metadata and typed envagent controls.
Normalized trace reads use [the trace read contract](../unified-trace-storage.md#read-protocol)
and the value-only DTOs in [read.rs](../../crates/protocol/src/trace/read.rs).
There are no turn, event, block, execution-tree or compatibility reading endpoints.

## Sessions

### `GET /api/v1/sessions`

Returns `{ "sessions": [...] }`, bounded by `limit` (default 50, maximum 200). Filters
are `agent`, `environment`, exact `workdir`, `updatedAfterMs`, and `updatedBeforeMs`.
Authorization is applied before the limit. Each row includes the encoded native `id`,
`environment`, `agent`, `nativeId`, computed `traceAgentRef`, optional workdir/title,
immutable `injectIdentity`, fidelity, launch flags and timestamps.

### `POST /api/v1/sessions`

Prepares a vendor session in the selected environment and returns 201 with
`draftId`, environment, agent, contained workdir, title and `injectIdentity`.
The request requires `environment`, `agent`, and `workdir`; optional fields are `title`,
`flags` and `injectIdentity` (default true). The owning envagent validates path containment.
A capture inventory and fixed admission epoch are issued before acquisition starts.
A draft is not a native Session and does not appear in Session listings.

### Prepared-link routes

`GET /api/v1/sessions/draft/{id}` returns draft metadata.
`DELETE /api/v1/sessions/draft/{id}` closes an unused draft.
`GET .../capabilities` reads current vendor configuration and supported controls;
`POST .../config` sets a declared configuration value. File routes under the same draft
prefix use the envagent's contained workdir.

`POST /api/v1/sessions/draft/{id}/commands` accepts `{text, attachments?, idempotencyKey?}`
and returns 202 with `{command, session}`. The immutable accepted command exists before
dispatch. `session` becomes available only when retained vendor evidence supplies its
native identity. A failed initial input cannot create a placeholder Session. Retrying
an idempotency key returns its original command; changed bytes are refused.

### `POST /api/v1/sessions/adopt`

Accepts `environment`, `agent`, `nativeId`, optional `title`, `watch`, and `injectIdentity`.
The envagent captures native sources, validates their recorded workdir, and returns
metadata. The server records the real identity and responds 201. `watch` continues capture
as the source grows. Explicit re-adoption after deletion creates a new admission epoch;
it cannot revive erased observations or snapshots.

### `POST /api/v1/sessions/{id}/reread`

Requests another bounded native capture pass under the existing authorized source scope.
Source-position and producer-receipt identity prevent duplicate observations. It does
not reset the Session or synthesize an execution interval.

### `GET /api/v1/sessions/enumerate`

Lists native metadata reported by one environment. `environment` is required;
`agent`, exact `workdir`, `cursor` and `limit` narrow the listing. The response includes
native IDs, available titles/timestamps, recorded workdirs and adoption blockers.
The server never opens another environment's native filesystem.

### `GET /api/v1/sessions/{id}`

Returns `{session}` after checking current environment observation authority.
Malformed encoded identities are 400; missing Sessions are 404.

### `PATCH /api/v1/sessions/{id}`

Updates `title` or the complete `flags` map. Flag values must be offered by the vendor.
Unknown fields are refused, including `injectIdentity`, which is fixed at creation.

### `DELETE /api/v1/sessions/{id}`

Requires control authority. Commits trace deletion/suppression, removes access to affected
snapshots, requests control closure, erases Session metadata and schedules exported input
bytes for bounded removal. Command tombstones prevent replay from redispatching erased input.

## Commands

`POST /api/v1/sessions/{id}/commands` accepts a new immutable message submission.
`GET /api/v1/commands/{id}` returns `{command, session}` for its author, subject to current
environment authority and deletion. A receipt distinguishes accepted, receiver-accepted,
consumed, unknown, rejected, cancelled and conflicting operational outcomes.

`POST /api/v1/trace/commands` accepts a typed control target, action, lease, idempotency
key and optional text/attachments/configuration/request answer. A Session target must
match its AgentRef. An embedded target requires an evidenced independent vendor route;
its anchor Session is never a fallback. Authorization and execution leases are checked
again at dispatch.

### Queued revisions

The `queue` action retains input on the server until its Session can accept another
execution. While no dispatch permit exists, its receipt reports `editable` and
`cancellable`. `PATCH /api/v1/trace/commands/{id}` accepts `{revision, text}` and records a
new immutable submitted revision plus a `Revised` outbox occurrence. The original input
bytes remain retained. `DELETE /api/v1/trace/commands/{id}?revision=N` cancels only the
specified current queued revision. A dispatch transaction selects and records the exact
revision it sends; edits after that boundary are refused.

Native queue actions follow the vendor's separate contract and do not claim server-side
editability after delivery. Queue acceptance does not prove model consumption.

### Attachments

Attachments are `{name, type, data}` with base64 `data`: at most five, 8 MiB per file and
10 MiB total. Accepted input retains exact text and attachment bytes. The envagent stages
attachments on the machine that runs the vendor; server filesystem paths are not sent
as attachment locations.

## Links and requests

`GET /api/v1/sessions/{id}/capabilities` reads current vendor controls.
`POST /api/v1/sessions/{id}/link` attaches control after checking the environment grant;
`DELETE` closes that control link. Attachment and closure are not semantic execution
outcomes. Missing native workdir prevents control without preventing retained trace reads.

`GET /api/v1/sessions/{id}/requests` reports current pending vendor requests. A typed
approval command must select an offered option and satisfy the request's form. Persistent
permission is refused when the request disallows it. A stale request cannot authorize an
unrelated tool action. Request ownership is reserved atomically with command dispatch;
a second resolver cannot answer the same waiting request. A retry with the original
idempotency key returns its existing receipt even after the request stops waiting.
An unconfirmed resolution stays non-actionable across reconnects. Public request IDs
are scoped to the held vendor process and map back to the vendor's exact native ID.

## Lineage and presentation

Spawn ancestry, interactions, history branches and inherited context are separate trace
relations. Session lists do not infer lineage from suffixes or fold child execution into
a parent. Titles are metadata; unnamed trace agents receive a reader label without
writing a guessed durable title. Bounded trace views, direct relations and stable object
anchors replace browser construction of a complete execution tree.
