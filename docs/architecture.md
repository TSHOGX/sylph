# Architecture

Sylph is a coding-agent workspace. A **server** holds the authoritative record of every
session and owns all session policy. An **envagent** runs on each machine where agents
execute and dials back to the server. A **web frontend** is a transport onto the server,
and so is anything else.

This document owns the system's shape, the session model, identity, and the authority
model. Every other document defers to it.

## Topology

```
    browser            other frontends
       │                      │
       └──────────┬───────────┘
                  │  HTTP + SSE, same origin
        ┌─────────▼──────────────────────────────┐
        │  sylph-server  (+ web, same origin)    │
        │  sessions · turns · links · policy     │
        │  journal.db · history.db · blobs/      │
        │  THE authoritative record              │
        └─────────▲──────────────────────────────┘
                  │  sylph.env.v1 — envagent dials IN
     ┌────────────┼─────────────┬──────────────────┐
     │            │             │                  │
 your laptop   SSH host     khora sandbox    someone else's box
 envagent      envagent     envagent         envagent
 + agent CLIs  + CLIs       + CLIs           + CLIs
     ▲
     └── bootstrapped the SSH host by copying itself over ssh
```

Three deployable products, and the direction of the arrow is the design:

| Product | Where it runs | What it holds |
| --- | --- | --- |
| `sylph-server` | one host, with the web bundle beside it | every session's record, all policy |
| `sylph-envagent` | every machine that runs agents | agent CLIs, vendor credentials, adapters |
| web bundle | same host and origin as the server | rendering only |

The envagent connects outward to the server, never the reverse, which makes a laptop
behind NAT, an SSH host on a private network, and an auto-pausing sandbox one case. A
server that had to reach in would work for exactly one of the three.

## An environment is an envagent, and that is the whole abstraction

There is no `local` / `ssh` / `khora` trichotomy in the code. There is one wire protocol,
`sylph.env.v1`, and environments differ only in **how an envagent came to exist**:

| Appearance | Mechanism |
| --- | --- |
| self-installed | a user installs `sylph-envagent` and pairs it |
| SSH-bootstrapped | an existing envagent copies the binary to a host it can reach and starts it |
| sandbox-provisioned | the server asks Khora for a sandbox whose image contains an envagent |
| development | `cargo run -p sylph-envagent` beside a dev server |

The adapter has to read the vendor CLI's native store, resolve its config, and inherit
its skills and project files — all local facts on the machine the CLI lives on. A
server-side SSH implementation would therefore need a remote filesystem for every vendor
detail it touches. Putting the adapter *in* the envagent removes the question, and once
it is removed the three implementations collapse into one. Cursor arrives at the same
model for the same reason: it copies its own component to a remote host rather than
driving that host over a connection.

**The envagent path is the only path**, including on the machine the server runs on. A
single-machine install is two processes, and a server with no envagent connected can run
nothing and reports that concretely. Treating the server's own machine as a privileged
environment with its own code path would make one environment permanently unlike every
other, so every capability would need answering twice. `docs/environments.md` owns the
protocol, the spool, pairing, agent availability, and provisioning.

A browser cannot fork a process, so no frontend hosts an environment. What a web user
gets instead is every envagent currently connected, including the one on the laptop they
are sitting at — the same capability by a different route, and the UI must not imply the
other thing exists.

## One class of session

There is one kind of session. Sylph does not divide sessions by who started them, and the
row records no class.

**Who holds stdio is a property of a link, not of a session, and it flips both ways.**
After its process exits, a session Sylph started is byte-identical on disk to one started
in a terminal; after Sylph resumes a terminal session, Sylph holds its stdio. A class
written at creation stops being true the first time either happens.

**Identity injection has no relationship to who started the session.** Adopting a
terminal session and injecting `SOUL.md` from that point on is coherent, and so is
starting a session from Sylph with no injection.

