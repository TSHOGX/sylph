# Environments

Where a session's work runs, and the wire between there and the server.

One authoritative record on the server; agents execute wherever an **envagent** is
running. `docs/architecture.md` owns why the arrow points inward and why there is one
abstraction instead of three. This document owns the protocol, the spool, pairing, agent
availability, and the two provisioning paths.

Wire shapes are generated into `docs/sylph.env.v1.schema.json`; the semantic types live
in `crates/protocol/src/env.rs`.

## The envagent is the whole abstraction

An envagent is a small binary that dials out to a server, authenticates, and then offers
four capabilities over one multiplexed connection:

| Capability | What the server asks for |
| --- | --- |
| **run** | start an agent session, carry its turns, report its events |
| **watch** | follow a session this envagent did not start, from the vendor's native store |
| **files** | read and write within a session's `workdir`, for attachments and the Artifacts pane |
| **health** | is this machine reachable, which agent CLIs does it have, at what versions |

Local, SSH, and sandbox differ only in how an envagent came to exist; afterwards they are
one case. The adapters are **linked into the envagent**, so vendor translation happens on
the machine holding the vendor's files.

### Every environment is an envagent, including the server's own machine

There is no in-process path. A server with no envagent connected can hold and serve
records but can run nothing, and it says so: session creation fails with a message naming
the missing envagent and how to pair one, rather than falling back to spawning an agent
beside itself.

A single-machine install is therefore **two processes**, `sylph-server` and
`sylph-envagent`, with two roots and two config files (`docs/configuration.md`).

## `sylph.env.v1`

One connection per envagent, initiated by the envagent, carrying framed messages in both
directions with independent streams multiplexed over it. A stream is a session's event
flow, a file transfer, or a control exchange.

The envagent dials out, so a laptop behind NAT, an SSH host on a private network, and a
sandbox that auto-pauses are one case.

Adapter framing is not the wire. Inside the envagent an adapter is a Rust call and events
are values (`docs/session-protocol.md`); NDJSON survives only where bytes cross a machine
boundary. The vocabulary is shared with the adapter contract without anything depending
on a subprocess.

Plaintext transport remains loopback-only. A non-loopback envagent listener requires the
configured rustls certificate and key, and a remote envagent validates the server certificate
against its configured CA before the wire handshake.

### Message classes

| Class | Direction | Purpose |
| --- | --- | --- |
| `hello` / `welcome` | env → server, server → env | version, agents present, machine facts, resume cursor |
| `session.*` | server → env | open, send a turn, interrupt, steer, set config |
| `event` | env → server | normalized events carrying producer sequence and source provenance |
| `native_blob_chunk` | env → server | bounded, ack-gated chunk of a native sidecar snapshot |
| `ack` | server → env | durable through a contiguous producer prefix |
| `agents` | env → server | the agent inventory changed (§ Agent availability) |
| `watch.*` | server → env | start or stop following a native session |
| `adopt.*` | server → env | bring in what a native store already holds |
| `files.*` | both | read, write, list, stat within a session's workdir |
| `health` | both | probe and report |

**The producer sequence and the canonical server sequence are distinct.** The envagent
allocates a monotonic producer sequence before spooling; the server assigns the session's
canonical sequence inside its durable ingest transaction. A dropped connection resumes
from the first unacknowledged producer item rather than guessing from a remote clock.

Native adopt and watch events arrive without a server turn id, so ingest assigns one
server-owned container per source. The vendor stream is unchanged; the container is what
makes each event reachable through the per-turn history API. A terminal event closes it,
and a finite source emits an ordered close diagnostic at EOF.

Native records that name a large sidecar are accompanied by `native_blob_chunk` messages.
The envagent opens the validated vendor locator on its machine, snapshots it in bounded
chunks, and spools those chunks before the record. The server retains the complete body in
CAS before acknowledging the record; a locator is never opened by the server and is never
used as a backup after projection.

