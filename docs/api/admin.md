# API — Errors, authentication, and authorization

The error shape every endpoint returns, how a caller authenticates, and how a
capability grant filters what it may read or drive.

## Error Responses

Every failing response under `/api/v1/` carries this envelope, with no
exceptions. That includes failures raised before any handler runs: a path that
matches no route answers `NOT_FOUND`, and a known path with the wrong method
answers `METHOD_NOT_ALLOWED`.

The invariant matters more than it looks. A bodiless framework 404 is
indistinguishable, at the client, from a handler reporting that a *file* is
missing — so a client showing "Not Found" cannot say whether the file is absent
or the endpoint is. Clients branch on `error.code`, never on the HTTP status
alone, and that is only sound if every error has a code.

```json
{
  "error": {
    "code": "INVALID_REQUEST",
    "message": "Invalid agent name",
    "details": {
      "field": "agent",
      "value": "unknown-agent"
    }
  }
}
```

`details` is optional and omitted when there is nothing structured to add.

The full set of codes is the `ErrorResponse.error.code` enum in
`docs/api-v1.openapi.json`; update the snapshot when the evolving contract settles. The two router-level codes
are worth calling out here because no handler can produce them:

| Code | Status | Meaning |
| --- | --- | --- |
| `NOT_FOUND` | 404 | No route matches this path. The endpoint does not exist in this build — distinct from `SESSION_NOT_FOUND` or `FILE_NOT_FOUND`, which mean the route exists and the named thing does not. |
| `METHOD_NOT_ALLOWED` | 405 | The path exists but not for this method. |

A client that sees `NOT_FOUND` for an endpoint it expects to exist is talking to
an older backend than it was built against, and should say so rather than
blaming the request.

## Authentication

Authentication uses process-local HttpOnly cookies for the first-run bootstrap Subject and registered
Subjects. Registration, login, logout, and environment grants are implemented routes described
by [ADR-0002](../adr/0002-environment-workspace-subject.md) and
[ADR-0006](../adr/0006-environment-authority-and-capability-grants.md). Required for every call except
`GET /api/v1/health`, `GET /api/v1/auth/token`, the registered auth mutations, and
`POST /api/v1/environments/pair`, which are exempt inside the middleware rather than
registered outside it, so the exempt list sits next to the checks it bypasses. Registered
Subject login, logout, and registration are available at the corresponding `/auth` routes;
the bootstrap token remains only until the first registered Subject exists, for local development
and fixtures.

- Cookie: `sylph_session={random_token}`; `Path=/`, `HttpOnly`, `SameSite=Strict`
- Origin must match loopback (`127.0.0.1`, `localhost`, `[::1]`)
- Host header validated against the same allowlist

### Authorization filters by capability

Once grants exist, every route resolves the caller's capability on the environment owning the
resource, and filters on that rather than on environment membership. `observe` covers sessions,
turns, links, search, workspaces, lineage, health, and file reads. `control` covers turn
submission, stop, approval, session creation, and file writes.

Filtering by environment alone would give a read-only Subject every control route on any
environment they can see, so the capability is the boundary and the environment is its scope.

#### `administer` is ungrantable, which is not the same as having no route

`administer` is never the subject of a grant
([ADR-0006](../adr/0006-environment-authority-and-capability-grants.md) § Three capabilities). But
"ungrantable" bounds *who* may invoke an operation, not *whether an operation is reachable over
HTTP* — and conflating the two produced a contradiction in this very document, which asserted that
`administer` "has no route" while defining `DELETE /api/v1/environments/{id}` fourteen hundred lines
earlier. What actually decides the question is **where the state being changed lives**:

| Operation | State lives | Carrier |
| --- | --- | --- |
| edit `browse_roots` | the envagent's own config | local: `config.toml` on that machine, no route |
| agent configuration | the machine's `PATH` and vendor files | local: no route |
| issue an invite | minted on the machine, redeemed at the server | local: `sylph-envagent invite`, by design outside the wire |
| revoke a grant | `env_grants`, a server table | **server route**, authority-root gated |
| unpair an environment | server-held credentials and lease | **server route**, authority-root gated |

The split is not a compromise; it is forced. Only the machine holding a path can canonicalize it
or see whether a component is a symlink, so `browse_roots` cannot be edited from the server at all
(`docs/configuration.md` § `browse_roots` bounds where agents run) — its absence of a route is a
containment property, not an access rule. And a grant row lives only in the server's store
(`docs/architecture.md` § One authority), so a local command *cannot* delete one. Each operation is
reachable exactly where its state is; there was never a choice.

So the two server-side administrative routes are gated by the environment's `grant_authority` root
rather than by any capability a grant conveys:

