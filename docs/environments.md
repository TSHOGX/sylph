# Environments

Where a session's work actually happens. One authoritative record, several
places the agent can run.

## The seam

Today the supervisor calls `Command::new(path).current_dir(workdir)`. Local
fork/exec is not behind an interface; it *is* the interface, spread across
process spawning, file staging, path containment, and the file-browsing API.

An environment is the answer to four questions, and nothing else:

```rust
#[async_trait]
pub trait Environment: Send + Sync {
    fn descriptor(&self) -> EnvironmentDescriptor;

    /// Start a process. The handle owns its streams and its signal channel.
    async fn spawn(&self, spec: ProcessSpec) -> Result<Box<dyn ProcessHandle>>;

    /// Files, for staging attachments and for the Artifacts pane.
    fn files(&self) -> &dyn FileSystem;

    /// Whether the environment is reachable right now, and why not.
    async fn probe(&self) -> Health;
}
```

`ProcessHandle` is the whole runtime contract:

```rust
#[async_trait]
pub trait ProcessHandle: Send {
    fn stdin(&mut self) -> Option<Box<dyn AsyncWrite + Send + Unpin>>;
    fn stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;
    fn stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>>;
    async fn signal(&self, sig: Signal) -> Result<()>;
    async fn wait(&mut self) -> Result<ExitStatus>;
}
```

That shape is chosen so the adapter protocol is unchanged by where it runs: it is
NDJSON over a duplex byte stream, which every candidate environment can provide.
Had the protocol needed a pty, shared memory, or a local filesystem path for
correlation, this seam would leak — and the leak would only appear when the
second implementation arrived.

## Why the adapter runs inside

The adapter runs **in** the environment, not on the host talking to it. This is
the decision the rest follows from.

The alternative — host-side adapter, remote agent CLI — sounds lighter and is
wrong. The adapter's job is vendor translation, which means reading the CLI's
native session store, resolving its config, and inheriting its skills and project
files. All of that is local to wherever the CLI lives. Split them and the
adapter needs a remote filesystem for every vendor detail it touches, which is
the same coupling, worse.

So an environment must provide the agent CLIs and the adapters. For local that is
already true. For Khora it is a Template concern: an image with `claude`, `codex`,
`pi`, the adapters, and credentials mounted at runtime.

## Implementations

| Environment | Substrate | State |
| --- | --- | --- |
| `local` | `tokio::process`, host filesystem | Today's behaviour, unchanged |
| `khora` | Khora sandbox: `ProcessService` + `FsService` | The reason the seam exists |
| `ssh` | One multiplexed connection, `sftp` for files | Designed for, not first |

### local

The floor and the default. `spawn` is `tokio::process::Command` with
`process_group(0)`; `signal` is `killpg`; `files` is `paths.rs` containment
against the session's workdir. Behaviour is bit-for-bit what ships now, which is
what makes the refactor verifiable: the existing suite is the regression test.

### khora

Khora provides exactly the four operations, which is why it is the shape the
trait was drawn around rather than an adaptation of it:

| Sylph needs | Khora offers |
| --- | --- |
| `spawn` | `ProcessService.Start`, streamed stdout/stderr/exit |
| duplex stdin | `StreamInput` / `SendInput` / `CloseStdin` |
| `signal` | `SendSignal`, preserved end to end |
| reconnect | `Connect` replays a bounded tail, then live events |
| `files` | `FsService` read/write/stat/list/mkdir/remove/move |
| health | lifecycle Events with a replayable cursor |

Two Khora properties change the design rather than just satisfying it.

**The data plane exists only while `running`.** Pause closes admission, drains,
then quiesces the guest. So a sandbox environment has a lifecycle Sylph must
model: a session's link dies when its sandbox pauses, and auto-pause on idle is a
Khora feature, not a failure. This is the same detachment the protocol already
handles for an idle link — which is why links were designed as disposable from
the start rather than as a connection to keep alive.

**`Connect` replays a bounded tail, and is explicitly not durable.** So it cannot
be the record. It is a reconnect convenience; the journal is the record. That
distinction is what keeps the sandbox path from quietly having weaker history
than local.

Khora models no caller, account, or work unit by design, and Sylph must not push
those into it. A sandbox is addressed by id and carries no Sylph identity; the
mapping from session to sandbox lives in Sylph's journal. Pushing session
semantics into the engine is rejected by its own §1.3 litmus, and a Sylph feature
that needs it is a Sylph bug.

### ssh

Included because designing for two implementations is how a seam ends up fitting
exactly two. `spawn` is a channel on a multiplexed connection, `signal` is the
channel's signal request with `kill -<sig> -<pgid>` as fallback, `files` is
`sftp`. Not built first, but nothing in the trait forbids it.

## Binding a session to an environment

An environment is chosen at session creation and recorded on the session:

```json
{"environment": {"kind": "khora", "id": "sbx_01H...", "template": "coding-2026-08"}}
```

Immutable for the session's life. A session's transcript is a record of work in a
place; moving it mid-history would make paths, tool output, and diffs refer to a
filesystem that no longer exists. Moving work to another environment is a
**fork** — a new session with a recorded ancestor, which is a relationship
TraceDB already represents.

`kind: local` is the default, and a session created without the field is local.
That keeps every existing session and client valid rather than needing a
migration.

## Why the record does not move with the work

TraceDB ingests from each agent's **native store on the host**. In a sandbox that
store dies with the sandbox, so nothing is ever ingested — the archive silently
has a hole exactly where the isolated work happened.

The fix follows from where the events already are. The server sees every
normalized event, because the protocol is what carries them, so the server is the
one component present for all work regardless of environment. It journals the
normalized stream as the durable record and feeds TraceDB from there.

This makes the journal real history rather than a bounded projection — a change
this repo has to make deliberately, since the journal was explicitly not a second
transcript archive. The trade is accepted because the alternative is
per-environment sync paths that each lose the tail on a crash, and because it is
the only option that gives local, sandbox, and SSH identical fidelity.

Vendor-native stores remain valuable where they exist: a local session's
`~/.claude` store keeps its own fidelity and TraceDB keeps ingesting it. The two
are reconciled by native session id, which is how a session already deduplicates
against the archive.

`docs/data-architecture.md` is authoritative for what follows from this, and two
of its consequences bind an environment directly. What a remote environment sends
is **native source bytes, not projected rows**, because projection is the part most
likely to change and the part an environment should never own — upload rows and
every environment must be upgraded in lockstep with the schema. And the direction
is upload only: an environment sends to the server, and the server never writes
session data anywhere outside `~/.sylph`.

## Health is per environment

`/api/v1/health` reports `tracedb.available` today. With environments it also
reports each configured environment's reachability, because "the agent will not
start" has a different cause and a different fix per environment — a missing
binary locally, an unreachable orchestrator or an exhausted node for Khora, a
dead connection for SSH. Collapsing those into one boolean is what turns a
five-second diagnosis into an hour.