File commands carry the session's recorded `workdir` from the server. The envagent
canonicalizes that root and confines every operation to it; a process-local hosted link
or vendor-store scan is not a prerequisite. Restarting an envagent or narrowing its
browse roots therefore cannot discard the file root of a session already recorded.

A transfer handle is published only after its last sidecar producer sequence is
acknowledged. Finite source EOF likewise waits through its close diagnostic. Both waits
target a fixed producer prefix; newer events cannot extend their work indefinitely.

**History work cannot consume interactive command capacity.** Native enumeration,
adoption, following, and re-reading share a bounded pool separate from file access and
session control. Saturation returns `COMMAND_OVERLOADED` before execution, with no
hidden queue whose wait can exceed the caller's deadline. Independent commands run
concurrently; mutations for one session or prepared draft retain wire arrival order.
Agent inventory probes run independently of command reception. A disconnected epoch
cancels unfinished command tasks and releases its acknowledgement waiters; it never
replays an ambiguous at-most-once operation automatically.

Stateless reads use fresh replayable request identities and connection-local waiters.
File listings and reads, catalogue enumeration, capability and health probes, and native
transfer chunk reads do not persist command receipts or response bodies. Their deadline
includes socket queueing, and late replies after timeout or disconnect are discarded.
Mutating commands retain the durable ambiguity and outcome ledger, so this optimization
cannot make a vendor input retryable after an unknown outcome.

## Agent availability

Which agents an environment can run is discovered, not configured, and it is a fact about
that one machine.

**The server's adapter set is fixed at build time.** `sylph-server` knows the vendors its
binary was compiled with, which bounds what any environment may report. The `e2e` cargo
feature replaces that set with the fixture adapter rather than adding to it, so no
production build has a path to it.

**Availability is `PATH`, not a registry.** There is no discovery pass, no adapter
directory, and so no map that can be wrong — the mechanism that had one produced this
repo's two worst silent failures, and both are the same defect: a lookup that answered
plausibly instead of failing. Zero discovered agents made `/` a dead keystroke three
layers from its cause, and a fixture adapter left reachable failed an entire E2E suite
against an agent that did not exist. "Why can I not use this agent" now has one answer,
one fix, and an error message able to say both.

**An environment's set is discovered per connection.** The envagent probes each vendor it
was built with — is the CLI present, and does its version meet the adapter's floor — and
carries the result in `hello`. A CLI that is present but too old is reported as
unavailable *with its version*, which is a different fix from installing it.

**It is re-discovered over the life of the connection.** An envagent watches for changes
and sends an `agents` message when its inventory moves, so upgrading a vendor CLI or
installing a new one takes effect without re-pairing or restarting the server. A machine
that starts with only `claude` usable, then upgrades `codex` past the floor and installs
`pi`, reports all three from that moment. The watcher and the `agents` message complete
the live half of the Milestone 1 exit gate in `docs/roadmap.md` § Milestone 1.

So availability is per environment and time-varying, and `GET /api/v1/environments` is
where it is read. A picker offering an agent must name the environment it will run in,
since the answer differs across environments and the failure otherwise arrives after
creation rather than before it.

## Turn control receipts

A session's unique turn generation is carried as a TurnLease. Send receipts confirm the
lease; guarded steer must match the envagent's open lease and the vendor's captured turn
identity. Native queue receipts describe submission to the harness channel and never name
a turn or assert consumption. A vendor refusal is typed separately from a transport error:
only a negative vendor response or local validation proves rejection. An unanswered command
remains unknown.

The envagent spools adapter output without waiting for network acknowledgement, then drains
the spool through a bounded delivery wake-up. The server reads command replies independently
of ordered event ingestion. This breaks the wait cycle between a command holding the session
sequencer and lifecycle events preceding its reply. A bounded ingress queue may disconnect
on overflow; the durable producer prefix replays on reconnect. Batch boundaries never defer
a turn's settlement past an event that opens its successor.

## Spooling, and why the record survives a dropped connection