- `grant_authority = machine` — the caller must prove that machine's OS-user. The envagent holds
  the machine's credential, so the proof is a short-lived confirmation code minted by a local
  command, redeemed on the route in the same shape pairing and invite already use
  (`docs/environments.md` § Pairing). A browser session alone never suffices, which is the whole
  point: an attacker holding a Subject's cookie must still reach the machine.
- `grant_authority = server` — the caller must hold the server administration capability, defined
  below. A Khora sandbox has no OS-user to ask.

**A route with no defined gate is worse than a missing route**, because the gate is what someone
implementing it will supply from context. Recording "no route" while a route existed left
`DELETE /api/v1/environments/{id}` gated by whatever the bootstrap cookie happened to allow, which
today is everything.

#### The server administration capability

`grant_authority = server` requires an authorizing party that is not any machine's OS-user, and
[ADR-0006](../adr/0006-environment-authority-and-capability-grants.md) named it "the server
administration surface" without defining it. It is **a flag on a Subject**, `subjects.is_admin`,
not a separate process, port, or console.

A flag rather than a service because there is nothing for a second surface to hold: the grant table
and the environment rows are in the one store already, so a separate admin process would be a
second writer to them — the "which copy is the truth" question the authority model exists to answer
(`docs/architecture.md` § One authority). And a flag rather than an `administer` row in `env_grants`
because that table is per-environment and this authority is not: it holds over every
`grant_authority = server` environment, including ones provisioned after the flag was set. A row
asserting it would also be a grant of the power to grant, which decision 3 forbids
(`docs/database-schema.md` § Subjects, environments, and grants).

Three bounds, all of which follow from the flag being an authority root rather than a capability:

- **It is not grantable and not delegable.** An admin Subject cannot confer it; it is set where
  Subjects are provisioned.
- **It confers no `observe` or `control` on any environment.** Administering a sandbox's grants is
  not reading its transcripts, and bundling them would make the audit surface the largest reader.
  An admin who wants to read a session grants themselves one, which leaves a row.
- **It does nothing on a `machine` environment.** Authority there is physical, and a server flag
  that could override it would make "the OS-user authorizes" false for anyone who could set the
  flag.

The bootstrap Subject satisfies this for local development. Registered Subjects are provisioned
through `/auth/register` and `/auth/login`; server-authority environments use the `is_admin` root.

#### `GET /api/v1/auth/token`

Bootstraps the cookie for a loopback client while the install has no registered Subject and
answers `{"ok": true}`. After registration it answers `401 authentication_required` and never
sets a cookie, so logout cannot silently restore the bootstrap identity. No credential is
exchanged during first-run bootstrap: the backend hands the cookie to a request that already
passes the loopback `Host`/`Origin` checks.

**`SameSite=Strict` is why the frontend must proxy `/api/v1/*` same-origin.** A
browser will not send the cookie to a different port, so pointing the UI straight at
`:4317` fails every authenticated call while `curl` succeeds.

**The token is regenerated on every backend start**, which invalidates open browser
sessions. That is the design — the cookie is process-local, and a token that
outlived the process would be a persisted credential with no way to revoke it. Do
not "fix" it by writing the token to disk; reload the page instead.

Mutations additionally require a loopback `Origin`, which is what makes a tunnelled
or LAN origin read-only unless it comes through a proxy that rewrites `Origin` to
loopback. See `docs/development.md`.

## OpenAPI Specification

The generated machine-readable snapshot is `docs/api-v1.openapi.json`; this document remains
the evolving source for endpoint semantics.

**Verify coverage by diffing, never by trusting a count.** A route added to
`crates/server/src/lib.rs` without a matching path here is the recurring failure, and the check
is mechanical: normalize path parameters and diff the spec against the `.route(...)` calls,
in both directions. Counts stated in prose have been wrong here in both directions at once
— claiming full coverage while a whole control plane was undocumented, and naming
endpoints as gaps that were not. **A claim that something is documented is itself a thing
to verify**, so grep for the heading too.

Two defects worth knowing about, because a path-level count cannot see either:

- **A response that `$ref`s the *request* schema.** A generated client then types the
  response as the body it just sent. A path being present is not a path being right, so
  check that each response names a *response* shape.
- **A response declaring no content at all**, which a generator reads as an untyped body
  rather than as an omission.

`error::tests::codes_match_the_openapi_enum` is the one part of this contract a failure
currently reports on its own; extending that to path coverage is a standing suggestion in
`docs/roadmap.md`.

The frontend does not consume generated types: `frontend/lib/api.ts` is hand-written
and mirrors the wire shape directly. Generating a second definition of every shape
would mean two sources to keep in step; see `docs/frontend-architecture.md` § Types. The
backend does pin one thing to this file automatically —
`error::tests::codes_match_the_openapi_enum` fails if an error code is emitted that the
schema's enum does not publish.
