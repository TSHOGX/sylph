# Data Architecture

Who owns a fact, where it lives, and which direction it travels.

This document governs the authority model. `docs/storage.md` owns the three-tier
mechanics of how bytes are kept and pruned; `docs/environments.md` owns where work
runs; this file owns the question they both defer to — **which copy is the truth.**

## One authority, and it is the Sylph server

The Sylph server holds a **private archive** under `~/.sylph`. It is the single
authoritative record of session history. Everything else — an agent's native store,
a remote environment's disk, a client's cache — is either a source it ingests from
or a projection it serves.

This inverts what the codebase did before. TraceDB ingested each agent's native
store on the host, which made the native store the authority and the archive a
deterministic projection of it. That is a coherent design, and it holds only while
every native store is local and reachable. Environments break the premise: a
sandbox's `~/.claude` dies with the sandbox, so the archive acquires a hole exactly
where the isolated work happened, and nothing reports it missing.

The server is the one component present for all work regardless of environment,
because the adapter protocol is what carries events and the server is on the other
end of it. So the server's normalized event stream is the authority, and the archive
is derived from it rather than from a filesystem the server may not be able to reach.

### The archive is private, and its location is Sylph's

The archive lives inside `~/.sylph`, not at TraceDB's default path. Sylph owns the
database calls; no external `trace-db` daemon is assumed to exist, and on a machine
that has one, Sylph does not read or write its archive.

This is a real simplification rather than a preference about paths. A shared archive
means two writers, which means a busy timeout, per-agent commit transactions, and an
exclusive lock for reindexing — coordination whose failure modes are all silent
interleavings. A private archive with one writer has none of them.

## Data travels one way: upload

**"Sync" is not a word this system uses.** The operation is **upload**, and it has a
direction: a backend environment sends its data *to* the authoritative server. The
server never pushes history outward, and it never writes session data anywhere
outside `~/.sylph`.

There is no bidirectional reconciliation, no last-writer-wins, no vector clock, and
no merge. Those are the machinery of multi-master replication, and every one of them
exists to answer "which of two conflicting writes is correct" — a question a
single-writer upload model never asks.

What travels is **native source bytes**, not projected rows. A remote environment
uploads the vendor's own session file; the server projects it locally with the same
code path that projects a local one. This matters for a reason worth stating plainly:
projection is the part most likely to change, and it is the part a remote environment
should never be responsible for. Upload bytes and the server can re-derive; upload
rows and every environment must be upgraded in lockstep with the schema.

### What this eliminates

The multi-host hazard this model removes is specific, so it is worth naming. A
candidate-state check that has no host predicate, keyed on an absolute local path,
lets machine A compare A's files against B's fingerprints and conclude "unchanged" —
silently skipping ingestion of data that exists only on B. Every symptom of that is
an absence, so nothing reports it.

Single-writer upload makes the bug unconstructible rather than fixed: only the server
projects, so there is never a comparison between one host's files and another host's
fingerprints. `host_id` therefore exists as a **provenance marker** — which machine
this came from, for display and diagnosis — and never as part of a primary key or a
freshness comparison. Anything that makes `host_id` load-bearing for identity has
reintroduced the distributed-key problem.

Content addressing needs the same discipline in the other direction. Objects are
keyed by hash of content, which is host-independent by construction, so the same
dependency source uploaded from three machines is stored once. Adding a host column
to the object table would destroy exactly that property. The object store must stay
host-blind.

## Retention stops being free once data is uploaded

TraceDB's "delete the archive and re-ingest" recovery is safe only while the native
source still exists. An uploaded row's source may be a sandbox that is already gone,
in which case the archive holds the only copy.

So the policy in `AGENTS.md` § Compatibility — breaking changes are free, design for
the end state — continues to apply to schemas, wire formats, and internal APIs, and
**stops applying to uploaded session data**. The signal is not a date and not a
version: it is whether irreplaceable installed state exists. A schema change that can
be satisfied by re-deriving from local native stores is free. The same change is not
free once the archive contains rows whose source is unreachable, and the difference is
a property of the deployment rather than of the change.