There is a network between the adapter and the journal, so a drop would put a hole in the
journal at the tail of a turn — the part AFK exists to deliver.

An envagent therefore keeps a **local durable spool**:

- Every event is appended to a local WAL-mode SQLite file **before** being sent.
- Rows are deleted only when the server acks past them.
- On reconnect, `hello` carries the last acked sequence per live session and the envagent
  resumes from there.
- The spool is bounded by size; on overflow the envagent stops the session's agent rather
  than dropping events.

The spool is a bounded, ack-gated buffer and is authoritative for nothing: two durable
records would reintroduce the "which copy is the truth" question the authority model
exists to answer. Its only contract is that nothing leaves it until the server has it.

Three ordinary events exercise it, and it is a correctness requirement for all three:
closing a laptop lid, switching Wi-Fi, and a sandbox auto-pausing on idle.

### Overflow is a fact in the record

Stopping the agent bounds the damage without describing it, so overflow records
`close_reason = spool_full` on the turn (`docs/session-protocol.md` § Outcomes), and the
trajectories the sweep closes carry `abandoned`: the vendor reported no failure, and
`abandoned` already means "nobody reported an outcome" while staying non-final. A reader
asking whether anything is missing gets the answer from the record rather than from a log
on another machine.

The reason comes from the closed `CloseReason` set rather than a string this section
invents, because a dropped connection produces the same shape one layer over
(`link_lost`).

Stopping the agent is what the envagent must do: a reader loop that exits while the agent
keeps producing converts a bounded gap into an unbounded one. The vendor's own store
still holds what the spool refused, so recovery is `adopt` on that session — the same
mechanism that brings in any native history, and a property of the reason rather than of
this condition, since `spool_full`, `link_lost`, `agent_exited`, and `grant_revoked` all
leave the vendor's store ahead of Sylph's.

The spool maintains its capacity counter with insert/delete triggers in the same
transaction as queue mutations. An append reads one counter, never a sum over the entire
backlog: network backpressure must not make every subsequent enqueue more expensive.
The collapsed schema in `crates/envagent/migrations/spool/` is checksum-validated at open,
so schema churn requires discarding the development spool rather than silently mixing
old rows with new counters. Producer sequence allocation and enqueue remain atomic.

### Moving the spool means checkpoint-then-rename

The spool is WAL-mode SQLite, so relocating it is not a move of three files. A crash
between renaming `-wal` and `.db` strands committed data in an orphaned WAL beside a
database that no longer references it. `PRAGMA wal_checkpoint(TRUNCATE)`, close every
connection, move the main file alone, delete the emptied sidecars — an emptied sidecar
left in place is adopted by the next database opened at that path.

## Pairing, and who may authorize an envagent

An envagent joins by pairing rather than by holding a long-lived shared secret in a
config file:

1. A user asks the server for a pairing code — short-lived, single-use.
2. `sylph-envagent pair --server <url> --code <code>` exchanges it for a durable
   credential and an opaque environment id, and records the wire endpoint (the HTTP port
   plus one, per `docs/configuration.md` § Ports).
3. The envagent stores the credential locally and dials in from then on.

The environment id is **opaque and minted here**, never a hostname or a user-supplied
name: the id is in a session's key, so a rename would destroy identity
(`docs/architecture.md` § Identity). Display names are mutable metadata.

An envagent is an execution identity rather than a user's personal agent.
[ADR-0006](adr/0006-environment-authority-and-capability-grants.md) is authoritative for
who may authorize a Subject on it; the rest of this section is the resulting flow.

### Authority is fixed at pairing

Pairing derives the environment's `grant_authority` from how it came to exist and never
changes it. `machine` means that machine's OS-user authorizes; `server` means a Subject
flagged `is_admin` does, which is what a sandbox needs, since no user is sitting beside
it.

### Three capabilities, two of them grantable

