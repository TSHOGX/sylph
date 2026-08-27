# Data Architecture

Who owns a fact, where it lives, and which direction it travels.

This document governs the authority model. `docs/storage.md` owns the three-tier
mechanics of how bytes are kept and pruned; `docs/environments.md` owns where work
runs; `docs/session-protocol.md` owns what an adapter may send. This file owns the
question the others defer to — **which copy is the truth** — and the identity rule
every one of them keys on.

Claims here are marked. *Measured* means it was observed against a real install on
the stated date and the evidence is named. *Decision* means it is a design choice this
document makes, and a reader disagreeing with it is disagreeing with a judgement rather
than with a fact.

## Two classes of session, and the boundary already exists

Every question in this document resolves differently for the two, so the split comes
first.

A **Sylph-born** session is one Sylph spawned: it holds the adapter's stdio, receives
the full `sylph.agent.v3` stream, journals every delta, and can drive the session
because it owns the only channel the agent listens on. A Sylph-born session in a remote
environment is still Sylph-born — the adapter's stdio is carried over the environment
transport, so the class is about who holds the channel, not about which machine the
process runs on.

An **adopted** session is one somebody else started — a human in a terminal, a cron
job, another tool. Sylph learns of it by reading the agent's native store. What Sylph
can see is whatever the vendor chose to write there, and what Sylph can *do* is
whatever control surface that vendor happens to expose to a second party.

*Decision:* this boundary is not imposed by Sylph and must not be smoothed over. It is
the difference between holding a process's stdin and not holding it, which no amount of
API design converts into the other. Every surface that reports capability reports which
class it is looking at.

## Identity is `(environment, agent, native_id)`

*Decision.* A session's durable key is the triple: which environment it was born in,
which agent runs it, and the id that agent's own store uses. Sylph mints its UUID on
first sighting and that UUID is thereafter the only id any API exposes.

Three candidates were rejected, each for a reason that would have surfaced late:

- **A content fingerprint.** trace-db's staleness signal is
  `sha256(file):bytes:mtime_ns` and is `NOT NULL` (*measured*, `parsers/mod.rs:116`).
  `opencode` writes no per-session file at all — a single SQLite store with a live WAL
  — so it cannot produce one, and a key one of five agents cannot compute is not a key.
- **The archive id.** A session is observable the moment its native store exists, which
  is before anything has ingested it. Keying on the archive would make identity depend
  on a projection's progress.
- **Anything containing a host or a path.** See § Provenance is not identity.

The triple is available at first sight for all five agents — the native id is the file
name or the store's own row id — and it is stable *because sessions do not migrate
between environments*. That constraint is what makes the simple key sufficient; it is
stated as an invariant below rather than left implicit.

## Sessions do not migrate between environments

*Decision, and the load-bearing one.* A session belongs to the environment it was born
in for its whole life. Sylph does not offer moving one, resuming one elsewhere, or
reconciling two copies of it.

This is a deliberate reduction in scope and it pays for most of the simplicity in this
document. A conversation in one environment and a conversation in another are different
conversations, so:

- No two-sided freshness comparison is ever needed, because no two environments hold
  candidate copies of one session.
- `cwd` is scoped by environment, which answers what used to be an open question here.
  Two machines both having `/home/user/app` is not an ambiguity, because the directory
  is only ever interpreted alongside the environment that named it.
- Upload can never conflict with what the server already holds. It either brings a
  session the server has not seen, or a longer prefix of one it has.

An earlier version of this document defended at length against a hazard where one host
compares its own files against another host's fingerprints and concludes "unchanged".
**That hazard cannot occur under this invariant** and the defence is deleted rather than
kept as insurance. Keeping it would have implied the shape it guards against is
reachable, which is how a compatibility branch outlives the defect it papers over.

## One authority, and it is the Sylph server

The Sylph server holds a **private archive** under `~/.sylph`. It is the single
authoritative record of session history. Everything else — an agent's native store, a
remote environment's disk, a client's cache — is either a source it ingests from or a
projection it serves.

