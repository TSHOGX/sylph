# Architecture

Sylph Web is a coding-agent workspace. A **server** holds the authoritative record
of every session and owns all session policy. An **envagent** runs on each machine
where agents actually execute and dials back to the server. A **web frontend** is a
transport onto the server, and so is anything else.

This document owns the system's shape, the session model, identity, and the
authority model. It absorbs what used to be spread across `architecture.md`,
`data-architecture.md`, and `control-plane.md`; those three argued the same
questions in three places, which is how they came to disagree.

Claims are marked. *Measured* means observed against a real install on the stated
date, with the evidence named. *Decision* means a design choice this document makes,
which a reader may disagree with as a judgement rather than as a fact.

## Topology

```
    browser            other frontends
       │                      │
       └──────────┬───────────┘
                  │  HTTP + SSE, same origin
        ┌─────────▼──────────────────────────────┐
        │  sylph-server  (+ web, same origin)    │
        │  sessions · turns · links · policy     │
        │  journal.db · history.db · blobs/       │
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

*Decision: the envagent connects outward to the server, never the reverse.* This is
what makes a laptop behind NAT, an SSH host on a private network, and an
auto-pausing sandbox all the same case. A server that had to reach in would work
for exactly one of the three.

## An environment is an envagent, and that is the whole abstraction

There is no `local` / `ssh` / `khora` trichotomy in the code. There is one wire
protocol, `sylph.env.v1`, and environments differ only in **how an envagent came to
exist**:

| Appearance | Mechanism |
| --- | --- |
| self-installed | a user installs `sylph-envagent` and pairs it |
| SSH-bootstrapped | an existing envagent copies the binary to a host it can reach and starts it |
| sandbox-provisioned | the server asks Khora for a sandbox whose image contains an envagent |
| development | `cargo run -p sylph-envagent` beside a dev server |

An earlier design had an `Environment` trait with one implementation per substrate,
which put SSH and sandbox process control inside the server. That was wrong in a
specific way: the adapter has to read the vendor CLI's native store, resolve its
config, and inherit its skills and project files, all of which are local facts on
the machine the CLI lives on. A server-side SSH implementation therefore needs a
remote filesystem for every vendor detail it touches. Putting the adapter *in* the
envagent removes the question — and once it is removed, the three implementations
collapse into one.

*This is the model Cursor uses*, and the reason is the same: it does not drive a
remote host over a connection, it copies its own component there and runs it. The
consequence for us is the one you want — **whatever your machine can reach by SSH,
your session list can reach**, because the bootstrap runs from your envagent rather
than from the server.

`docs/environments.md` is authoritative for the wire protocol, spooling, pairing,
and the SSH and sandbox paths.

### There is no "browser local"

A browser cannot fork a process, so no frontend can host an environment. What a web
user gets instead is every envagent currently connected, **including the one on the
laptop they are sitting at**. That is the same capability by a different route, and
the UI must not imply the other thing exists.

## One class of session

*Decision.* There is one kind of session. Sylph does not divide sessions by who
started them.

The previous model had `origin` (`sylph` | `native`), `identity_mode`, and
`lifecycle_mode` on every row, and the former `data-architecture.md` argued at length that
the boundary "must not be smoothed over". The implementation says otherwise.
*Measured 2026-08-27, `backend/src`:*

| Column | Write sites | Behavioural branches |
| --- | --- | --- |
| `lifecycle_mode` | 4 | **0** — stored in SQLite, read by nothing |
| `identity_mode` | 4 | **1** — `links/mod.rs:468`, chooses `SOUL` path or empty string |
| `origin` | every insert | **0** in the backend; 1 badge, 1 banner, 1 search colour, 1 `deleteEmpty` guard in the frontend |

So a whole section of invariants rested on one `Option<String>`. That is the
inverse of the failure `AGENTS.md` warns about: not a spec blessing an
implementation accident, but a spec hardening a distinction the implementation never
made.

Three separate arguments say the split was wrong rather than merely unbuilt:

**Who holds stdio is a property of a link, not of a session, and it flips both
ways.** After its process exits, a session Sylph started is byte-identical on disk
to one started in a terminal. After Sylph resumes a terminal session, Sylph holds
its stdio. Any class written onto the session row lies the first time either
happens.

**Identity injection has no relationship to who started the session.** Adopting a
terminal session and then injecting `SOUL.md` from that point on is a coherent thing
to want, and so is starting a session from Sylph with no injection at all. So
injection is a per-session boolean with a default, not a class.

**Sylph has no lifecycle policy of its own.** The 24h-idle / 04:00-reset rule was
inherited from the Discord chat daemon's `chat/state.py`. It is deleted, not
deferred: nothing schedules it, `end_incarnation` has no callers, and a reset would
break the one property this document most depends on — a stable identity — by
replacing the agent-native id mid-session. The **link** idle timeout is a different
and real thing (30 minutes) and stays.

What is deleted: `sessions.origin`, `sessions.identity_mode`,
`sessions.lifecycle_mode`, the whole `backend_incarnations` table,
`store::end_incarnation`, and every frontend surface that renders an origin.

What replaces the one branch that did work: `sessions.inject_identity`, a boolean,
default true for a session Sylph starts and false for one it adopts. **A default,
not a policy** — it is settable at creation and refused afterwards, because a
transcript records what produced it and rewriting the flag would rewrite what the
stored history means.

### What genuinely differs, and where it belongs

Two real differences survive, and neither is a session class.

**How events are obtained** is an adapter mode (§ `docs/session-protocol.md`): hold
the process's stdio, or read the vendor's native store. One vocabulary comes out
either way.

**How much resolution the record has** is `fidelity` on the record itself —
`stream`, `blocks`, or `messages`. Note this is *not* correlated with who started
the session: a session Sylph started, read back from the vendor's native store
after the process died, is message fidelity too. A renderer reads the field to
decide whether it may draw a duration; a waterfall over message-fidelity data would
present equal-length bars as measurements.

## Identity is `(environment, agent, native_id)`

*Decision.* A session's id is the triple, and there is no Sylph-minted UUID.

The evidence is better than the previous design assumed. *Measured 2026-08-27:*
`claude`'s adapter already mints a UUID and hands it to the vendor with
`--session-id` (`adapters/claude/adapter.py:425`); `claude`, `pi`, and `gemini` all
accept `--session-id`; `codex` and `opencode` assign their own and report it. So the
native id is available to Sylph for every agent, either by dictation or on the first
frame, and a second Sylph id is pure redundancy — plus a permanent invitation to
compare ids from two spaces, which is silently always false (`AGENTS.md` records the
instance that shipped fully tested and pruned nothing).

Rules the triple needs to hold:

- **`environment` is an opaque id minted at pairing**, never a user-visible name.
  A name in a key means renaming destroys identity. Display names are mutable
  metadata.
- **The encoding does not use `/`.** This repo has already paid for that once: 350
  of `pi`'s 372 model ids contain a slash, so splitting on one mis-parses the
  majority. The wire form is `<env>:<agent>:<native>` with `env` restricted to
  base32, and a URL path segment carries it percent-encoded.
- **A session exists once an agent has given it an id**, which means `POST
  /sessions` starts the agent rather than reserving a row. This kills the "draft
  session with no work in it" concept, and with it `DELETE /sessions/{id}` for
  empties and the frontend's `deleteEmpty` probe. A failed spawn now reports at the
  start screen instead of leaving a dead row behind.
- **The id is stable for the session's whole life**, because nothing resets an
  incarnation any more.

Three alternatives, each rejected for a reason that would have surfaced late:

- **A content fingerprint.** `opencode` writes no per-session file — one SQLite
  store with a live WAL — so it cannot compute one (*measured*). A key one agent
  cannot produce is not a key. (`opencode` is now unsupported, but the shape of the
  argument is why the rule stands: nothing constrains an agent to keep a file.)
- **An archive id.** A session is observable the moment its native store exists,
  before anything has parsed it. Keying on a projection makes identity depend on
  that projection's progress.
- **Anything containing a host or a path.** Provenance, not identity; see below.

## Sessions do not migrate between environments

*Decision, and the load-bearing one.* A session belongs to the environment it was
born in, for life. Sylph offers no move, no resume-elsewhere, and no reconciliation
of two copies.

Nearly all of this document's simplicity is bought with it:

- No freshness comparison between two environments, because two environments never
  hold candidate copies of one session.
- `cwd` is unambiguous. Two machines both having `/home/user/app` is not a
  collision, because a path is only ever read alongside the environment that named
  it.
- Upload can never conflict. It either brings a session the server has not seen, or
  a longer prefix of one it has.

Moving work elsewhere is a **fork**: a new session with a recorded ancestor.

### Provenance is not identity

`environment` is in the key because it names a birthplace, which does not change.
`host_id`, hostnames, and paths are provenance: shown to a user, used for
diagnosis, never an input to a comparison. And content-addressed objects stay
**host-blind** — keyed by hash alone, so the same dependency source uploaded from
three machines is stored once. A host column on the object table destroys exactly
that.

## One authority, one store, one vocabulary

The server's own store under `~/.sylph` is the authoritative record. Everything
else is a source it ingests from or a projection it serves.

*Decision: there is exactly one event vocabulary and one durable store, and this is
the largest change in this rewrite.* The previous design had two of each — an
adapter protocol feeding `journal.db`/`history.db`, and trace-db's parsers feeding
`archive.db` — meaning two per-vendor normalizers, two schemas, and two query
paths for one concept. That is a drift generator, and it produced the open question
the old document could not close: full-text search covered only what the archive
had parsed, so a session whose environment had gone away was searchable nowhere.

Under one vocabulary, native history is parsed *by an adapter* and emitted as the
same events a live turn emits. It lands in the same journal → history pipeline,
carries a `fidelity` marker, and is indexed by the same FTS table. The open question
is answered by construction rather than moved.

What this deletes: `archive.db` as a separate store, `backend/src/tracedb/mod.rs`
(1440 lines of client plus per-operation spelling pins), the reachable/unreachable
distinction, the `tracedb.bin` setting, and the two-id-space hazard in full.

What it costs, stated plainly: the vendor-format knowledge in trace-db's
`parsers/` must move into Sylph's adapters. *Measured:* that is **1219 lines** for
the three supported agents (`claude.rs` 388, `codex.rs` 561, `pi.rs` 270), against
1440 lines of client code deleted — a net subtraction before counting the 867 lines
of `gemini.rs` and `opencode.rs` that go away with those agents.

`docs/storage.md` owns the tiers and the fold format. `docs/agents.md` owns each
vendor's native format and quirks.

### Data travels one way: in

The server never pushes history outward and never writes session data outside
`~/.sylph`. There is no bidirectional sync, no last-writer-wins, no vector clock,
and no merge — every one of those exists to answer "which of two conflicting writes
wins", which the no-migration invariant means nobody asks.

Two inbound paths:

- **A live session** arrives as a consequence of running. The envagent streams
  normalized events to the server as they happen, so there is no moment at which
  the data is only in one place — see § Spooling in `docs/environments.md` for what
  makes that true across a dropped connection.
- **An existing native session** is brought in by an explicit act: **watch** it
  (follow it as it runs) or **adopt** it (bring in what is already recorded).

*Decision: bringing in native sessions stays explicit rather than becoming an
automatic sweep of every store on the disk.* "Everything on this machine is now on
the server" is not a default a tool picks on someone's behalf. Offering it as a
one-time choice when an envagent first pairs is fine, and is the shape to build.

What travels is what the adapter produced from the native bytes it read locally,
which is why the adapter runs in the environment. Uploading rows the server has to
trust would put the schema in every environment and require upgrading them in
lockstep.

## One link, two typed capability facets

*Decision.* A link has one state and two facets, each of which can report
unavailable with a typed reason: **observe** and **control**.

A third link state was considered and rejected, because the vendors do not sort into
three buckets (*measured 2026-08-27*): a terminal `pi` is observable and
undrivable, `codex` is both, `claude` is observable and semi-drivable behind a
human approval gate. A third enum variant would have to pick one and would
misreport the others.

Two inversions in the current code have to be repaired with it. `links/mod.rs:61`
documents an empty capability surface as meaning *no process exists*, which is
exactly backwards for a session running under someone else — the process is alive
and it is Sylph that cannot reach it. And both `LinkFailureCode` values (`NoAdapter`,
`HandshakeFailed`) describe *our own spawn* failing, so neither can say "somebody
else's process, running fine, not ours to drive".

This is also what makes "another machine is read-only" fall out rather than being a
rule anyone writes: **control is available exactly while that environment's envagent
is connected.** Disconnected, observe still answers from the server's store.

## Observation is multi-party; control is single-owner

Two questions get conflated because both read as "can two things touch one session".

**Observation may be concurrent.** Several clients, a terminal and a browser
together, more than one person — all reading is safe, because reading creates no
branch. This is what makes the AFK case work: the terminal keeps going while the
phone watches.

**Control must have one owner, with explicit handoff.** *Measured:* none of the
agent CLIs takes a lock, and two writers to one session produce sibling branches off
a single parent pointer with zero errors at the storage layer. The damage is purely
semantic, which is why nothing catches it. `claude`'s own approval gate is a vendor
implementing this same idea, which is evidence the shape is right.

## AFK degrades in a defined order

The motivating scenario is the reason this architecture exists: work with an agent
in a terminal, walk away, see it from a phone, continue it. It is not one
capability. *Measured 2026-08-27 against claude 2.1.247, codex, pi:*

| | claude | codex | pi |
| --- | --- | --- | --- |
| mid-turn native writes | tool-call and content-block boundaries | item completion | every `message_end` |
| second-party control | authenticated per-process UDS: delivers messages, **cannot drive** | app-server daemon, `remote-control pair`, `queue --thread` | **none** — stdio only |
| resume after exit | yes | yes | yes |

So: **observe always, resume always, live continuation where the vendor lets a
second party in.** A client renders "observable, not drivable" as a first-class
state, never as a failure.

`claude`'s inbound `user` frames are gated on the sitting human's approval — the
binary carries "held for the recipient user's approval", declined, and expired
strings. Whether the gate is per-frame or once per peer is **unresolved**, and it is
the difference between useful remote continuation and a feature that fails precisely
when nobody is at the keyboard. `docs/agents.md` carries the per-vendor detail.

## Turn execution

A **turn** is a durable resource, not an HTTP request's lifetime:

1. `POST /sessions/{id}/turns` with an idempotency key → `202` and a turn id
2. The server ensures a link, then sends the turn on it
3. The client streams `GET /turns/{id}/events` (SSE), reconnecting with
   `Last-Event-ID`
4. The turn settles when the adapter reports an outcome — there is no process exit
   to read, because the adapter is long-lived and a turn is a message on it
5. A server restart marks in-flight turns `interrupted`

One turn per session at a time; a second is refused (`SESSION_ALREADY_RUNNING`)
rather than queued, because the link holds one conversation.

**An agent may start a turn.** *Measured on claude 2.x:* a session with
backgrounded work emits the delegate's completion after its turn's `result`, then an
entire exchange of its own to report it. So the server opens an agent-initiated turn
(`origin: "agent"`) on the first out-of-turn event of any kind. Such a turn has no
waiter, its request text is empty, and a client that ignores `origin` will render an
answer to a message the user never wrote. This is turn `origin` and is unrelated to
the deleted session `origin`.

Two orderings here are load-bearing and were each paid for:

- **`stop` settles the turn before touching the link.** An agent that stops
  producing looks like a lost link to the drain task, which would report `LINK_LOST`
  for something the user asked for. Interrupting first was a race that failed
  `stopping_a_running_turn_cancels_it` about two runs in five.
- **A session returns to `idle` before its turn is marked terminal.** These are two
  writes and a client will observe between them, so the row that makes the next
  action legal is written first. The other order leaves a finished turn on a
  `running` session, which is the state that makes the composer refuse the next
  message.

## Security model, and what multi-user requires

Today: loopback bind, a process-local `HttpOnly` `SameSite=Strict` cookie whose
token rotates on restart, Host header validation, Origin validation on mutations.
That is adequate for a single-user local server and **is not an authentication
system** — it knows a request came from a browser that completed a local handshake,
not who is asking.

*Decision: multi-user gets a place in the model, not an implementation.* One person
with a laptop, a phone, and a browser tab needs no identity model; they need the
authority to be the server, which it now is. What stays so the door is not closed:
every mutation is a server operation, so an authorization check has one home;
request resolution records who resolved; and both environment binding and session
ownership are per-session, which is the unit a boundary would follow.

The topology this document describes makes three of the unsolved questions sharper,
and **"many users reach it over the web" requires all of them answered first**:

- Identity, per-user session visibility, transport security, CSRF beyond the
  bootstrap cookie.
- **Who owns an envagent.** New with this design and with no home in the current
  model. Without it, one user can run agents on another user's laptop.
- `browse_roots` becomes an **authorization boundary** rather than a convenience,
  because it bounds where an agent may execute. It is currently per-install.

Binding to a non-loopback interface stays a separate, explicit change and must not
be reached by relaxing the bind address.

### Filesystem exposure

A client string becomes a filesystem operation on three surfaces, and the *check* is
shared (`paths::contain_in_any`: canonicalize, then compare component-wise) because a
containment rule written twice is one that will diverge.

| Surface | Root | Scope |
| --- | --- | --- |
| `GET /sessions/{id}/files*` | the session's recorded `workdir` | list, read, parse, raw bytes |
| `POST /sessions/{id}/files` | same | create; no overwrite by default |
| `GET /fs/{roots,tree}` | `browse_roots` (`$HOME` + configured) | directory names only |

Two invariants that were each paid for in a real hole:

**Derive the path you operate on from the check itself.** Canonicalize once, into
one variable, and use that variable. A `contain_in_any` result paired with a path it
did not produce is not a bound — that shape let a read canonicalize for the check and
then open the original path, following a symlink out of the workdir.

**A write cannot canonicalize its own target**, because the file does not exist yet,
so containment sits on the parent and the last component is appended verbatim. A
pre-existing symlink there is inside the root by name and points anywhere. Inspect
with `symlink_metadata` first, then resolve and re-check.

With environments, both checks live in the **envagent**, evaluated against that
machine's own configuration. The server never resolves a remote path.

## What is deliberately not built

- Session migration between environments (§ above). Forking is the answer.
- Multi-user authentication and per-user visibility (§ above).
- Horizontal scale-out. The authority is SQLite and a link is stateful in one
  process, so growth is **shard by session and route**, not stateless replicas.
  Anything else means moving to Postgres. Worth knowing now because it decides
  which of the two happens later; nothing in the API forecloses either.
- A compatibility layer for anything. `AGENTS.md` § Compatibility applies: no other
  consumer exists, so schemas, wire formats, and internal APIs change in place.
