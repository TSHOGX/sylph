# API

A versioned REST API with SSE streaming, on `/api/v1/*`. This is a reference organized by
resource, searched rather than read end to end.

| Resource | File |
| --- | --- |
| Health, diagnostics, identity, environments, agents, workspaces | [environments.md](environments.md) |
| Sessions, turns, the event stream, links, requests, lineage, search | [sessions.md](sessions.md) |
| Unified trace objects, history, snapshots, payloads and search | [../unified-trace-storage.md](../unified-trace-storage.md#read-protocol) |
| Files, attachments, machine-level browsing | [files.md](files.md) |
| Error shape, authentication, capability filtering | [admin.md](admin.md) |

`docs/api-v1.openapi.json` is the generated machine-readable snapshot and may lag; these
files are the evolving contract. The unified trace read protocol replaces the former
turn/block/execution-node reads; their old routes are implementation context until the
replacement lands, not a compatibility API promise.

## Conventions

`http://127.0.0.1:4317` by default, loopback only. Every endpoint except `GET /health`
requires the process-local HttpOnly cookie.

Turn creation returns **202** and the client streams events over SSE, which resumes by
`Last-Event-ID` or `after_seq`. Turn creation is idempotent under a client-supplied key.
Errors share one shape, in [admin.md](admin.md).

## A session id in a path is the identity triple

A session is `(environment, agent, native_id)` (`docs/architecture.md` § Identity). On the
wire and in a path segment it is encoded `<environment>:<agent>:<native_id>` and
**percent-encoded** as one segment, so `{id}` throughout these files is that string.

Two rules a client must not break:

- **Do not split on `/`.** Native ids and vendor values contain slashes — 350 of `pi`'s
  372 model ids do — which is why the delimiter is `:`.
- **Treat it as opaque.** A client echoes it back and never parses it to learn which agent
  or environment a session belongs to. Those are separate fields on the session object,
  and reading them out of the id is how an encoding change breaks a client silently.

There is no server-minted UUID. `POST /sessions` therefore starts an agent and returns a
process-local draft handle rather than reserving a row; capabilities and configuration are
read or changed through the draft routes, and the first turn promotes it to a normal
session. A draft handle is never a session id, and appears in no listing or native-history
browse.