This inverts what the codebase did before. TraceDB ingested each agent's native store on
the host, which made the native store the authority and the archive a deterministic
projection. That holds only while every native store is local and reachable.
Environments break the premise: a sandbox's `~/.claude` dies with the sandbox, so the
archive acquires a hole exactly where the isolated work happened, and every symptom of
that is an absence.

The server is the one component present for all work regardless of environment, because
the adapter protocol carries the events and the server is on the other end of it. So the
server's normalized event stream is the authority for Sylph-born work, and the archive is
derived from a stream rather than from a filesystem the server may not reach.

### The archive is private, and Sylph owns its location

The archive lives inside `~/.sylph`, not at TraceDB's default path. No external
`trace-db` daemon is assumed to exist, no user installs one, and on a machine that
happens to have one, Sylph neither reads nor writes its archive.

*Decision:* trace-db is **kept and embedded as a library**, not abandoned and not
subprocessed. The reasoning is that its value is concentrated in one component that
would otherwise have to be rewritten: `parsers/` is 2412 lines covering five vendors'
native formats, and those formats are the part a vendor changes without warning. The
daemon, the gRPC service, the CLI, and the JSON protocol carry none of that value and
are deleted. *Measured:* embedding drops the dependency graph from 148 crates to 72 at
`--no-default-features`, and in-process reads are 100× to 6000× cheaper than the
subprocess path they replace — `coverage()` 5.2 µs against 11.5 ms, a whole-archive scan
7.9 ms against 52.9 s.

The rule that survives embedding is the one that matters: **Sylph code goes through the
crate's API and never touches its tables.** Embedding removes the JSON round trip that
used to make that boundary physical, which makes it easier to break and no less binding.

A private archive with one writer also removes coordination that a shared one requires:
a busy timeout, per-agent commit transactions, and an exclusive lock for reindexing,
whose failure modes are all silent interleavings. *Measured:* with two ingests running,
search went 186 → 273 ms and `open_read_only` 0.7 → 3.8 ms, so the contention is real
rather than theoretical.

### Sylph's normalized stream does not enter the archive

*Decision, with three structural reasons.* Sylph's own event stream is never pushed
through ingest. Sylph-born history lives in `journal.db` and `history.db`; nothing
merges the two stores.

**This is narrower than it sounds, and the difference is load-bearing.** A Sylph-born
session usually *does* appear in the archive — not because Sylph put it there, but
because the vendor CLI wrote its own native file and the ordinary parser path ingested
it, with a real fingerprint and no synthesis. That is exactly what retention's
watermark reads (`storage/retention.rs`, keyed on `archive_id`), so the rule below must
not be read as "the archive knows nothing about Sylph's sessions" — it would delete
retention's only authority.

The rule is about *direction*: the archive is fed by parsing native sources, never by
Sylph handing it normalized events. The reasons:

- `Agent` is a closed five-variant enum whose every variant maps to a filesystem root
  (*measured*, `facade.rs:2074`). Sylph is not one of them, and adding a variant with no
  root inverts what the type means.
- `sessions.fingerprint` is `NOT NULL` and a Sylph turn has no native file, so the value
  would have to be synthesized — and a synthesized fingerprint is a lie that retention
  reads as provenance.
- `upsert_many` sets `event.span_id` **unconditionally** to `call:<call_id>` (*measured*,
  `model.rs:538`). Pushing Sylph's events through ingest would silently overwrite the
  trajectory identity `span` exists to carry.

The accepted consequence is stated plainly because it is a real gap: **full-text search
covers only what the archive parsed.** A Sylph-born session in an environment whose
native store the server cannot reach — a sandbox, an SSH host that has gone away — is
searchable nowhere, because its bytes never reached a parser. Pretending one index
covers everything would be the more expensive error, since the failure is silently
missing results rather than an error. § Open questions carries this as unresolved.

## Data travels one way: upload