Retention's watermark discipline is what makes this tractable: it deletes only what a
downstream authority has confirmed, so a lagging consumer causes growth rather than
loss. Growth is visible and fixable. That reasoning is unchanged by this document and
`docs/storage.md` remains authoritative for it. What changes is the set of authorities:
with the server as the source of truth, "confirmed by TraceDB" is no longer the
question, and the authority that matters is whichever downstream copy — if any — the
operator requires before local bytes may go.

## Fidelity is a property of the record, and must be visible

Not every source supports the same resolution, and collapsing that into one boolean
is dishonest in a way a user cannot detect.

A live protocol stream carries every delta as it arrives. A vendor native store does
not: `claude` writes at content-block completion, and `pi` writes only at
`message_end`, which means no intra-message timing exists in its store at all. A
session reconstructed from `pi`'s store is genuinely a transcript rather than a
stream, and no amount of care recovers timings that were never recorded.

So a record carries an explicit fidelity — stream, blocks, or transcript — and the UI
says which one it is showing. A waterfall rendered from transcript-fidelity data with
no indication would present equal-length bars as measurements, which is the failure
mode a previous fixture bug already demonstrated: when durations come from the wrong
field, every bar is the same length and the whole suite passes.

## Observation is multi-party; control is single-owner

Two different questions get conflated because both are "can two things touch one
session at once".

**Observation may be concurrent and multi-party.** Multiple clients, a terminal and a
browser together, several users — all reading one session is safe, because reading
creates no branch.

**Control must be single-owner, with explicit handoff.** This is not a limit of the
current implementation but a measured property of the vendors: none of the agent CLIs
takes a lock. Two writers to one session produce sibling branches off a single parent
pointer, with zero errors and zero corruption at the storage layer — the damage is
entirely semantic, which is why nothing catches it. A system that permitted
co-driving would be building a conflict its dependencies cannot report.

Post-session interaction is a separate and easier question, and it works for every
agent: it is resume, which the vendors already support.

## The daemon form, and what it gates

Sylph is expected to take a daemon-like shape: a user installs a Sylph server, and
reaches it from one or more clients, or directly over the web, possibly as several
users.

Nothing in this document's authority model requires that, and the model is what makes
it possible — one authoritative store with many readers is the shape a server wants,
and it is the reason replay belongs to the server rather than to each frontend.

**Multi-user access is an explicit future gate, not solved here.** Today the backend
binds loopback by default and authenticates with a `SameSite=Strict` cookie whose
token rotates on restart, deliberately invalidating open browser sessions. That is
adequate for a single-user local server and is not an authentication system: it has no
notion of *who* a user is, only that a request came from a browser that completed a
local handshake.

Any non-loopback deployment — which multi-user access requires — is a **separate
authentication and threat-model change**, and must not be reached by relaxing the bind
address. Concretely, the following are unanswered and must be answered before a
non-loopback bind is offered: user identity and per-user session ownership, whether one
user may observe another's sessions, transport security, and what a compromised client
can reach given that a session's `workdir` is the working directory of a spawned
process. That last one is the sharpest: `SYLPH_BROWSE_ROOTS` bounds where an agent may
run, so on a multi-user server it is an authorization boundary rather than a
convenience, and it is currently per-install rather than per-user.

## Open questions

Recorded because they are load-bearing and unresolved, not as a task list.

**Search ownership.** With the server's event log as the authority, full-text search
over history could belong to the server's own store or remain TraceDB's. The archive
currently owns the tokenizer and the index; the journal owns the events. Whichever way
this goes, one of the two stops holding an index it currently maintains.

**`history.db` at archive scale.** The journal was explicitly not a second transcript
archive, and making it the authority means it becomes one. The three-tier split exists
because a long read transaction blocks WAL checkpointing, and that pressure grows with
the volume this change invites.

**`cwd` across machines.** The sidebar groups sessions by working directory, and the
archive's directory filter is a substring match rather than a path match. Neither
survives contact with two machines that both have `/home/user/app`, and a path is not
an identity once it can name a directory on a host that is not this one.
