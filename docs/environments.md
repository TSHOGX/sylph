# Environments

For trace transport, [unified-trace-storage.md](unified-trace-storage.md#capture-contract)
replaces normalized event delivery with durable source capture and pure server mapping.
The topology, pairing, grants, framing bounds and envagent-owned filesystem policies in
this document remain authoritative. Existing message/trait spellings below are current
implementation context where the target capture contract replaces them.

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

One connection per envagent, initiated by the envagent, carries framed messages in both
directions. Capture upload, command results, operational session state, and file operations
share the connection but have separate identities and receipt rules.

The envagent dials out, so a laptop behind NAT, an SSH host on a private network, and a
sandbox that auto-pauses use the same runtime. Plaintext remains loopback-only. A remote
listener requires configured rustls credentials, and the envagent validates the server
certificate before the protocol handshake.

### Message classes

| Message | Direction | Purpose |
| --- | --- | --- |
| `hello` / `welcome` | both | Version, installation, connection epoch, agent inventory and machine health |
| `command` / `command_result` | both | Immutable request identity, retry policy and operational outcome |
| `capture_upload` | env → server | Begin a declared capture, transfer bounded payload chunks, and commit it |
| `capture_reply` | server → env | Upload ticket, chunk receipt, terminal retained/deletion receipt, or explicit rejection |
| `session_control_state` | env → server | Current command lease, actual native turn identity, and active state |
| `capability_snapshot` / `request` | env → server | Current capabilities and requests awaiting human input |
| `heartbeat` / `agents` / `diagnostic` | env → server | Current connection and machine state |

[The wire schema](sylph.env.v1.schema.json) describes these envelopes. Known envelopes reject
unknown fields; unknown message types cannot enter capture admission. There is no semantic
event batch, native-transfer projection path, or acknowledgement cursor in the handshake.

The producer receipt identity and native source position are independent. The envagent
commits exact bytes and a fixed ingestion authorization epoch before parsing or routing any
vendor frame. The server admits those captures and maps trace semantics from retained
evidence. [unified-trace-storage.md](unified-trace-storage.md#capture-contract) owns this
contract, including pre-identity inventory capture and source generation changes.

File commands carry the session's recorded `workdir`. The envagent canonicalizes that root
and confines operations to it; a process-local link or vendor-store scan is not required.
A source locator remains acquisition metadata and is never opened by the server as a
fallback for absent retained payloads.

**History work cannot consume interactive command capacity.** Native enumeration,
adoption, following, and re-reading share a bounded pool separate from file access and
session control. Saturation returns `COMMAND_OVERLOADED` before execution, with no
hidden queue whose wait can exceed the caller's deadline. Independent commands run
concurrently; mutations for one session or prepared draft retain wire arrival order.
Agent inventory probes run independently of command reception. A disconnected epoch
cancels unfinished command tasks and releases its acknowledgement waiters; it never
replays an ambiguous at-most-once operation automatically.

Stateless reads use fresh replayable request identities and connection-local waiters.
File listings and reads, catalogue enumeration, capability and health probes do not
persist command receipts or response bodies. Their deadline
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

The envagent commits raw adapter output before decoding it and drains the spool through
bounded uploads. Operational replies and lifecycle state are independent of semantic
projection. A native turn id cannot replace a command lease: they name different facts.
A late completion for an earlier native turn cannot settle its successor.

## Spooling, and why the record survives a dropped connection

The shared WAL-mode SQLite capture registry commits source identity, generation, lexical
position, producer sequence, authorization epoch and payload bytes together. Partial JSONL
tails remain durable without advancing the complete cursor. Snapshot reads are bounded
before allocation and verified again before a revision is committed.

Only a terminal retained or authorized-deletion receipt releases queued bytes. Reconnect
replays the same observations and authorization epochs; it does not reserialize vendor
payloads, mint another occurrence, or infer acceptance from a command result. The server
archive remains authoritative after admission.

At-most-once commands have a separate operational attempt ledger. An attempt is reserved
before the vendor operation; a duplicate returns its retained outcome. An interrupted
attempt without a durable result stays unknown and is not executed again. Fresh replayable
read commands do not accumulate bodies in this ledger.

Vendor process lifecycle captures distinguish spawn, measured exit, and capture failure.
The process transport retains stdin before attempting the write, waits for the write
receipt, and captures stdout/stderr before routing JSON. A lost reply stays unknown;
only an explicit vendor refusal or local validation can prove rejection. Oversized
vendor lines stop at the configured frame bound and report an acquisition gap.

Native SQLite metadata is acquired through a consistent read transaction, including WAL
commits. Unknown columns retain SQLite storage classes and exact binary/text bytes. A
selected Codex rollout records its captured source generation, complete lexical record
count, and pending tail size; directory order and modification time never select it.

Referenced output files are watched after their parent capture is durable. Each observed
sidecar revision produces a separate attachment observation with an exact parent JSON
Pointer and retained payload reference. The original capture remains immutable. Missing,
unreadable, and later available sidecars are distinct observations.

### Overflow is a fact in the record

Overflow bounds and reports capture loss; it cannot manufacture a vendor outcome.
The envagent applies spool backpressure and the actual controlled-process stop policy,
retaining diagnostic capacity and a named source gap. It cannot claim to stop a native
agent it only observes. Parent loss does not close independent children/resources.

[unified-trace-storage.md](unified-trace-storage.md#admission-and-acknowledgments)
owns overflow, receipt and recovery behavior. The spool's accounting and enqueue/sequence
allocation remain transactional, so growing backlog does not turn each enqueue into a
full-table scan.

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
is what the durable capture spool and terminal receipts are for.

**`Connect` replays a bounded tail and is explicitly not durable**, so it is a reconnect
convenience rather than a record.

Khora models no caller, account, or work unit by design, and Sylph must not push those
into it. A sandbox is addressed by id, and the mapping from session to sandbox lives in
the server's store.

## Watching and adopting a native session

Adoption captures the selected native session's current inventory. Following keeps
acquiring that inventory as it changes. Both use the same source registry and pure trace
mappers, preserve vendor fidelity, and are explicit actions regardless of who started the
native session (`docs/architecture.md` § Data travels one way).

A following link observes files independently of control. It cannot interrupt an external
process merely because its native transcript is readable. A held vendor process has its
own operational control capability and lease.

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

Reread captures another verified pass through the native source inventory. It shares
source reservations with active followers and uses immutable SourcePosition plus the
current admission epoch to recognize already retained occurrences. Repeated identical
messages at distinct positions survive; projected-content hashes never suppress them.

Capture, incomplete tails, rewrite generations, sidecars and chunk budgets follow
[unified-trace-storage.md](unified-trace-storage.md#source-identity-and-generations).
A pass reports examined occurrences/bytes, new versus already retained occurrences,
source gaps and its admitted cut. Zero new content is distinct from a failed scan.

Reread does not start a vendor execution or infer current selected history from mtime.
Re-projection is a separate pure server job over admitted evidence, so it does not need
a reachable envagent or a present native source. Atomic generation publication follows
[unified-trace-storage.md](unified-trace-storage.md#generation-publication).

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

A failed acquisition pass records an unreadable or missing source observation at the last
committed cursor. It cannot claim that the vendor stopped. Complete lexical bytes remain
queued until their terminal receipt, and an admission failure leaves the same observation
available for replay. A successful later pass records the changed source state.

Delivery stops at the first unaccepted capture so later records cannot conceal a producer
gap. Bounded replay runs alongside control and file commands. Invalid authorization,
connection epoch or payload integrity is refused before receipt; a retry cannot rewrite
the retained observation or its fixed ingestion epoch.
