# Architecture

Sylph is a coding-agent workspace. A **server** holds the authoritative record of every
session and owns all session policy. An **envagent** runs on each machine where agents
execute and dials back to the server. A **web frontend** is a transport onto the server,
and so is anything else.

This document owns the system's shape, public Session identity, environment topology and
authority. [unified-trace-foundation.md](unified-trace-foundation.md) owns trace semantics;
[unified-trace-storage.md](unified-trace-storage.md) owns their physical realization.
Those target contracts replace the former turn/block trace model. Implementation gaps
are tracked in [roadmap.md](roadmap.md#unified-trace-foundation-review).

## Topology

```
    browser            other frontends
       │                      │
       └──────────┬───────────┘
                  │  HTTP + SSE, same origin
        ┌─────────▼──────────────────────────────┐
        │  sylph-server  (+ web, same origin)    │
        │  sessions · turns · links · policy     │
        │  control · evidence · projections      │
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

Acquisition is an adapter surface: live capture, native capture, or both. Capture and
vendor process control execute in the envagent. Pure vendor semantic mappers execute
over the server's retained observations, including while an environment is offline.

Resolution belongs to the record. The coarse `fidelity` summary does not conceal mixed
coverage or authorize a duration: source intervals, endpoints, clock domain and
uncertainty govern each measurement. The full contract is
[unified-trace-foundation.md](unified-trace-foundation.md#coverage-is-not-a-rank).

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
- **Interactive creation commits a Session only after actual work and a native id.** `POST /sessions`
  starts a prepared link rather than reserving a row. The handle is process-local and
  disposable: it may be used to read capabilities, change configuration, and browse the
  contained workdir, but it is never a session id and never appears in history. A first
  user message — including a prompt command or an attachment — promotes it atomically;
  abandoning it closes the link without leaving a record, and a failed spawn reports at
  the start screen rather than leaving a dead row behind.
- An imported native thread can be identified before its interior arrives. An
  evidenced embedded child has an AgentRef without a fabricated standalone Session.
  [unified-trace-foundation.md](unified-trace-foundation.md#identity-and-reference-resolution)
  defines that reference and its independently reported control route.
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
- Environment ownership prevents cross-environment session-copy reconciliation. Within
  an environment, rewrites, reverted histories and source conflicts remain possible and
  follow [the capture contract](unified-trace-storage.md#source-identity-and-generations).

Moving work elsewhere is a **fork**: a new session with a recorded ancestor.

## One authority, one store, one vocabulary

The server owns the archive. Retained vendor observations and accepted server facts
are authoritative evidence; semantic claims, search and readers are rebuildable
projections. The envagent spool transfers evidence until acknowledged and is not an
independent archive authority.

One typed vocabulary describes AgentThread, Activation, Message, Delivery, Operation,
Resource, history/context and measurements. Root and child agents share that model.
A graph read never changes original ownership or copies execution into a parent.

[unified-trace-foundation.md](unified-trace-foundation.md) defines the vocabulary.
[unified-trace-storage.md](unified-trace-storage.md#authority-and-database-boundaries)
separates short operational transactions, durable evidence admission and derived query
work. Database boundaries isolate writes; they do not create competing sources of truth.

### Data travels one way: in

Vendor stores are sources to capture, not a backup that can authorize evidence deletion.
The server never reads remote native paths; the envagent captures their bytes and
provenance. Once retained, re-projection uses pure mappers without a reachable vendor.
Missing source material is explicit coverage, not a fabricated empty transcript.

Control requests travel outward through the envagent as before. Their immutable accepted
revisions and delivery outcomes enter the same evidence record through the operational
outbox. A queued request is not proof of execution or model inclusion.

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

A submitted request is a durable control operation with an immutable Message revision
and Delivery attempt. It is not itself an Activation. The server serializes control
against native execution leases; accepted cancellation holds the slot until its actual
vendor/supervisor outcome is known. An at-most-once ambiguous dispatch is never retried
merely because the client or server restarted.

Activations follow observed native boundaries, including harness/agent-originated work.
An arbitrary event arriving after another activation ended cannot open a guessed one.
Server restart changes observation/control state; it does not invent remote execution
outcomes or close independent child resources.

The delivery/lifecycle contract is
[unified-trace-foundation.md](unified-trace-foundation.md#messages-deliveries-and-model-visibility).
Operational durability follows [the outbox protocol](unified-trace-storage.md#operational-facts-use-an-outbox).

### Session trace is the transport boundary for delegated work

A capture connection may multiplex work from several agents. Ownership is explicit and
independent of the parent connection or activation receiving the bytes. Every agent uses
the same reader contract. An embedded child is addressable through its real scoped native
identity; it is not a promoted tool host or a new Session class.

The semantic boundary is [unified-trace-foundation.md](unified-trace-foundation.md#ownership-topology-interaction-and-context).
A separate typed action route is required before controlling an embedded agent; its
containing Session is never a fallback control target.

## Security model, and what multi-user requires

Today: loopback bind, a process-local `HttpOnly` `SameSite=Strict` cookie whose token
rotates on restart, Host header validation, Origin validation on mutations. That is
adequate for a single-user local server and **is not an authentication system** — it knows
a request came from a browser that completed a local handshake, not who is asking.

Multi-user authentication is implemented with its boundary defined by
[ADR-0002](adr/0002-environment-workspace-subject.md): authenticated Subjects receive
explicit Environment grants. [ADR-0006](adr/0006-environment-authority-and-capability-grants.md)
settles who may issue such a grant, and splits it into `observe` and `control` because a
control grant reaches arbitrary code execution within `browse_roots` while an observe grant
reads a transcript. The bootstrap cookie remains an explicit local development identity;
registered Subjects and environment grants are the durable multi-user path. Non-loopback
envagent binding requires TLS.

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
- Horizontal scale-out. The authority is SQLite and a link is stateful in one process, so
  growth is **shard by session and route**, not stateless replicas. Anything else means
  moving to Postgres. Worth knowing now because it decides which of the two happens later;
  nothing in the API forecloses either.
