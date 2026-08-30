# Environments

Where a session's work runs, and the wire between there and the server.

One authoritative record on the server; agents execute wherever an **envagent** is
running. `docs/architecture.md` owns why the arrow points inward and why there is one
abstraction instead of three; this document owns the protocol, the spool, pairing, and the
two provisioning paths.

> **`crates/envagent` is designed, not built.** Everything below is the intended shape and
> is stated in the present tense as a specification. Today adapters run in-process in the
> server, so there is exactly one implicit environment — the machine the server is on.
> `docs/roadmap.md` tracks what remains.

## The envagent is the whole abstraction

An envagent is a small binary that dials out to a server, authenticates, and then
offers exactly four capabilities over one multiplexed connection:

| Capability | What the server asks for |
| --- | --- |
| **run** | start an agent session, carry its turns, report its events |
| **watch** | follow a session this envagent did not start, from the vendor's native store |
| **files** | read and write within a session's `workdir`, for attachments and the Artifacts pane |
| **health** | is this machine reachable, does it have the agent CLIs, what does it report |

There is no `Environment` trait with one implementation per substrate. Local, SSH,
and sandbox differ only in how an envagent came to exist, and after it exists they
are the same case. The adapters are **linked into the envagent**, so vendor
translation happens on the machine holding the vendor's files — the constraint that
made a server-side SSH implementation unworkable.

## `sylph.env.v1`

One TLS connection per envagent, initiated by the envagent, carrying framed
messages in both directions with independent streams multiplexed over it. A stream
is a session's event flow, a file transfer, or a control exchange.

*Decision: the envagent dials out.* A laptop behind NAT, an SSH host on a private
network, and a sandbox that auto-pauses are then all one case; a server that had to
reach in would work for exactly one of the three.

*Decision: adapter framing is not the wire.* Inside the envagent, an adapter is a
Rust call and events are values (`docs/session-protocol.md`). NDJSON survives only
where bytes genuinely cross a machine boundary — here. So the vocabulary is shared
with the adapter contract, but nothing depends on a subprocess existing.

### Message classes

| Class | Direction | Purpose |
| --- | --- | --- |
| `hello` / `welcome` | env → server, server → env | version, agents present, machine facts, resume cursor |
| `session.*` | server → env | open, send a turn, interrupt, steer, set config |
| `event` | env → server | one normalized event, sequenced per session |
| `ack` | server → env | durable through this sequence number, per session |
| `watch.*` | server → env | start or stop following a native session |
| `adopt.*` | server → env | bring in what a native store already holds |
| `files.*` | both | read, write, list, stat within a session's workdir |
| `health` | both | probe and report |

**Every event carries a per-session monotonic sequence, and the server acks by
sequence.** That pair is what makes a dropped connection lossless rather than a hole
in the record; § Spooling is why it cannot be skipped.

## Spooling, and why the record survives a dropped connection

The old shape was `adapter stdout → server → journal`, in one process,
synchronously. Now there is a network in the middle, and **a drop puts a hole in the
journal exactly at the tail of a turn** — which is the part AFK exists to deliver.

So an envagent keeps a **local durable spool**:

- Every event is appended to a local WAL-mode SQLite file **before** being sent.
- Rows are deleted only when the server acks past them.
- On reconnect, `hello` carries the last acked sequence per live session and the
  envagent resumes from there.
- The spool is bounded by size; on overflow the envagent stops the session's agent
  rather than dropping events, because a silent gap is worse than a stopped turn.

*Decision: the spool is not a second journal.* It is a bounded ack-gated buffer, and
it is authoritative for nothing. Two durable records would reintroduce the "which
copy is the truth" question the authority model exists to answer. Its only contract
is: nothing leaves it until the server has it.

This is the same shape as bringing in native sessions, differing only in what
travels — live events rather than parsed history — which is why there is one
mechanism and not two.

Three ordinary events exercise it, and it is a correctness requirement for all
three: closing a laptop lid, switching Wi-Fi, and a Khora sandbox auto-pausing on
idle.

### Renaming a WAL-mode SQLite file means checkpoint-then-rename

The spool is WAL-mode SQLite, so it inherits a rule this repo already paid for:
moving three files is wrong. A crash between renaming `-wal` and `.db` strands
committed data in an orphaned WAL beside a database that no longer references it.
`PRAGMA wal_checkpoint(TRUNCATE)`, close every connection, move the main file alone,
delete the emptied sidecars.

## Pairing, and who owns an envagent

An envagent joins by pairing, not by holding a long-lived shared secret in a config
file:

1. A user asks the server for a pairing code — short-lived, single-use.
2. `sylph-envagent pair --server <url> --code <code>` exchanges it for a durable
   credential and an opaque environment id.
3. The envagent stores the credential locally and dials in from then on.

The environment id is **opaque and minted here**, never a hostname or a
user-supplied name. `docs/architecture.md` § Identity is why: the id is in a
session's key, so a rename would destroy identity. Display names are mutable
metadata.