`observe` reads. `control` drives a session and creates one within `browse_roots`.
`administer` — editing `browse_roots`, agent configuration, issuing and revoking grants,
unpairing — is the authority root itself and is never granted, so a Subject holding a
grant cannot pass one on.

`administer` being ungrantable is separate from whether an operation has a route. Its
carrier splits by where the state lives: `browse_roots` and agent configuration are the
envagent's own config and have no route, because only the machine holding a path can
canonicalize it; revoking a grant and unpairing change server tables and are server
routes gated by the authority root. `docs/api/` is authoritative for both the
split and each gate.

### Invite adds a Subject to an environment that already exists

`sylph-envagent invite --capability observe --ttl 15m` mints a code; the Subject redeems
it while authenticated, and the server records the grant. The command **runs on the
machine and requires that machine's OS-user** — `sylph.env.v1` carries no message that
issues a grant, since a wire message would let one compromised envagent admit new
Subjects to itself.

Invite and pair share the code shape and stay separate flows: an invite reaching the
pairing path would mint a second environment id for a machine that already has one, and
that id is a component of session identity.

The server validates five properties on redemption: environment scope, capability,
expiry, single consumption, and issuer provenance. Scope is the load-bearing one — a
grant asserts only over the environment that issued it.

### Revoking reaches work already in flight

Revocation refuses new control, and any dispatched vendor operation that cannot be
recalled is recorded as `abandoned` with `close_reason = grant_revoked` on the turn it cut
short. The caller's `authorization` becomes `not_granted`; the link's facets do not move,
because a facet describes the environment rather than the caller
(`docs/architecture.md` § A facet describes the environment, never the caller).

### Disclosure happens twice

Pairing states that this machine's sessions, transcripts, and native history will reach
the server. An invite states the concrete reach being handed over: the resolved
`browse_roots`, the agent CLIs present, the raw transcript exposure, and the capability
conveyed. Pairing is a user configuring their own machine; an invite gives that reach to
someone else, and only resolved values describe it truthfully.

### Unpairing is the terminal operation, and it keeps every session

`sylph-envagent unpair`, or `DELETE /api/v1/environments/{id}` from the authority root,
ends the relationship: the credential is destroyed, the connection closes, and no future
one is accepted.

**Every session that environment produced is retained**, and this is the point of having
the operation. The server's store is the authority (`docs/architecture.md` § One
authority), so a record's survival cannot depend on the machine that produced it still
being paired — a machine that has been decommissioned is exactly when its transcripts
matter most.

What changes is the link, not the record. Unpaired sessions report `observe` available
from the store and `control` unavailable with `environment_unreachable`, the same as any
disconnected environment, and they remain listable, readable, and searchable. Retention
treats the environment as unreachable rather than as empty, so it leaves the watermark
untouched and nothing is pruned on the strength of a question that can no longer be asked
(`docs/storage.md` § Retention). The session becomes terminal only when the server has an
explicit no-native-source fact; unpairing alone is not that fact.

Re-pairing that machine mints a **new** environment id and a new set of sessions.
Sessions do not migrate between environments, and an id is not reissued.

## How an envagent comes to exist

### Self-installed

A user installs `sylph-envagent` and pairs it: a laptop, a workstation, a build box, and
the server's own machine.

### SSH-bootstrapped

An existing envagent copies the binary to a host it can already reach and starts it
there. **The bootstrap runs from an envagent, not from the server**, which is what makes
whatever your machine can reach by SSH reachable from your session list.

The requirement on the target is what SSH already implies: a shell, a writable directory,
and the agent CLIs the user wants to run.

Once started, the target has two possibilities, and they differ in what AFK promises:

| | Target can reach the server | Target cannot |
| --- | --- | --- |
| connection | target dials in directly | the bootstrapping envagent relays |
| laptop may leave | yes | no — control dies with the relay |
| what a client shows | an ordinary environment | "reachable via <machine>" |

Both are real, and a client must distinguish them: shown identically, the second promises
AFK continuity it cannot keep.

