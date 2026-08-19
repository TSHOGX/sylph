# Sylph Web architecture

## Product boundary

Sylph Web is a local control plane and renderer over existing coding-agent runtimes. It does not replace agent-native stores and does not make TraceDB responsible for orchestration. The first release binds to loopback and trusts the local OS account.

## Session classes

Session origin is a durable policy decision, not a visual label.

| Origin | Created by | Identity | Lifecycle | Durable history |
|---|---|---|---|---|
| `sylph` | WebApp or Sylph transport | Inject `SOUL.md` through the runner | Sylph expiry/reset policy | Native agent store, then TraceDB |
| `native` | Agent TUI/CLI, later opened in WebApp | Preserve the native agent context | Preserve native session; no implicit reset/compact | Native agent store, then TraceDB |

Importing a native trace creates a lightweight continuation record in the live journal. It does not copy the archived trajectory and it sets `identity_mode=preserve` and `lifecycle_mode=preserve`. The runner receives `SYLPH_SOUL=""`, which is the existing runner-protocol opt-out.

## Storage ownership

```text
agent native store ──ingest──▶ TraceDB ──list/show/search──▶ stable rendering
       ▲                                                        │
       │ resume                                                 │ reference
       │                                                        ▼
Sylph runner ◀──────── Web service ◀──────── browser     live.sqlite3
                                                   active turns + policy only
```

`live.sqlite3` uses WAL and stores WebApp session policy plus user/assistant/error events needed immediately. Completed `thinking`, `tool_call`, `tool_result`, and `usage` runner events are also journaled; `started` and `delta` events exist only in the browser projection. The final runner envelope remains authoritative for assistant text. The journal is not a second long-term transcript archive. Once a native session is ingested, a later reconciliation job can map the live continuation to its TraceDB session and prune duplicate live event text after a retention window.

TraceDB is accessed through its JSON protocol. Sylph Web never reads TraceDB tables, so schema and tokenizer changes remain owned by TraceDB.

## Runtime flow

1. The server discovers `runners/*/runner.sh` through `--describe`.
2. A WebApp-created session records Sylph identity and lifecycle modes.
3. A native session is listed and rendered from TraceDB; continuing it creates a `native` bridge record.
4. A durable turn is inserted into the live journal and the HTTP API returns `202 Accepted`; the `TurnSupervisor` owns the child process independently of the request.
5. The service opts into `SYLPH_STREAM_FORMAT=ndjson-v1`. Runners project vendor JSONL into normalized reasoning, assistant, tool, usage, and status events while preserving the final-envelope contract.
6. `GET /api/v1/turns/:id/events` is a reconnectable SSE stream. It replays journaled stable events, then the bounded live projection, then newly published events. `Last-Event-ID` and `after_seq` are supported.
7. The agent writes its normal native session. TraceDB ingestion makes that history stable and cross-agent searchable.

The formal contract is described in [api-v1.openapi.json](api-v1.openapi.json). TypeScript type definitions are generated from the Rust backend's serialization schemas.

### Durable execution model

`sessions` represent product conversations; `backend_incarnations` represent opaque agent-native contexts; `turns` represent individual execution lifecycles; and `events` are the bounded live journal. Numbered migrations are applied transactionally and the service marks residual active turns as `interrupted` on restart. A per-session idempotency key prevents duplicate turn creation. Stop targets the detached process group with SIGTERM and escalates to SIGKILL after two seconds. Subscriber queues and live projections are bounded so a slow browser cannot apply backpressure to runner stdout.

Before each managed turn, the service applies the same lazy lifecycle boundary as the Discord daemon: discard the opaque backend session after 24 hours idle or when local time crosses 04:00. The decision is never applied to a native session.

Agent commands are generated from each runner's `--describe` schema and validated server-side before invocation. They share process supervision and opaque session-id updates with turns, but their final text is an ephemeral status response: command prompts and results are not appended as conversation events, and commands do not trigger an implicit lifecycle reset.

Browser attachments are base64-transported over the loopback API, decoded under `dataDir/attachments/<session>/<turn>/`, and forwarded to the runner as absolute `--attachment` paths. Basenames are sanitized and files are created with private modes. A turn accepts at most five files, 8 MiB per file and 10 MiB total; attachment metadata, not binary content or staging paths, is recorded in the live journal.

## Security

- The default listener is `127.0.0.1`.
- Loading the root page issues a process-local, HttpOnly, SameSite=Strict cookie. API calls require it.
- Requests with a Host outside the explicit loopback names are rejected to prevent DNS rebinding.
- Mutations reject a mismatched `Origin`.
- Workdirs must exist, and runner names must come from discovery.
- Exposing the listener beyond loopback is unsupported until authenticated remote access, TLS termination, origin allowlists, attachment limits, and explicit permission policy are designed.

## Planned seams

- `RunnerRegistry`: runner discovery and capability metadata.
- `runTurn`: runner protocol v1 consumer with optional normalized NDJSON events.
- `TraceDbClient`: durable session query interface.
- `LiveStore`: transient turn journal and session policy.
- Reconciler: maps live backend session IDs to TraceDB IDs, tracks ingestion watermarks, and prunes duplicated text.
- Event normalization extensions: diffs, plans, approval requests, and richer status metadata.