*Measured 2026-08-27:* `codex remote-control pair` uses short-lived pairing codes for
the same purpose, which is evidence the shape works rather than a guess.

**Who owns an envagent is unresolved and gates multi-user.** On a single-user install
it does not arise. The moment a server has two principals, an unowned envagent means
one user can run agents on another user's laptop —
`docs/architecture.md` § Security model carries this with the rest of that gate.

## How an envagent comes to exist

### Self-installed

A user installs `sylph-envagent` and pairs it. This is the case for a laptop, a
workstation, a build box. Nothing else is special about it, including on the machine
the server itself runs on — *decision: the server does not embed an envagent*, so
"the server's own machine" is not a privileged environment with its own code path.
That is the same class of special case as the session classes deleted in
`docs/architecture.md`, and it would drift the same way.

### SSH-bootstrapped

An existing envagent copies the binary to a host it can already reach and starts it
there. **The bootstrap runs from an envagent, not from the server** — which is the
whole point: whatever your machine can reach by SSH, your session list can reach.

The requirement on the target is only what SSH already implies: a shell, a writable
directory, and the agent CLIs the user wants to run.

Once started, the target has two possibilities, and they differ in **what AFK
promises**:

| | Target can reach the server | Target cannot |
| --- | --- | --- |
| connection | target dials in directly | the bootstrapping envagent relays |
| laptop may leave | yes | **no** — control dies with the relay |
| what a client shows | an ordinary environment | "reachable via <machine>" |

Both are real. A UI that showed them identically would promise AFK continuity it
cannot keep in the second case.

### Sandbox-provisioned (Khora)

The server asks Khora for a sandbox whose image contains an envagent, which then
dials in like any other. **Provisioning is server-side** — it needs Khora API
credentials and must not depend on any client being online — but the running sandbox
is not a special environment.

That makes a sandbox the **only** environment needing no client online at all, which
is the strongest AFK case in the system: start work from a phone, close everything,
it keeps running.

Two Khora properties shape the design rather than merely satisfying it:

**The data plane exists only while `running`.** Pause closes admission, drains, then
quiesces the guest. So auto-pause on idle is a feature, and a session's link dying is
expected rather than a fault. This is what the spool and the ack cursor are for.

**`Connect` replays a bounded tail and is explicitly not durable**, so it cannot be
the record. It is a reconnect convenience; the server's store is the record. Without
that distinction the sandbox path would quietly have weaker history than a laptop.

Khora models no caller, account, or work unit by design, and Sylph must not push
those into it. A sandbox is addressed by id; the mapping from session to sandbox lives
in the server's store. A Sylph feature that needs Khora to know about Sylph is a Sylph
bug.

## Watching and adopting a native session

Both bring in a session Sylph did not start, and they differ only in whether it is
still running. Both are **explicit acts** (`docs/architecture.md` § Data travels one
way).

**adopt** — the adapter parses what the native store already holds and emits it as
normalized events. One pass, then done. Fidelity is whatever the vendor wrote.

**watch** — the same parse, then follow the file as it grows, emitting events as they
appear. All three supported agents flush mid-turn, so a watcher sees a turn in progress
rather than only its end; `docs/agents.md` § Capability matrix has the per-vendor
granularity. Two `pi` hazards are recorded there too, because a watcher must be built
around them: it truncate-rewrites with no lock, so a reader can observe a half-written
file, and the file does not exist at all before the first assistant message — **an absent
file is not an absent session.**

A watched session's **control** facet is whatever that vendor offers a second party,
which is not universal (`docs/agents.md` has the table). Its **observe** facet works
regardless. This is the facet pair from `docs/architecture.md`, and a watched session
is not a third link state.

Offering "bring in everything on this machine" once, when an envagent first pairs, is
the right shape for the bulk case. A recurring background sweep is not: it would
publish sessions the user never associated with Sylph.

## Health is per environment

`/api/v1/health` reports each environment separately, because "the agent will not
start" has a different cause and a different fix per machine: a missing CLI locally,
an unreachable orchestrator or exhausted node for Khora, a dead connection for a
laptop that went to sleep. Collapsing those into one boolean is what turns a
five-second diagnosis into an hour.

An envagent reports which agent CLIs it found and their versions, so "which agents
can I use" is answered per environment rather than globally. A user picking an
environment with no `codex` on it should see that before creating the session, not
after.

## A degraded dependency is one fact per run, not one per attempt

**An unreachable envagent is a persistent state, so report it once at `error` and at
`debug` thereafter.** A per-attempt warning turns one missing dependency into thousands of
identical lines, and this repo has rebuilt that flood twice at different layers.

The fix belongs in **the client that holds the connection**, not in the handlers that poll
it: a sidebar and a picker both polling will otherwise reproduce the flood one layer up.
Let a caller ask whether the state was already reported rather than each caller
remembering.