**Sylph has no lifecycle policy of its own.** Nothing idles a session out and nothing
resets one: a reset would replace the agent-native id mid-session, breaking the identity
rule below. The **link** idle timeout is a different and real thing (30 minutes) and
stays.

So the only distinction on the row is `sessions.inject_identity`: a boolean, default true
for a session Sylph starts and false for one it adopts. Settable at creation and refused
afterwards, because a transcript records what produced it and rewriting the flag would
rewrite what the stored history means.

### What genuinely differs, and where it belongs

**How events are obtained** is an adapter mode (`docs/session-protocol.md`): hold the
process's stdio, or read the vendor's native store. One vocabulary comes out either way.

**How much resolution the record has** is `fidelity` on the record itself — `stream`,
`blocks`, or `messages`. It is not correlated with who started the session: a session
Sylph started, read back from the vendor's native store after the process died, is
message fidelity too. A renderer reads the field to decide whether it may draw a
duration, since a waterfall over message-fidelity data would present equal-length bars as
measurements.

## Identity is `(environment, agent, native_id)`

A session's id is the triple, and there is no Sylph-minted UUID.

The native id is the durable identity, but it is not necessarily available when a process
is first opened: Claude assigns its id when the first turn starts, while Pi and Codex may
expose an in-memory id before their stores hold a resumable transcript. A second Sylph id
would be redundancy plus a standing invitation to compare ids from two spaces — a
comparison that yields no error at any layer and is simply always false. **One id space
makes the unanswerable comparison unwritable.**

**What is forbidden is a second id space, not generating a value.** Where a vendor takes
`--session-id`, the envagent generates one and hands it over at launch, and the vendor
adopts it as its own native id — so the id Sylph keys on and the id in the vendor's store
are one value in one space, which is the whole point of dictating rather than correlating
afterwards. The rejected shape is a Sylph id held *alongside* a vendor id, which is what
creates two spaces and the always-false comparison. Which vendors accept this is read from
`AgentInfo::dictates_native_id` (`docs/session-protocol.md` § Opening a session), and an
**adapter** never mints: an id an adapter invented would look like the vendor's while
being Sylph's, without ever reaching the vendor.

Rules the triple needs to hold:

- **`environment` is an opaque id minted at pairing**, never a user-visible name. A name
  in a key means renaming destroys identity. Display names are mutable metadata.
- **The encoding does not use `/`.** Most of `pi`'s model ids contain a slash, and vendor
  ids generally may. The wire form is `<env>:<agent>:<native>` with `env` restricted to
  base32, and a URL path segment carries it percent-encoded.
- **A durable session exists only after a real turn and a native id.** `POST /sessions`
  starts a prepared link rather than reserving a row. The handle is process-local and
  disposable: it may be used to read capabilities, change configuration, and browse the
  contained workdir, but it is never a session id and never appears in history. A first
  user message — including a prompt command or an attachment — promotes it atomically;
  abandoning it closes the link without leaving a record, and a failed spawn reports at
  the start screen rather than leaving a dead row behind.
- **The id is stable for the session's whole life**, which follows from nothing resetting
  a session.

Three alternatives, each rejected for a reason that would have surfaced late:

- **A content fingerprint.** Nothing constrains an agent to keep a per-session file at
  all: an agent storing its sessions in one SQLite database with a live WAL cannot
  compute one, and one has been measured doing exactly that. A key one agent cannot
  produce is not a key.
- **A parsed-history id.** A session is observable the moment its native store exists,
  before anything has read it, so keying on a projection makes identity depend on that
  projection's progress.
- **Anything containing a host or a path.** Provenance, not identity.

`environment` is in the key because it names a birthplace, which does not change.
`host_id`, hostnames, and paths are provenance: shown to a user, used for diagnosis,
never an input to a comparison. Content-addressed objects stay **host-blind**, keyed by
hash alone, so the same dependency source uploaded from three machines is stored once.

## Sessions do not migrate between environments