**"Sync" is not a word this system uses.** The operation is **upload**: an environment
sends its data *to* the authoritative server. The server never pushes history outward,
and never writes session data anywhere outside `~/.sylph`. In particular there is no
"sync into another directory on the machine" — the archive is not a folder a user
manages.

There is no bidirectional reconciliation, no last-writer-wins, no vector clock, and no
merge. Those are the machinery of multi-master replication, and every one of them exists
to answer "which of two conflicting writes is correct" — which the no-migration
invariant means nobody ever asks.

Upload has **two paths**, and they differ by session class rather than by transport:

- A **Sylph-born** session arrives on the server as a natural consequence of running:
  the adapter's stream already terminates at the server, so nothing separate has to be
  uploaded and there is no moment at which the data is only in one place.
- An **adopted** session requires an **explicit upload**, because Sylph did not create
  it and must not assume permission to publish somebody's local transcripts. This is the
  user-facing feature: point Sylph at local sessions, upload them, and read them from any
  client.

*Decision:* adopted upload stays explicit rather than becoming an automatic scan. A
background sweep of every native store would publish sessions the user never associated
with Sylph, and "everything on this disk is now on the server" is not a default a tool
gets to pick on someone's behalf.

What travels is **native source bytes**, not projected rows. An environment uploads the
vendor's own session file and the server projects it with the same code that projects a
local one. Projection is the part most likely to change, and it is the part a remote
environment should never own: upload bytes and the server re-derives, upload rows and
every environment must be upgraded in lockstep with the schema. It is also why there is
exactly one parser version in the system at any time.