### Sandbox-provisioned (Khora)

The server asks Khora for a sandbox whose image contains an envagent, which then dials in
like any other. **Provisioning is server-side**, since it needs Khora API credentials and
must not depend on a client being online, while the running sandbox is an ordinary
environment.

Khora is a separate project, maintained at [TSHOGX/khora](https://github.com/TSHOGX/khora).
Its development and deployment environment is the local OrbStack `sbx` microVM. When
implementing or integrating the Khora side of this boundary, inspect and run the checkout
inside `sbx` first; the host checkout of Sylph is not the authoritative Khora source. Sylph
and Khora may need coordinated changes, so treat the two repositories as a joint development
surface and verify the wire and lifecycle contract at their seam.

That makes a sandbox the only environment needing no client online at all, which is the
strongest AFK case in the system: start work from a phone, close everything, it keeps
running.

Two Khora properties shape the design:

**The data plane exists only while `running`.** Pause closes admission, drains, then
quiesces the guest, so auto-pause on idle is a feature and a link dying is expected. This
is what the spool and the ack cursor are for.

**`Connect` replays a bounded tail and is explicitly not durable**, so it is a reconnect
convenience rather than a record.

Khora models no caller, account, or work unit by design, and Sylph must not push those
into it. A sandbox is addressed by id, and the mapping from session to sandbox lives in
the server's store.

## Watching and adopting a native session

Both bring in a session Sylph did not start, and they differ only in whether it is still
running. Both are explicit acts (`docs/architecture.md` § Data travels one way).

**adopt** — the adapter parses what the native store already holds and emits it as
normalized events. One pass, then done. Fidelity is whatever the vendor wrote.

**watch** — the same parse, then follow the file as it grows. All three supported agents
flush mid-turn, so a watcher sees a turn in progress rather than only its end;
`docs/agents.md` § Capability matrix has the per-vendor granularity and the hazards a
watcher must be built around.

A watched session's **control** facet is whatever that vendor offers a second party, which
is not universal; its **observe** facet works regardless. This is the facet pair from
`docs/architecture.md`, and a watched session is not a third link state.

### A product surface bringing a session in asks to follow it

The two modes are an *endpoint* distinction, not a question to put to a user. A client
offering "bring in this session" sends `watch: true`, and the frontend's picker does so
unconditionally — a session brought in without following is a snapshot that stops at the
moment it was taken, and the terminal it came from goes on writing.

Two shapes were rejected, both of which fail by producing a plausible session rather than
an error:

- **Deciding from the row** — treating a recorded `endedAtMs` as "this one is finished, one
  pass is enough". An absent end stamp is not an ended session, and a session idle when it
  was listed and resumed in the terminal afterwards is exactly the case a user would then
  report as Sylph not updating.
- **A per-row toggle** — which turns the same outcome into a choice the user is blamed for,
  in a surface whose whole value is that one click is the whole interaction.

Following a store that has stopped growing costs a follower that never fires, which is why
the safe default is the unconditional one. What stays out of bounds is the *unasked* case:
this follows a session a user named, and never one Sylph found on its own.

Offering "bring in everything on this machine" once, when an envagent first pairs, is the
right shape for the bulk case. A recurring background sweep would publish sessions the
user never associated with Sylph.

### Re-reading a session Sylph already holds

A session adopted from a terminal keeps growing in the vendor's store after the one pass
that brought it in. **Re-read** is that pass again, on demand, for a session that already
has a row: the adapter parses the native store from the beginning and the server ingests
only what its journal does not already hold.

The pass is a disk-backed, ordered message transfer. Commands return an opaque transfer
handle and byte length; the server reads bounded chunks and drains records, user-input
facts, and events in their original order. A single large record may span chunks. Uploads
for retained re-projection use the same framing in the other direction. Transfer handles
are scoped to their session, expire, and are released after use; incomplete transfers
never publish a replacement projection. The transport frame limit is not a history-size
limit.

It is one pass and nothing else. It does not start a follower, does not change what
`adopt` does, and does not touch the session's `fidelity` — a record's resolution is a
property of the source that wrote it, so re-reading the same store cannot raise or lower
it. `watch` remains the way to follow a session continuously.

**A re-read is bounded by what the journal already holds, not by what the vendor emits.**
The adapter has no cursor to resume from: `Adapter::adopt` restarts at the beginning of
the store, so the second pass re-emits every record of the first. Suppression is therefore
the server's, and it happens at ingest.

**Raw identity and projected-event identity answer different questions.** All three
adapters emit native records, keyed by `(session, source, version)`. Projected events
still use content identity when the source supplies no durable per-event identifier or
the live source version is process-local. A re-read digests each incoming event over the
fields that decide what a reader sees, and skips one whose digest a previous pass already
journaled for that session. The digest is per session and covers both tiers, because a
turn's events live in whichever tier consolidation last left them.

**Two events may legitimately be identical**, and this is the cost the design accepts
knowingly: a vendor that wrote the same text twice in one session has a second record that
is indistinguishable from a replay of the first. Suppressing it loses one repeated line;
admitting it duplicates the entire transcript on every click. The transcript is the record
being protected, so the digest wins, and the count of what a pass suppressed is reported
rather than silent.

**A turn's opening bracket is the one event content identity cannot answer for, so the
stream decides it.** A `turn`/`started` is ephemeral and carries no `stream_id`, so it
reaches the journal and no further: the history fold has nothing to key a block on, and
the fold then deletes the journal rows it read. Once a turn is consolidated its opener is
in neither tier, and no digest can recognize it on a later pass.

The two obvious answers fail in opposite directions. Re-ingesting it grows the journal by
a dead row per click, invisibly, because every reader-facing query filters to stable rows.
Suppressing it whenever no container is open swallows a genuinely appended turn's bracket
too — which loses no content, since any event arriving with no container open opens one,
so no turn count can observe it. What it loses is the *start*: the container opens at the
next event's stamp, so the turn silently begins late and any duration read from it
measures the wrong interval.

So an opener is **held aside and resolved by what follows it**: written once a following
event proves unheld, and dropped when everything after it is already held. A dropped
opener is reported as examined, and as already held only where the pass can actually
establish that — which is what keeps the endpoint's counts from summing to a number
nobody measured (`docs/api/sessions.md` § `POST /api/v1/sessions/{id}/reread`).

**A pass that finds nothing new is a success, and says so.** "Ingested nothing" and "could
not read the store" are the same shape in a journal and opposite facts, so a re-read
reports what it examined, what it added, and what it recognized as already held. A
re-read whose store is gone is a failure with its own status, never a quiet no-op.

## Health is per environment

`/api/v1/health` reports each environment separately, because "the agent will not start"
has a different cause and a different fix per machine: a missing CLI locally, an
unreachable orchestrator or exhausted node for a sandbox, a dead connection for a laptop
that went to sleep.

**An unreachable envagent is one fact about the run, not one per attempt.** Report it once
at `error` and at `debug` thereafter, in the client that holds the connection rather than
in the handlers that poll it — a sidebar and a picker both polling would otherwise
reproduce the flood one layer up.

## Native persistence faults

A failed native write is a synchronization failure, not evidence the vendor agent
stopped. The server journals a synchronization notice, Nacks the unaccepted producer
prefix, and keeps the connection and turn alive. A successful retry records recovery;
repeated failures of the same blocked pass do not produce one notice per record.
Protocol authentication, epoch, producer, and payload-integrity violations still refuse
the connection.

The envagent stops delivery at the first Nack. It does not send later numbered records
that can only create more gaps. One bounded replay task retries the retained prefix
alongside command handling, including after restart. Producer sequence assignment,
receipt validation, and accepted-through continuity remain unchanged. This is one
producer channel; the design does not claim collision-free sharding or independent
session delivery channels.