A session belongs to the environment it was born in, for life. Sylph offers no move, no
resume-elsewhere, and no reconciliation of two copies. Nearly all of this document's
simplicity is bought with it:

- No freshness comparison between environments, because two environments never hold
  candidate copies of one session.
- `cwd` is unambiguous. Two machines both having `/home/user/app` is not a collision,
  because a path is only ever read alongside the environment that named it.
- Upload cannot conflict. It either brings a session the server has not seen, or a longer
  prefix of one it has.

Moving work elsewhere is a **fork**: a new session with a recorded ancestor.

## One authority, one store, one vocabulary

The server's own store under `~/.sylph` is the authoritative record. Everything else is a
source it ingests from or a projection it serves.

There is exactly one event vocabulary and one durable store. Native history is parsed **by
an adapter** and emitted as the same events a live turn emits, landing in the same journal
→ history pipeline, carrying a `fidelity` marker, and indexed by the same full-text table.
A reader cannot tell which mode produced a row, and nothing translates between two id
spaces because there is only one.

A second store with its own per-vendor normalizers, schema, and query path bounds what any
capability built on top can honestly claim: full-text search over a *subset* of history
covers whichever sessions some parser happened to reach, so a session whose environment
has gone away is searchable nowhere. One store answers it by construction.

The cost is that vendor-format knowledge lives in Sylph's own adapters, which is where it
belongs anyway — the adapter already owns that vendor's launch line and control surface,
so its native format is the same body of knowledge rather than a second one.

`docs/storage.md` owns the tiers and the fold format. `docs/agents.md` owns each vendor's
native format and quirks.

### Data travels one way: in

The server never pushes history outward and never writes session data outside `~/.sylph`.
There is no bidirectional sync, no last-writer-wins, no vector clock, and no merge — each
of those exists to answer "which of two conflicting writes wins", which the no-migration
invariant means nobody asks.

Two inbound paths:

- **A live session** arrives as a consequence of running. The envagent streams normalized
  events to the server as they happen, so there is no moment at which the data is only in
  one place (`docs/environments.md` § Spooling).
- **An existing native session** is brought in by an explicit act: **watch** it (follow it
  as it runs) or **adopt** it (bring in what is already recorded).

Bringing in native sessions stays explicit rather than becoming an automatic sweep of
every store on the disk. Offering it as a one-time choice when an envagent first pairs is
the shape to build.

What travels is what the adapter produced from the native bytes it read locally, which is
why the adapter runs in the environment. Uploading rows the server had to trust would put
the schema in every environment and require upgrading them in lockstep.

## One link, two typed capability facets

A link has one state and two facets, each able to report unavailable with a typed reason:
**observe** and **control**.

**Two facets rather than one tri-state**, because the vendors do not sort into three
buckets: a terminal `pi` is observable and undrivable, a Sylph-started `codex` is both,
and `claude` answers differently depending on whether a control transport exists at all
(`docs/agents.md` § Capability matrix). A single enum would have to pick one and misreport
the rest. `codex` also splits the two natively — a second client may drive a turn without
subscribing to its events — so the separation exists in a real vendor rather than only in
Sylph's model of one.

Two rules follow:

- **An empty capability surface means "Sylph cannot reach it", never "there is nothing
  there".** Those coincide for a session Sylph launched and are opposite for one running
  under someone else, where the process is alive and healthy.
- **A failure code must be able to describe someone else's process.** Codes that only
  describe our own spawn failing cannot express "running fine, not ours to drive", the
  ordinary state of a terminal-born session.

This is also what makes "another machine is read-only" fall out rather than being a rule
anyone writes: **control is available exactly while that environment's envagent is
connected.** Disconnected, observe still answers from the server's store.

### A facet describes the environment, never the caller

Both facets answer **"can Sylph drive this"** — a property of the environment and the
vendor. Every `UnavailableReason` is of that kind: no CLI on that machine, no envagent
connected, no channel this vendor offers a second party, a handshake that would not come
up. **So one session has one link state, and every observer sees the same one.**