`opencode` is the exception that proves the rule is per-agent. It has no per-session
file, only a 541 MB SQLite store with a live WAL, so "upload the bytes" has no
straightforward referent and the fingerprint the ingest path requires cannot be produced
(*measured*; an attempt reached 4.6 GB still running at ~20 minutes and starved every
other agent's pass). **Its read strategy is unresolved and must not be guessed.**
Ingestion is one strategy per agent, not one scan.

### Provenance is not identity

An uploaded record carries where it came from, and that marker must stay out of every
key.

`host_id` and `environment` are **provenance and scope**: shown to a user, used for
diagnosis, and — for `environment` — part of the identity triple because it names the
session's birthplace, which does not change. What neither may become is an input to a
freshness comparison, because there is never anything to compare.

Content addressing needs the same discipline in the other direction. Objects are keyed by
hash of content, which is host-independent by construction, so the same dependency source
uploaded from three machines is stored once. Adding a host column to the object table
destroys exactly that property. **The object store stays host-blind.**

## AFK is two capabilities, not one

The motivating scenario: work with an agent in a terminal, walk away, then see that
session's state from a phone and continue it. That is the requirement this architecture
exists to serve, and it decomposes into two capabilities with very different promise
strength. *Decision: they are surfaced separately, because a single "AFK works" boolean
would be a promise Sylph cannot keep for every agent.*

**Observation is near-universal.** *Measured 2026-08-27*, four of five agents flush their
native store mid-turn, so a session's state is externally readable while it is still
running: `claude` at tool-call and content-block boundaries (record age at flush reached
9.4 s), `codex` at item completion, `pi` on every `message_end`, `opencode` into SQLite
rows. `gemini`'s mechanism is **unlocated** — a genuine unknown, not a measured absence.

Two `pi` hazards are recorded because a reader would otherwise design around the wrong
shape: `_rewriteFile()` truncate-rewrites with mode `"w"` and no lock, so a reader can
observe an empty or half-written file; and `_persist` buffers until the first assistant
message, so the file does not exist at all early in a turn. "Only at `message_end`" is
also true-but-trap — a turn emits one per assistant message *and* per tool result, so a
5-tool turn was ~11 appends. That is message granularity, not turn granularity.

**Continuation is per-agent, and three of five have a channel.** *Measured:*

| Agent | Second-party control surface |
| --- | --- |
| `codex` | Shared local app-server daemon; `remote-control start\|stop\|pair`, `queue --thread <uuid> --message <text>`, `--remote ws://\|wss://\|unix://` |
| `opencode` | `serve` + `attach <url>` + `acp` |
| `claude` | Per-process authenticated UDS; delivers messages, **cannot drive** |
| `pi` | None. `RpcClientOptions` is `{cliPath, cwd, env, provider, model, args}` — no address field, no `createServer`, no `.sock` |
| `gemini` | Start-time flags only |

`claude`'s socket is worth stating precisely because an earlier version of this document
got it wrong. The `[uds-messaging]` module accepts eight frame types and all eight are
ownership coordination; `set_model`, `set_permission_mode`, `stop_task`, and
`subtype:"interrupt"` live ~2 MB away in the SDK/WebSocket control table, which is a
different channel. So a message can be *delivered* to a running `claude`, and inbound
`user` frames are **gated on the sitting human's approval** — the binary carries "held for
the recipient user's approval", declined, and expired strings. Whether that gate applies
per-frame or once per peer is unsettled, and it is the difference between useful remote
continuation and a feature that fails exactly when nobody is at the keyboard.

*Decision:* post-session continuation — resume after the process has exited — is a
separate and much easier question, and it works for **every** agent, because resume is a
first-class vendor feature. So AFK degrades in a defined order: observe always, resume
always, live continuation where the vendor allows a second party in.

## One link state, two typed capability facets

*Decision.* An adopted session is **not** a third link state. The link stays one state
and grows two typed facets — what can be *observed* and what can be *driven* — because
the vendor table above does not sort into three buckets: terminal-born `pi` is
observable and unreachable, `codex` is observable and controllable, `claude` is
observable and semi-reachable behind a human gate. A third enum variant would have to
pick one of those to represent and would misreport the other two.

What has to change is the *reasoning*, not just the shape. `backend/src/links/mod.rs:61`
documents an empty capability surface as meaning **no process exists**, which is exactly
inverted for a terminal-born session: the process is alive and it is Sylph that cannot
reach it. `LinkFailureCode` reinforces the same assumption — both its values,
`NoAdapter` and `HandshakeFailed`, describe *our own spawn* failing, so neither can
express "somebody else's process, running fine, not ours to drive". And
`LinkState::detached()` zeroes `supports` and `capabilities` together, which conflates
"we know of no capabilities" with "there are none".

`docs/session-protocol.md` carries the same inversion in two more places: § Capability
facets defines an empty surface the same way, and its turn semantics define a turn on a
detached session as spawning to resume — right for a session Sylph owns, wrong for one
that is already running under someone else.

## Observation is multi-party; control is single-owner

Two questions get conflated because both read as "can two things touch one session".

**Observation may be concurrent and multi-party.** Several clients, a terminal and a
browser together, more than one user — all reading one session is safe, because reading
creates no branch. This is what makes the AFK case tractable: the terminal keeps working
while the phone watches.

**Control must be single-owner, with explicit handoff.** *Measured:* none of the agent
CLIs takes a lock. Two writers to one session produce sibling branches off a single
parent pointer, with zero errors and zero corruption at the storage layer — the damage
is entirely semantic, which is why nothing catches it. A system that permitted
co-driving would be manufacturing a conflict its dependencies cannot report.

So taking control of an adopted session is an explicit act with an owner, and the
handoff is visible to whoever had it. `claude`'s approval gate is a vendor
implementation of exactly this idea, which is evidence the shape is right rather than an
obstacle.

## Fidelity is a property of the record, and must be visible

Not every source supports the same resolution, and collapsing that into one boolean is
dishonest in a way a user cannot detect.

A live protocol stream carries every delta as it arrives. A vendor native store records
less, and how much less differs per agent — the granularities are in § AFK above. What
none of them records is intra-message delta timing, and no amount of care recovers a
timing that was never written.

So a record carries an explicit fidelity — **stream**, **blocks**, or **messages** — and
the UI says which one it is showing. A waterfall rendered from message-fidelity data with
no indication would present equal-length bars as measurements, which is the failure a
previous fixture bug already demonstrated: when durations come from the wrong field, every
bar is the same length and the whole suite passes.

This is also the second independent reason `history.db` is not redundant with the archive.
The archive stores each event's final text; `history.db` stores the text *plus* where each
delta ended and when it arrived (*measured*, `fold.rs:18-26`). One is a transcript, the
other is a recording, and only the second can answer a timing question.

## Retention, and when breaking changes stop being free

`AGENTS.md` § Compatibility applies in full today: this is development-stage, no
irreplaceable user data exists, multi-machine and sandbox data is disposable, and a
schema change that can be satisfied by re-deriving from native stores costs nothing.
Delete-and-re-ingest is a legitimate recovery.

*Decision:* the signal that ends this is neither a date nor a version. It is whether the
deployment holds **irreplaceable installed state** — an uploaded row whose native source
is a sandbox that no longer exists, or a remote host now unreachable. At that point the
archive holds the only copy of that session and the free-breaking-changes rule stops
applying to *uploaded session data specifically*, while continuing to apply to schemas,
wire formats, and internal APIs.

Retention's watermark discipline is what keeps that tractable: it deletes only what a
downstream authority has confirmed, so a lagging consumer causes growth rather than loss,
and growth is visible and fixable. `docs/storage.md` remains authoritative for the
mechanics. What this document changes is the set of authorities — with the server holding
the truth, "confirmed by TraceDB" is no longer the question.

## The daemon form, and what it gates

Sylph is expected to take a daemon-like shape: a user installs a Sylph server and reaches
it from one or more clients, or directly over the web, possibly as several users.

Nothing in the authority model requires that, and the model is what makes it possible —
one authoritative store with many readers is the shape a server wants, and it is why
replay belongs to the server rather than to each frontend.

**Multi-user access is an explicit future gate, not solved here.** Today the backend binds
loopback by default and authenticates with a `SameSite=Strict` cookie whose token rotates
on restart, deliberately invalidating open browser sessions. That is adequate for a
single-user local server and is not an authentication system: it has no notion of *who* a
user is, only that a request came from a browser that completed a local handshake.

Any non-loopback deployment — which multi-user access requires — is a **separate
authentication and threat-model change**, and must not be reached by relaxing the bind
address. Unanswered and required first: user identity and per-user session ownership,
whether one user may observe another's sessions, transport security, and what a
compromised client can reach given that a session's `workdir` is the working directory of
a spawned process. That last is the sharpest — `SYLPH_BROWSE_ROOTS` bounds where an agent
may run, so on a multi-user server it is an authorization boundary rather than a
convenience, and it is currently per-install rather than per-user.

## Open questions

Recorded because they are load-bearing and unresolved, not as a task list.

**`claude`'s approval gate scope.** Per-frame or once per peer. This decides whether
live continuation of a terminal `claude` session is a feature or a dead end, and it is
the highest-value unknown in this document.

**`gemini`'s mid-turn write behaviour.** Unlocated rather than absent. Until measured,
`gemini` must be treated as observation-unknown rather than observation-capable.

**`opencode`'s ingestion strategy.** No per-session file, no computable fingerprint, and
a naive read is catastrophically expensive. Must be decided before any ingestion timer
ships.

**Search over what the archive never parsed.** The archive indexes whatever a parser
read; the journal holds Sylph's normalized events with no index. A Sylph-born session
whose native store reached the server is covered by coincidence — the vendor's file was
ingested — and one whose environment is gone is covered nowhere. Either the journal
grows an index or the two are searched separately with the UI saying which. Neither has
been chosen, and "usually covered" is not a contract.

**`history.db` at archive scale.** The journal was explicitly not a second transcript
archive, and making it the authority for Sylph-born work makes it one. The three-tier
split exists because a long read transaction blocks WAL checkpointing, and that pressure
grows with the volume this invites.



