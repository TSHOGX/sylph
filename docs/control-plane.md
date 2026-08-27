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
                Environments    Adapters        Archive
              local / khora /   per agent    (in-process parse,
                    ssh        (v3 duplex)     read, search)
```

The archive is drawn in-process deliberately. It is a private store under `~/.sylph`
that this server owns, not a daemon a user installs and this server talks to — see
`docs/data-architecture.md` § The archive is private, and Sylph owns its location.
A frontend never reaches it directly, so "which copy is the truth" has one answer per
session class and no negotiation.

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
| 24h / 04:00 reset logic | Server policy, one implementation — *not yet implemented server-side; `docs/architecture.md` § Session Model* |
| `harness/` runner dispatch | `POST /api/v1/sessions/{id}/turns` |
| `ProcessSupervisor` for `/stop` | `POST /api/v1/turns/{id}/stop` |
| Runner discovery + hot reload | `GET /api/v1/agents`, refreshed by `POST /api/v1/agents` |

This migration is not optional, because v1 is being removed rather than kept
beside v3 (`docs/session-protocol.md`). The daemon's runners stop being
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

**Decision: multi-user gets a place in the model, not an implementation.** The daemon
form — a Sylph server a user installs, reached by several clients — makes multi-*client*
real immediately and multi-*user* only eventually, and those are different problems. One
person with a laptop, a phone, and a web tab needs no identity model at all; it needs the
authority to be the server, which is what this document already says. So the three
affordances above stay, `resolvedBy` keeps its column, and nothing else is built yet.

The reason not to build more is specific rather than a matter of taste. A user concept
whose only consumer is a single-user install cannot be exercised, so every check would be
written against an imagined threat model and would be wrong in a way no test could
observe — the shape recorded in `AGENTS.md` as a compatibility branch outliving the
defect it papers over. The signal to build it is a real second principal: a shared host,
or a server holding sessions their owner should not be able to see all of. Until then the
honest statement is that this is single-user software with a multi-client surface, and it
must say so rather than implying otherwise by carrying half a permission system.

## Adopted sessions are the reason this matters most

The sharpest case for one server owning policy is not two of Sylph's own frontends
disagreeing. It is a session **Sylph did not start**: an agent run in a terminal, which
the user then walks away from and wants to keep watching from a phone. Nothing about that
session is Sylph's — not the process, not the identity, not the transcript's format — so
whatever reaches the phone has to be assembled by the one component that can read the
vendor's store and hold a channel to the vendor's process. A frontend cannot do it, and a
second frontend doing it differently is the drift this document exists to prevent.

That splits the capability in two, and the split is per-vendor rather than per-product:
**observation is near-universal, control is not.** Every frontend must therefore render
"observable but not drivable" as a first-class state rather than as a failure — the
degradation order is observe, then resume, then live continuation where the vendor admits
a second party. `docs/data-architecture.md` § AFK is two capabilities, not one carries
the measured per-vendor table.

## API surface

Additive to `/api/v1`, and all of it is now in `docs/api-v1.openapi.json` — 33 paths, 41
method-path pairs, matching the router exactly. Existing endpoints keeping their
contracts is a statement about this phase, not a policy: this project writes no
compatibility code, and a better shape is a reason to change a contract in place.

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

`interrupt` is deliberately separate from the existing `stop`, and the difference is
**not** the one this document used to claim. It said `stop` kills the process group
while `interrupt` asks the agent to stay alive; read against `supervisor/turn.rs`,
`stop` asks the adapter to interrupt *first* and only tears the link down when that
fails or is refused. So the two overlap in mechanism and differ in guarantee:

* `stop` is **unconditional**. It settles the turn as `Interrupted` before touching the
  link at all, so the record does not wait on the agent to answer and a user asking to
  stop is never told the agent declined. The teardown fallback costs the session, which
  is why it is a fallback.
* `interrupt` is **cooperative**. It asks and reports what happened, keeping the session
  and its link.

The ordering inside `stop` is load-bearing rather than tidy: an agent that stops
producing is one its drain task observes closing, which reports `LINK_LOST` — true about
the transport and false about what happened, since the user asked for this. Settling
first means the drain task finds the turn already terminal and does nothing.
Interrupting first was a race, not a fix: `stopping_a_running_turn_cancels_it` failed
roughly two runs in five on it. Note also what teardown is: `close`, 500 ms, then
`SIGTERM` to the process group — there is **no `SIGKILL` escalation**, and the older
"SIGTERM, escalating after 2 s" description belonged to a v1 shape where one process per
turn made killing it equal stopping the turn.

## Event stream

Unchanged in framing. `GET /turns/{id}/events` keeps SSE, `Last-Event-ID`
replay, and the stable/ephemeral split. New event kinds are additive.

One addition: a **session-scoped** stream, `GET /sessions/{id}/events`. Links,
capability changes, and requests are session facts, not turn facts, and a client
that only watches turns cannot see an approval raised while it was idle.

The `no-transform` header and hand-rolled frame parsing stay exactly as they are.
Both exist because of bugs already paid for — a proxy gzipping SSE, and
`EventSource` dispatching by frame name — and neither is revisited here.