A caller's own authorization is a different fact and gets a different field. Moving the
`control` facet to unavailable on revocation would make `control.available` answer two
questions at once — *can Sylph drive this* and *may **you** drive it*. The first is
shared, the second is per-Subject, and the same session would then hold two different link
states depending on who asked.

The damage would land in the cache. `no_agent` and `not_ours_to_drive` are cacheable
because no retry changes them, and a withdrawn grant cached under that rule is a denial
outliving the grant it describes, while a newly issued grant would not take effect until
the tab reloaded.

So the wire carries three things and a client combines none of them itself:

| Field | Answers | Same for every observer | Cacheable |
| --- | --- | --- | --- |
| `link.observe`, `link.control` | can Sylph reach it | yes | per `UnavailableReason::is_retryable` |
| `authorization` | may this caller drive it | **no** | never |
| `controlBlock` | the one answer a composer acts on | no, derived | never |

`controlBlock` is derived and transmitted rather than computed per client, because one
fact with two derivations diverges the moment either grows a case. It reports
authorization before reachability, because telling an unauthorized caller that someone
else's laptop is asleep answers a question they had no standing to ask.

Revocation is therefore visible in two places, each permanent in a different sense. The
link says only that this caller holds no grant, since revocation deletes the grant row
and a deleted row cannot be distinguished from one never issued. What revocation *did*
stays on the turn it cut short, as `close_reason = grant_revoked`
(`docs/session-protocol.md` § Outcomes) — a record of the event rather than a per-caller
projection of current state.

## Observation is multi-party; control is server-ordered

Observation is concurrent: multiple clients may subscribe to one session and converge on
the server record. Control requests are accepted concurrently too, but the server assigns
request ids and a deterministic session sequence before dispatching them, so send, stop,
cancel, and approval ordering is the server's ordering. Idempotency keeps a retry from
executing twice.

There is no controller owner, takeover, or handoff. The request origin subject is retained
for audit and UI provenance, not ownership; queue support and unsupported control are
typed outcomes. See [ADR-0004](adr/0004-concurrent-control-ordering.md).

## AFK degrades in a defined order

The motivating scenario is why this architecture exists: work with an agent in a terminal,
walk away, see it from a phone, continue it. It is three capabilities, and they degrade
separately.

**Observe always, resume always, live continuation only where a control transport exists.**
All three supported agents write their native store mid-turn, so an observer sees work in
progress rather than only outcomes, and all three can be resumed after their process
exits. What varies is second-party *control*, and the discriminator is structural: a
session Sylph started holds its stdio and is drivable, while a session started in a
terminal offers a second party a channel only where the vendor provides one.
`docs/agents.md` § Capability matrix is authoritative per vendor and carries the versions
each row was measured against.

So a client renders "observable, not drivable" as a first-class state rather than as a
failure. That is what the two facets exist to express.

## Turn execution

A **turn** is a durable resource, not an HTTP request's lifetime:

1. `POST /sessions/{id}/turns` with an idempotency key → `202` and a turn id
2. The server ensures a link, then sends the turn on it
3. The client streams the session trace (SSE), reconnecting with `Last-Event-ID`
4. The turn settles when the adapter reports an outcome — there is no process exit to
   read, because the adapter is long-lived and a turn is a message on it
5. A server restart marks in-flight turns `interrupted`

One turn per session at a time; a second is refused (`SESSION_ALREADY_RUNNING`) rather
than queued, because the link holds one conversation.

**An agent may start a turn.** *Measured on claude 2.x:* a session with backgrounded work
emits the delegate's completion after its turn's `result`, then an entire exchange of its
own to report it. So the server opens an agent-initiated turn (`origin: "agent"`) on the
first out-of-turn event of any kind. Such a turn has no waiter and its request text is
empty, so a client that ignores `origin` will render an answer to a message the user never
wrote. This is a property of the turn; a session carries no such field.

