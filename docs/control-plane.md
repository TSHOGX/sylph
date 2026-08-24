# Control Plane

One server owns session policy. Frontends are transports.

## The problem this fixes

Session rules are implemented twice. `chat/state.py` in the Sylph skill and
`backend/src/db/store.rs` here both own per-conversation agent choice, flag
state, native session ids, and the 24h-idle / 04:00 lifecycle boundary. Two
implementations of one policy, in two languages, already differing in details
nobody chose deliberately.

The cost is not duplication. It is that **a session's identity depends on which
frontend created it**. A Discord conversation is invisible to the web UI, cannot
be searched with it, and resets on a rule the web app implements slightly
differently. "One authoritative record" is false while two components decide what
a session is.

## Shape

```
      web browser        Discord         terminal / CLI        future
           │                │                  │                 │
           └────────────────┴──────────────────┴─────────────────┘
                                    │
                          HTTP + SSE (/api/v1)
                                    │
                        ┌───────────▼───────────┐
                        │   Sylph server        │  sessions, turns, links,
                        │   (Rust, Axum)        │  policy, journal, requests
                        └───────────┬───────────┘
                                    │
                     ┌──────────────┼──────────────┐
                     │              │              │
                Environments    Adapters       TraceDB
              local / khora /   per agent    (archive, search)
                    ssh        (v3 duplex)
```

The server owns: session identity and origin, identity and lifecycle policy,
turn supervision, link management, the event journal, pending requests,
environment binding, and adapter discovery.

A frontend owns: rendering, input, and its own transport's idioms. Nothing else.
A frontend that stores which agent a conversation uses has reimplemented policy,
and will drift.

## Discord becomes a frontend

`discord_chat_daemon.py` keeps what is irreducibly Discord — Gateway connection,
2000-character chunking, attachment download, slash-command registration, the
channel↔thread model — and gives up what is not.

| Today in the daemon | After |
| --- | --- |
| `chat_state.json` sessions, agents, flags | `GET/PATCH /api/v1/sessions` |
| 24h / 04:00 reset logic | Server policy, one implementation |
| `harness/` runner dispatch | `POST /api/v1/sessions/{id}/turns` |
| `ProcessSupervisor` for `/stop` | `POST /api/v1/turns/{id}/stop` |
| Runner discovery + hot reload | `GET /api/v1/agents` |

This migration is not optional, because v1 is being removed rather than kept
beside v2 (`docs/session-protocol.md`). The daemon's runners stop being
discoverable, so it moves to the server or it stops working. Choosing the
migration over a shim is what leaves one control plane; the shim would have
preserved two.

What the daemon keeps is a binding table: Discord channel → Sylph session id.
That is genuinely Discord's own state, and it is the only state it needs.

Three things get better rather than merely consolidating:

* A Discord conversation appears in the web UI, in search, and in TraceDB,
  because it *is* a Sylph session.
* Discord inherits capabilities it never had. Approval requests become a message
  with buttons; a slash command list that updates when a skill is edited;
  interrupt that keeps the session instead of killing it.
* The daemon stops needing to be running for its sessions to exist.

The reverse direction matters too: a session started in the browser can be
continued from Discord, because neither owns it.

Sylph's two motions are unchanged by this. `ask`/`send` is Claude reaching the
user and does not touch sessions at all. The chat daemon is the ear, and it stays
the ear — it just stops also being a brain.

## Requests need a transport-neutral shape

An approval must be answerable from any frontend, including one that was not
attached when it was raised. So a pending request is a **server resource**, not a
message on a stream:

```
GET  /api/v1/sessions/{id}/requests        → pending requests
POST /api/v1/requests/{id}/resolve         → {option, input?}
```

It is also published on the event stream so an attached client reacts instantly.
The stream is the notification; the resource is the truth. A design where the
stream is the only carrier strands the request the moment a client disconnects —
and a stranded approval blocks an agent indefinitely.

First resolution wins; a second returns `409` naming the resolver, so two people
answering at once is a defined outcome rather than a race.

## Multi-user is now a real question

The security model is loopback-only, single-user, with a process-local cookie.
"Multiple users using multiple frontends" changes that, and the honest position
is that it is **not solved by this work**.

What this design does is avoid foreclosing it:

* Every mutation is a server operation, so an authorization check has exactly one
  place to live rather than one per frontend.
* Request resolution records *who* resolved, so the field exists before there is
  an identity to put in it.
* Environment binding is per session, which is the unit an authorization boundary
  would follow.

What remains genuinely unsolved, and must not be quietly shipped: authentication
beyond the bootstrap cookie, per-user visibility, credential isolation between
users sharing a host, and the CSRF and origin work that non-loopback binding
requires. Binding to a non-loopback interface stays a separate, explicit change.

## API surface

Additive to `/api/v1`. Existing endpoints keep their contracts.

| Endpoint | Purpose |
| --- | --- |
| `GET /sessions/{id}/replay` | The session's stream, reconstructed with original timing |
| `GET /sessions/{id}/capabilities` | Live slash commands, subagents, typed config, permission mode |
| `POST /sessions/{id}/link` | Establish a link eagerly |
| `DELETE /sessions/{id}/link` | Tear one down |
| `GET /sessions/{id}/requests` | Pending requests |
| `POST /requests/{id}/resolve` | Answer one |
| `POST /sessions/{id}/steer` | Inject text mid-turn, naming the turn it is meant for |
| `POST /sessions/{id}/interrupt` | Stop the turn, keep the session |
| `POST /sessions/{id}/config` | Set one typed config option; answers with the refreshed surface |
| `GET /environments` | Configured environments and health |

Both `steer` and `interrupt` are session-scoped rather than turn-scoped: they act on
the agent, and the session is what holds a link, so a caller need not know a turn id
to interrupt the only turn running. `steer` still *names* a turn — as a precondition,
not as routing — because a steer racing a turn boundary would otherwise be delivered
into the next turn, succeeding silently with the text in the wrong conversation.

`config` is what a control command resolves to. `/model` sent as prompt text comes
back as a paragraph, because a headless CLI has no picker to open, so the option is
typed state a client renders and choosing one is a typed set. See
`docs/session-protocol.md` § Session config.

`interrupt` is deliberately separate from the existing `stop`. `stop` kills the
process group; `interrupt` asks the agent to stop the turn and stay alive. They
are different user intentions — "that is not what I meant" versus "stop
everything" — and conflating them means one of the two is unavailable.

## Event stream

Unchanged in framing. `GET /turns/{id}/events` keeps SSE, `Last-Event-ID`
replay, and the stable/ephemeral split. New event kinds are additive.

One addition: a **session-scoped** stream, `GET /sessions/{id}/events`. Links,
capability changes, and requests are session facts, not turn facts, and a client
that only watches turns cannot see an approval raised while it was idle.

The `no-transform` header and hand-rolled frame parsing stay exactly as they are.
Both exist because of bugs already paid for — a proxy gzipping SSE, and
`EventSource` dispatching by frame name — and neither is revisited here.