Two orderings are load-bearing, and both express one principle: **write the row that makes
the next action legal first**, because a client will observe between two writes that settle
one logical fact.

- **`stop` settles the turn before touching the link.** An agent that stops producing
  looks like a lost link to the drain task, so interrupting first reports `LINK_LOST` for
  something the user deliberately asked for.
- **A session returns to `idle` before its turn is marked terminal.** The other order
  leaves a finished turn on a `running` session, which is the state that makes the composer
  refuse the next message.

### Session trace is the transport boundary for delegated work

The session page consumes `GET /sessions/{id}/trace/events`: one monotonic session cursor,
stable replay from both storage tiers, and a bounded in-memory tail for events that have
not consolidated yet. This is the only stream that can remain attached when a delegated
trajectory outlives the turn that opened it.

The trace envelope carries both `trajectoryId` (in the `streamId`/`span` vocabulary) and
`arrivalTurnId`. The first declaration establishes trajectory ownership; `arrivalTurnId` is
arrival provenance only. Keeping those separate is what keeps a late child close from being
routed to a synthetic `origin: agent` turn, and it lets the frontend project the event into
the existing owner card without moving the event's historical sequence.

The index is derived from the event envelope, which stays append-only. Materializing it as
`trajectories` and `invocations` tables is a schema change rather than a second authority.

## Security model, and what multi-user requires

Today: loopback bind, a process-local `HttpOnly` `SameSite=Strict` cookie whose token
rotates on restart, Host header validation, Origin validation on mutations. That is
adequate for a single-user local server and **is not an authentication system** — it knows
a request came from a browser that completed a local handshake, not who is asking.

Multi-user is a planned milestone, with its boundary defined by
[ADR-0002](adr/0002-environment-workspace-subject.md): authenticated Subjects receive
explicit Environment grants. [ADR-0006](adr/0006-environment-authority-and-capability-grants.md)
settles who may issue such a grant, and splits it into `observe` and `control` because a
control grant reaches arbitrary code execution within `browse_roots` while an observe grant
reads a transcript. Until that lands, the bootstrap cookie remains a single-user
development mechanism and non-loopback binding stays refused rather than warned about. The
remaining gate covers identity, transport security, and CSRF.

### Filesystem exposure

A client string becomes a filesystem operation on three surfaces, and the *check* is
shared (`paths::contain_in_any`: canonicalize, then compare component-wise) because a
containment rule written twice will diverge.

| Surface | Root | Scope |
| --- | --- | --- |
| `GET /sessions/{id}/files*` | the session's recorded `workdir` | list, read, parse, raw bytes |
| `POST /sessions/{id}/files` | same | create; no overwrite by default |
| `GET /fs/{roots,tree}` | `browse_roots` (`$HOME` + configured) | directory names only |

Two invariants, each paid for in a real hole:

**Derive the path you operate on from the check itself.** Canonicalize once, into one
variable, and use that variable. A `contain_in_any` result paired with a path it did not
produce is not a bound — that shape let a read canonicalize for the check and then open the
original path, following a symlink out of the workdir.

**A write cannot canonicalize its own target**, because the file does not exist yet, so
containment sits on the parent and the last component is appended verbatim. A pre-existing
symlink there is inside the root by name and points anywhere. Inspect with
`symlink_metadata` first, then resolve and re-check.

Both checks live in the **envagent**, evaluated against that machine's own configuration
(`docs/configuration.md` § `browse_roots` bounds where agents run). The server never
resolves a remote path.

## What is deliberately not built

- Session migration between environments. Forking is the answer.
- Multi-user authentication and per-user visibility.
- Horizontal scale-out. The authority is SQLite and a link is stateful in one process, so
  growth is **shard by session and route**, not stateless replicas. Anything else means
  moving to Postgres. Worth knowing now because it decides which of the two happens later;
  nothing in the API forecloses either.