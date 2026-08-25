# Sylph Web Development Guide

Sylph Web is a local-first native interface for Sylph and coding-agent sessions
archived by TraceDB.

This file holds rules and facts that stay true as the project moves. It
deliberately records no progress, phase, or status: `docs/roadmap.md` owns
those, and duplicating them here only creates a second copy to forget.

## Product Invariants

- Treat session origin as durable data. `sylph` sessions use Sylph identity and
  lifecycle policy; `native` sessions preserve the originating agent's identity
  and lifecycle.
- Never inject `SOUL.md`, reset, compact, or rewrite prompts for a native
  session unless the user explicitly converts it to a Sylph session.
- Read durable coding-agent history through TraceDB interfaces. Do not query
  TraceDB's SQLite schema directly.
- The live journal is authoritative only while a WebApp turn is active or has
  not yet appeared in TraceDB. TraceDB is authoritative for stable history.
- Bind to loopback by default. Any non-loopback deployment requires a separate
  authentication and threat-model change.

## Orientation

- `backend/` — Rust, Axum, SQLx. HTTP and SSE on port 4317 by default.
- `frontend/` — Next.js App Router, React, TanStack Query, Tailwind.
- `docs/` — the governing specs. Read the relevant section before implementing.
- `scripts/` — install and deployment tooling.

| Question | Authoritative source |
| --- | --- |
| What is built, what is next | `docs/roadmap.md` |
| System design, data flow | `docs/architecture.md` |
| Endpoint contracts | `docs/api-design.md`, `docs/api-v1.openapi.json` |
| Tables, indexes, migrations | `docs/database-schema.md` |
| UI structure and conventions | `docs/frontend-architecture.md` |
| Where files live, what a setting does | `docs/configuration.md` |
| What an adapter may send, and when | `docs/session-protocol.md` |
| How history is kept, replayed, pruned | `docs/storage.md` |
| Where a session's work runs | `docs/environments.md` |

Record progress in commit messages and the roadmap. Do not add new progress,
handoff, or summary documents: they duplicate the roadmap, then drift from it.
Carry-forward context belongs in the roadmap's open items; a durable gotcha
belongs under Pitfalls below; reasoning about a specific decision belongs in a
comment beside the code.

## Running and Verifying

```sh
# backend
cd backend && cargo run
cd backend && cargo test && cargo clippy --all-targets

# frontend
cd frontend && npm run dev
cd frontend && npm run build && npx eslint . && npm run e2e
```

`npm run build` type-checks, so it is the TypeScript gate; there is no separate
`tsc` step.

E2E starts both servers itself on offset ports with a throwaway `SYLPH_HOME` and a
mock adapter, so it never touches your real journal, archive, or agents.
Do not start servers by hand for it. It shells out to `cargo`, so the `PATH`
caveat below applies to E2E too, and a cold run compiles the backend first.

Run the checks for every part of the repo you touched, not only the one you
edited most. Backend and frontend share a wire contract, so a change on either
side can break the other's tests.

## Configuration

Everything Sylph owns is under **one root**, `~/.sylph`, and everything a user
configures is in **one file**, `~/.sylph/config.toml`. Both are created on first
start, including under `cargo run`. `docs/configuration.md` is authoritative for
the layout, the settings table, and why it is shaped this way.

Resolution is **environment, then file, then a default derived from the root**.
Environment wins so a throwaway instance or a test needs no edit to a user's file;
the file wins over the default because that is what the file is for. Nothing
outside the root is consulted, and no former location is kept as a fallback.

| Variable | Applies to | Default |
| --- | --- | --- |
| `SYLPH_HOME` | backend | `$HOME/.sylph` |
| `SYLPH_CONFIG` | backend | `<home>/config.toml` |
| `SYLPH_HOST` | backend | `127.0.0.1` |
| `SYLPH_PORT` | backend | `4317` |
| `SYLPH_DATA_DIR` | backend | `<home>/data` |
| `SYLPH_AGENTS_DIR` | backend | `<home>/agents` |
| `SYLPH_SOUL` | backend | `<home>/SOUL.md` |
| `SYLPH_QUICK_START_DIR` | backend | `<home>/scratch` |
| `SYLPH_BROWSE_ROOTS` | backend | *(none; `$HOME` always included)* |
| `SYLPH_TRACEDB_BIN` | backend | `trace-db` on `PATH` |
| `SYLPH_LOG`, then `RUST_LOG` | backend | `info` |
| `SYLPH_BUNDLED_AGENTS_DIR` | backend, tests and packagers only | `<exe dir>/adapters`, then `<repo>/adapters` |
| `SYLPH_WEB_BACKEND` | frontend | `http://localhost:4317` |
| `SYLPH_WEB_DEV_ORIGINS` | frontend, `next dev` only | *(none)* |
| `SYLPH_WEB_PREFIX` | `scripts/install.sh` | `$HOME/.local/share/sylph-web` |
| `SYLPH_WEB_BIN_DIR` | `scripts/install.sh` | `$HOME/.local/bin` |
| `SYLPH_WEB_FRONTEND_PORT` | `scripts/install.sh` | `3000` |

Each of these has a `config.toml` key except the first two (which locate the file
itself) and `SYLPH_BUNDLED_AGENTS_DIR`. That one is environment-only on purpose:
which adapters ship with a binary is decided by whoever built it, not by a user, and
offering it in the file would invite pointing a supported setting at adapters
versioned against another protocol release. E2E sets it because bundled resolution
would otherwise find the repo's real `adapters/` and run the developer's actual
agent CLIs.

`SYLPH_AGENTS_DIR` is the *user's own* adapters, searched **after** the bundled
ones so a name there shadows a bundled adapter — the escape hatch when a vendor
changes faster than the shipped adapter catches up. Copying bundled adapters into
it would freeze them at install time and break the next upgrade.

`SYLPH_BROWSE_ROOTS` is colon-separated and *adds* to `$HOME`, which is always
browsable. It bounds **two** things, and the second is the larger one: what
`/api/v1/fs/*` may *enumerate*, and what a client may *name* as a session's
`workdir` on `POST /sessions` and `POST /sessions/import`. A workdir is the
`current_dir` of a spawned agent, so widening this setting widens where arbitrary
code may run — not merely what names are visible. The Quick Start directory is
permitted in addition, always, since both `SYLPH_HOME` and `paths.quick_start_dir`
can move it outside `$HOME`. One key rather than two because `/fs/*` exists solely
to name a workdir, so a second key would have to be kept equal to this one; see
`docs/configuration.md` § `browse_roots` bounds where agents run.

`SYLPH_SOUL` points at the identity preamble. An explicit *empty* value is
meaningful rather than missing — the protocol reserves it for "no identity" — and a
configured path that does not exist also resolves to none, matching the adapters'
own `[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`. Reporting a configured-but-absent file
as present would let the UI promise an injection that never happens. It is resolved
once, in `Config`, so the settings panel shows the same file the adapter reads.
`GET /api/v1/soul` reports what resolved.

The journal is always `journal.db` inside the data dir, with `history.db` and
`blobs/` beside it; all three are derived rather than separately configurable,
because they are one store split for SQLite's sake. A pre-existing `live.db` is
renamed on startup, after a truncating WAL checkpoint — never by moving the three
WAL files, since a crash between renaming `-wal` and `.db` strands committed data
in an orphaned WAL.

TraceDB is a **separate project** this one depends on. It defaults to a `PATH`
lookup, and `tracedb.bin` pins a checkout build. Unreachable is not a failure: live
sessions work and archive features report themselves unavailable — check
`/api/v1/health` for `tracedb.available`. Retention is one of them, and degrades
safely: with no archive it keeps everything and says so in `/diagnostics` rather
than reporting a failure per session.

Three surfaces report what resolved, all from one producer so they cannot
disagree: `sylph-web-backend doctor`, `GET /api/v1/config`, and Settings →
Configuration. `sylph-web-backend migrate --from DIR` moves an older data
directory in.

`SYLPH_WEB_DEV_ORIGINS` is comma-separated **hostnames**, no scheme and no port.
Next 16 blocks cross-site requests to `/_next/*` as DNS-rebinding defence, so a
browser reaching the dev server from a LAN IP gets 403s on every chunk and the page
freezes before React hydrates — which reads as a backend failure rather than a
dev-server policy. It is not needed for localhost, nor behind the loopback reverse
proxy, which rewrites `Origin`. See `docs/development.md`.

## Pitfalls Worth Remembering

These have each cost real debugging time. They are properties of the toolchain
and the design, not of any current task.

**A proxy that gzips an SSE stream buffers it invisibly.** Next.js proxies
`/api/v1/*` through the standard `compression` middleware, which compresses any
compressible content type — `text/event-stream` included — and a gzip stream
cannot be decoded until its block is flushed. The whole turn then lands in the
browser at once, while `curl` sees perfectly paced frames because it sends no
`Accept-Encoding`. That asymmetry makes a transport bug look like a rendering
bug. The event stream therefore sets `Cache-Control: no-cache, no-store,
no-transform`; `no-transform` is what forbids the re-encoding, and it belongs on
the backend response so it holds behind any proxy, tunnel, or nginx.

**`EventSource` dispatches by frame name, so `onmessage` sees almost nothing.**
Every frame the backend emits is named after its event kind (`event: thinking`),
and `onmessage` receives only frames named `message` — so listening there drops
the entire stream. Per-kind `addEventListener` is not the fix either: a kind added
later is then silently dropped, and a frame named `error` collides with the
browser's own connection-error event. `frontend/lib/use-sse.ts` reads the body
with `fetch` and parses frames itself, which is also what makes the parser
unit-testable; `EventSource` is not, which is why the original bug shipped
untested.

**Testing "it streams" needs a runner slower than one render.** Every E2E spec
asserted final state against a mock runner that finished in well under a second,
so a client that dropped every mid-turn event passed the entire suite. The `paced`
scenario in `frontend/e2e/fixtures/adapters/e2e/adapter.py` spaces its stages a
second apart, and `e2e/streaming.spec.ts` asserts an earlier stage is on screen
while a later one has not been emitted yet. Asserting only the end state cannot
distinguish streaming from batching.

**`cargo` is not on the default `PATH`.** Use `~/.cargo/bin/cargo`, or export
`PATH="$HOME/.cargo/bin:$PATH"` first. A bare `cargo test` fails with
`command not found`, which reads like a broken repo rather than a shell problem.

**SQLx runs queries at runtime here, not at compile time.** The codebase uses
`sqlx::query`, never the `query!` macros. There is no `DATABASE_URL` and no
`.sqlx` offline directory, so reaching for `query!` will not compile. SQL
mistakes surface in tests, which is why store-level tests matter.

**`next lint` was removed in Next 16.** Invoke ESLint directly (`npx eslint .`
or `npm run lint`). `npx next lint` fails with a misleading
`Invalid project directory` error naming a `lint` directory.

**`SYLPH_WEB_BACKEND` is read at build time.** Next compiles rewrites into the
route manifest, so it must be set for `next build`, not only for `next start`.
Setting it only at start time leaves a production build proxying to the default
port.

**Auth is a `SameSite=Strict` cookie, so the frontend must proxy `/api/v1/*`
same-origin.** A browser will not send the cookie cross-origin to another port.
The token rotates on backend restart, which invalidates open browser sessions by
design — do not "fix" it by persisting the token.

**Reading a mid-write file yields a phantom error.** With concurrent work in the
tree, a compiler or linter can capture a half-written file and report an error
whose line numbers do not match the source. Re-run before believing it.

**Specs must not count shared state, even though the E2E home is now wiped per
run.** `frontend/.e2e-home/` used to accumulate every session across every run — it
passed 240 during Phase 7 and 357 during Phase 9 — and the sidebar caps its listing
at 200. A spec that creates a session in `/tmp` and asserts "the group grew by one"
therefore passed for months and then failed permanently once the shared group
reached that cap, with a diff-by-one that looks like a UI bug and is not. Past the
cap a new session *displaces* an older row instead of adding one, so the count stays
flat. This bit twice, in exactly the same place, which is why `npm run e2e` now
wipes the directory first (`frontend/e2e/reset-home.mjs`).

The wipe removes the trigger, not the hazard: a run creates over a hundred sessions
of its own, so a spec late in the order can still meet the cap. Assert on identity —
"the session I just created is listed here", matched by its id — or create a
`mkdtemp` directory per spec and count against that.

**That same accumulation also makes the suite progressively slower, and it looks
like flakiness.** The `mkdtemp` directory each spec creates becomes a *permanent
sidebar group*, because grouping is by workdir and the journal is never cleared.
After enough runs the sidebar renders 151 groups of which 148 hold one dead
single-session directory each, and every page load pays for all of them.
Measured during Phase 9: the suite went from 1.5 minutes to 35 minutes, and two
or three specs failed per run on the 30s default timeout — a *different* two or
three each time, which is what makes it read as flakiness rather than as a
fixture problem. Raising the timeout does not help, because the cost grows without
bound. `npm run e2e` now wipes the directory before the run, so this should not
recur; `SYLPH_E2E_KEEP_HOME=1` opts out to inspect a journal after a failure, and
leaving it on brings both symptoms back.

**Neither Playwright hook can wipe that directory, and both fail the same way.**
Measured on 1.60.0. A `globalSetup` runs *after* `webServer`, because `webServer` is
a plugin and `createGlobalSetupTasks` (`playwright/lib/runner/index.js`) orders
plugin setup first; module scope in `playwright.config.ts` runs *again in every
worker process*. Both therefore delete `journal.db` out from under a backend that has
already opened it, and the suite answers `unable to open database file` and 500s —
the module-scope variant eight seconds into the run, which reads as a mid-run
database failure rather than as a fixture wiping itself. The npm script is the only
place with the ordering this needs: one process, once, before Playwright is asked to
start anything. A genuinely fresh home starts clean, so a first-run database error is
this bug and not a product one.

**An E2E fixture that cannot be discovered fails the suite while looking like a
product bug.** Discovery requires `adapter.sh`; after the v1 runner path was deleted,
`frontend/e2e/fixtures/adapters/e2e/` still held only `runner.sh`, so the backend
reported `Agent discovery: 0 usable, 0 broken` and every spec that created a session
or ran a turn failed against an agent that did not exist. The failures read as broken
features, not as a missing fixture. When a protocol version changes, the fixture is
part of the protocol surface — check `Agent discovery: N usable` in the backend log
before believing an E2E failure is about the UI.

**Zero discovered agents is a silent, total failure that reads as a UI bug.** With no
adapters, `/api/v1/agents` is empty, no session can be created, and every existing
session reports `{"attached":false}` — so `commands` is `[]` and the composer's slash
menu, which required a non-empty list to open at all, made `/` a dead keystroke. The
cause is three layers from the symptom and the server logs `0 usable` at the same
level as success. Three things now make it legible: discovery logs at `error` and
names every directory it searched, `/health` reports `agents.available`, and
`LinkState` carries a typed `failure` so the composer can say *which* of "still
connecting", "adapter not installed", or "this agent has none" it is. A stale
`runners/` tree holding v1 `runner.sh` with no `adapter.sh` is now reported as broken
rather than skipped, because such a directory is an agent that cannot be spoken to,
not an absent one. Check `Agent discovery: N usable` before believing an empty
composer is a frontend bug.

**A detached link is a 200, so caching it forever pins the failure.** `/capabilities`
reports detached-with-reason rather than erroring, which keeps a session readable when
its agent is gone — but under `staleTime: Infinity` that made one failed handshake
permanent for the life of the page, and `/` stayed dead until a turn was sent or the
tab reloaded. That is what made a deterministic config problem present as
intermittent. The stale time is now conditional on the failure code: `no_adapter` is
cached like a success because no retry installs an adapter, while a failed handshake
is allowed to go stale.

**A partial capability update must never be able to speak for the whole surface.**
`claude` emits `system/init` once per *turn*, not once per process. An adapter that
answered it with a whole-surface `capabilities` payload carrying only the permission
mode erased the project's 46 slash commands on the first exchange of every session,
and the symptom surfaced two layers away as a composer menu that would not open.
Capability updates are facet-scoped for exactly this reason (`docs/session-protocol.md`
§ Capability facets): an unmentioned facet cannot be cleared. Merging partials would
have fixed the instance and left the hazard.

**Reading the wrong key off a vendor payload fails silently.** `capabilities_from`
read `entry.get("id")` from `claude`'s model catalogue, which answers
`{"value","displayName",…}`. The result was an empty model list on every session ever
opened, presenting as a UI bug in the composer's model control. When adapting a vendor
shape, print one real payload rather than inferring field names — two of three guesses
about this CLI's shapes were wrong, and each one compiled.

This is the most-repeated defect in this file: **six instances so far**, each one
compiling, each surfacing one or two layers from the cause. They are catalogued
individually below (`pi`'s `set_model`, the invented `task_*` subtypes,
`AskUserQuestion`'s `selectedOptions`, `requestedSchema` versus `requested_schema`,
and `pi`'s never-emitted `agent_start`) because the *specific* keys are what a reader
needs. The general rule is the one above, plus three shapes worth knowing before
writing any adapter:

- **A type in a vendor's event union is not a promise that anything emits it.** `pi`
  0.84.4 declares `agent_start` and `agent_end`, documents them, and has **zero emit
  sites** in its shipped bundle — its `Task` tool builds a nested agent whose events
  reach an internal callback that never touches the transport. An adapter written
  from the type definitions ships a tree that never populates. Grep the
  implementation for emit sites, not the types for declarations.
- **An SDK object's attribute names and its JSON wire names are different
  namespaces.** The MCP spec says `requestedSchema`; the Python SDK's dataclass field
  is `requested_schema` with the camelCase form as a Pydantic *alias*, and `claude`
  passes `__dict__` without `by_alias=True`. So the spec-correct key reads `None`
  against a spec-compliant SDK. When the payload came from an SDK rather than off a
  socket, read the Python side.
- **Ask the binary.** Grepping the 321 MB `claude` binary for a key is cheap and
  decisive: `selectedOptions` occurs nowhere in it, which would have caught that
  instance before it shipped.

**serde's `rename_all` does not rename fields inside enum variants.** An internally
tagged enum needs `rename_all_fields` as well, or camelCase wire fields inside a
variant parse as `None` while the code compiles and every non-variant field works.

**A clean `git merge` can still lose code.** Both sides adding methods to the same
object literal merges without a conflict marker and silently keeps only one set —
this happened to `api.files` during the Phase 7 merge, dropping two client methods.
Nothing in git reports it; only `npm run build` did. Run both sides' full checks on
a merge that reports itself clean, especially the type check.

**Sylph's session id and TraceDB's are disjoint id spaces, and confusing them is
silently always-false.** Sylph keys a session by UUID; the archive keys the same
conversation as `<agent>:<native-id>` (`claude:9bdacb6e-…`). Comparing the two
produces no error at any layer — the archive answers "no such session", the caller
concludes nothing is confirmed, and every component behaves impeccably while the
feature does nothing. This is how the first `TraceDbAuthority` shipped fully written
and tested, pruning nothing, with an empty `watermarks` table that looked exactly
like correct conservatism. `tracedb::archive_id` is the forward direction and
`JournalStore::find_session_by_archive_id` the inverse; a type that needs the archive
id to be constructed cannot be asked the unanswerable question, which is the actual
fix. Anything comparing an id against the archive needs a test with a *real* archive
id, because a wrong one passes.

**A subsystem whose correct behaviour is "nothing happened" needs a report, or it
cannot be distinguished from a broken one.** Retention keeping every block is right
for a terminal session and a symptom for a delegated one, and the observable state —
no blocks pruned — is identical. It is also identical to never having run at all,
which is what it was doing. The watermark row is written even when nothing is
confirmed, and `/diagnostics` reports confirmed/withheld/terminal/unchanged
separately, because a single "pruned: 0" answers none of the questions that matter.

**A degraded dependency is one fact about the run, not one fault per item — and not
one per attempt either.** With no TraceDB, a supported configuration, probing per
session turned one missing binary into a warning per session per pass. Probe once per
pass. But that is only half of it: the *spawn* failure was also logged at `error` on
every attempt, so a timer-driven caller reproduced the same unbounded stream one
layer down — visible in the E2E log, which is exactly a no-TraceDB install. An absent
binary is a persistent state, so `tracedb::Client` now reports it once at `error` and
at `debug` thereafter. Any client polled on a timer needs that shape; the fix belongs
in the client, not in each caller.

The related rule: "cannot ask" must never be recorded as "asked, and nothing is
safe", or a watermark claims a check that never happened.

**TraceDB's ingestion ack is archive-global and expensive to read.** It is one
monotonic sequence per ingest *run*, committed after the data transaction so it is
never a false positive — but it says nothing about which events of which session are
covered, so it cannot replace a per-session coverage bound. Its only surfaces are a
gRPC ingest response and `doctor --json`, measured at **2m34s** because doctor walks
every agent's native store and verifies the archive; `trace-db api` does not expose
it. Per-session facts are both cheaper and more precise: `ingestedAtMs` on a `list`
row is a durable per-session commit marker (`upsert_many` writes the session row and
its events in one transaction), and `show`'s `fingerprint` says which version of the
native source was ingested.

**Renaming a WAL-mode SQLite file means checkpoint-then-rename, not moving three
files.** A crash between renaming `-wal` and renaming `.db` leaves committed data in
an orphaned WAL beside a database that no longer references it — silent loss of
exactly the turns that were in flight. `PRAGMA wal_checkpoint(TRUNCATE)` first, close
every connection, then move the main file alone. The emptied sidecars must also be
deleted, or a later `live.db` adopts them.

**TraceDB's API spellings are inconsistent per-operation.** `show` emits
snake_case (`created_at_ms`, `started_at_ms`), while `stats`, `search`, and
`list` emit camelCase (`totalSessions`, `startedAtMs`). The backend client
(`backend/src/tracedb/mod.rs`) pins the exact spelling each operation emits so
a spelling change fails a test rather than silently yielding `None`. Do not add
dual-spelling aliases: they mask future renames and suppress parse errors that
would catch breaking changes.

**The archive stores no event-level delegation structure, and `parent_id` is not
it.** A live turn carries `span` on every event; nothing archived does. `data_json`
is the only field where a raw vendor payload could survive — where `claude`'s own
`parent_tool_use_id` would be — and it is **null on all 36308 events sampled across
all five agents**. `parent_id` is the field that looks like a span and means
something different per agent: for `claude` it is the `parentUuid` conversation
chain (13689 of 15119 pointers name the immediately preceding event, fanout 1), for
`opencode` it groups parts under a message (fanout 4–5), `pi` mixes both, and
`codex` and `gemini` never set it. Honouring `claude`'s as a `span` nests a whole
transcript into one chain thousands deep. Its only real use is pairing a
`tool_result` to its `tool_call`. Attribution to the *specific* delegating call was
measured and rejected too: `claude` names a child `<parent>/agent-a<name>-<hash>`,
but **1918 of 2104** suffixes are hash-only, so matching recovers 8.8% and
misattributes on a collision — structure that is right for one delegation in eleven
is worse than an honest flat view, because nothing on screen distinguishes them. So
archived delegation is session-level in both directions (`parentId`, `delegations`),
and the UI must say the archive records no link to the exact call.

**`session_metadata` is 11.5 ms, not the 0.5 ms this file used to claim.**
Re-measured over 300 sequential calls against a 4922-session archive: 11.5 ms and
~2.5 KB each. So a 20-row listing page spends ~230 ms on lineage alone and a
whole-archive metadata scan costs **52.9 s** — which rules out "just ask every row"
for anything a user waits on, and is why the lineage fold caches per parent. By
contrast a *full unfiltered `list`* is 4922 rows in 10 pages at **164 ms**, so
deriving what you can from the listing is ~300× cheaper per fact than asking per
row. Finding one session's children uses that: an id-prefix test over one pass
finds 2104 of 2316 delegates, while the other 212 (almost all `opencode`, which
records only a `parent_session_id`) would cost the 52.9 s scan and are reached from
their parent's directory listing instead.

**A caller that logs a degraded dependency per request rebuilds the flood the
client was fixed to prevent.** `tracedb::Client` reports an absent binary once at
`error` then at `debug` — but `api::archive` and `api::workspaces` each warned per
request on top of that, and both are polled by the sidebar and workspace picker.
Measured in one E2E run, which is a no-TraceDB install by construction: **2339
identical `TraceDB list failed` lines**. The client now exposes
`unavailability_reported()` so a caller can defer to what was already said, which is
deliberately not the same as string-matching the error message — the text is
TraceDB's and may be reworded, while the flag is the client's own record of having
spoken. This is the second instance of the pitfall below, one layer up from where it
was first fixed.

**TraceDB's `cwd` filter is a substring match, not a path match.** `list` and
`search` compile it to `cwd LIKE '%' || ? || '%'`, so asking for `/w/app` also
returns `/w/app-old` and every subdirectory. Anything that means "this one
directory" must re-filter for exact equality after normalizing trailing slashes —
`api::archive` and `api::workspaces` share `normalize` for exactly this.

**`list` does not collapse fork lineage, but `search` does.** Delegated sessions
are rows of their own and are half of a real local archive (2316 of 4606
non-empty, re-censused after the archive grew to 4922 sessions; it was 2238 of
4416, so the ratio is stable and worth trusting over either absolute), so a
listing that does not fold them into their parent is half noise.
`list` carries no parent column, so lineage costs one `show` per candidate —
`Client::session_metadata` is the cheapest shape, but see the measurement above:
11.5 ms and ~2.5 KB, not the ~0.5 ms once recorded here.

**The `/agent-` id suffix is not a reliable lineage signal.** It catches 2039
subagents but misses 199 that record only a `parent_session_id`, almost all
`codex`; those sat at the top level of every listing until
`TraceSession::lineage` started reading the archive's own record. The suffix
survives as a fallback for when a metadata lookup fails.

**A subagent often runs in a different directory than its parent.** 484 of 2238
do — typically a worktree or a subdirectory. Folding those into a parent that is
not in the directory being listed deletes them from the only listing that would
ever show them: it emptied 42 directories, one holding 193 sessions. A
cross-directory child is promoted to a row of its own, because in its own
directory it is not delegated work, it *is* the work.

**`parent_session_id` and `forked_from` are different relationships.** A subagent
folds into its parent as a count. A fork replays its origin's entire prefix and
continues past it, so the two share every early user turn and read as one
conversation resumed — listing both shows it twice under one label. Only one
survives, and which one cannot be fixed in advance: of the two real cases here,
one has its later work in the fork (1108 events against 389) and the other in the
origin (2459 against 649). `list` is ordered by `endedAtMs` descending, so the
survivor is simply whichever is reached first.

**An unanchored `cwd` filter can starve a paged scan.** Because the match is a
substring, a parent directory's pattern also matches every path beneath it, and
those sort by recency rather than depth. `/Users/x/Workspace/huawei` has 88
sessions of its own whose first exact match lands ~1300 rows in; an 8-page cap
gave up first and reported the directory empty. Bound such a scan in rows
examined (`MAX_UPSTREAM_ROWS`), not pages. `trace-db` also caps a `list` page at
500 rows however many are requested.

**Unknown request fields are silently ignored by `trace-db api`.** Its handlers
read named keys off a JSON object rather than deserializing a typed struct, so a
misspelled field is dropped with no error and the operation runs unfiltered. `list`
takes `since` as a *string*, not `since_ms` as an integer — passing the latter
looks like it works and filters nothing. Verify a new field changes the result
rather than assuming it was accepted.

**The first `user` event is usually not what the user typed.** Agents inject their
own context as leading pseudo user turns (`<permissions instructions>`,
`<environment_context>`, `<skills_instructions>`, memory files as
`# AGENTS.md instructions`, Claude's `Caveat:` preamble). Taking the first user
event verbatim labels most sessions with boilerplate that is byte-identical across
every session of that agent. `tracedb::is_injected_envelope` skips them, which over
311 measured sessions raises recognizable labels from ~10% to 98%. Keep that rule
narrow: an earlier version also treated a leading `You are ` as boilerplate and
discarded 44 genuine labels, because subagent task briefs legitimately open that
way.

**`output: "standalone"` omits `.next/static` and `public`.** The bundle is
otherwise self-contained, so without those two copied in the server starts
cleanly and every asset 404s — which reads as a broken app rather than a broken
install. `scripts/install.sh` copies them and fails the install if `static` is
missing.

**launchd holds the log descriptors, and starts from a bare environment.** It
opens `StandardOutPath` and `StandardErrorPath` before exec'ing `start.sh`, so
rotating by rename sends the current run's output into the rotated file;
`start.sh` copies and then truncates in place to keep the inode. launchd also
provides only `HOME` — the backend's defaults are relative to it, but every other
override has to be written into the plist, which the installer does for whatever
was set when it ran. A moved Node install therefore needs a re-run.

**macOS ships bash 3.2, so `wait -n` does not exist.** `start.sh` polls both PIDs
with `kill -0` instead. Any new shell script here has to hold to 3.2 syntax.

**Archived titles are per-agent, not universal.** `claude` records one for ~73% of
sessions and `opencode` for all of them; `codex` and `pi` record none. Any surface
showing an archived title needs the opening-ask fallback, or most rows read as
untitled.

**`100vh` is taller than the screen on a phone.** It is the *large* viewport
height: it measures as though the browser's collapsible toolbars were hidden, so
a `h-screen` shell overflows the visible area and the document scrolls to make
up the difference — dragging the whole UI up and exposing blank space below the
composer. Use `h-dvh`. This is only half the fix: the document must also be
pinned (`html, body { height: 100%; overflow: hidden }`) and every scroll
container needs `overscroll-behavior: contain`, or a flick that reaches the end
of a pane continues into the document anyway. All three together, or the symptom
persists in a different form.

**Mobile Safari zooms when a focused input's text is under 16px, and does not
zoom back out.** This presents as "the whole screen moved" and is easily
mistaken for a layout bug. Floor input text at 16px under `pointer: coarse` only
— dense chrome legitimately uses 11–12px, and inflating every input to fix a
mobile-only problem is the wrong trade.

**A `<select>` is sized by its longest option, not by its current value.** So a
runner declaring one long model name widens that control permanently, and
because the width comes from content the user cannot see, `min-w-0` and
`truncate` cannot recover it — the intrinsic minimum is already too large. This
is what pushed the composer off the right edge of a phone. A button rendering
the current value can truncate; a `<select>` cannot. It also cannot be styled:
on iOS it renders as a system wheel with its own typography.

**A `ring` is a `box-shadow`, so `overflow-hidden` clips it.** Focus indicators
built with `focus-visible:ring-*` disappear on rows inside a clipping ancestor —
which is where they matter most, since those are the dense lists a keyboard user
navigates. Use `outline` with `outline-offset`; an outline is never clipped.

**Unicode dingbats are font data, not icons.** `📎` and `⚡` render as full-colour
emoji on iOS and macOS, and several geometric shapes (`▩`, `▧`, `▮`) are absent
from the default CJK fallback chain and draw as `.notdef` boxes. What a glyph
looks like is decided by the reader's installed fonts, so an interface built
from them looks different per platform and cannot be corrected. Inline SVG paths
cost ~1 KB gzipped for a whole set and tree-shake per route.

**A menu that is `invisible` while it measures cannot take focus.** `visibility:
hidden` elements are not focusable, so `node.focus()` in the same effect that
sets the position silently does nothing — and since `onKeyDown` is a React
handler on that node, Escape never arrives and the surface cannot be dismissed
by keyboard. Focus in a second effect, after the position commits. The same
class of failure applies to any dialog with no auto-focusing control of its own:
focus the container, or the key handler is unreachable.

**`autoFocus` walks into that same trap, and a text field makes it worse.** React's
`autoFocus` fires at mount, which for an anchored popover is before it has been
measured and therefore while it is still `invisible` — so a filter field declared that
way never receives focus, every keystroke goes to the trigger, and the field reads as
dead. Gate focusing on the same "is it visible yet" state the positioning uses.

**A child's focus call runs before its modal ancestor has recorded the opener.**
React runs child effects before parent effects, and `useDialog` captures
`document.activeElement` in its own mount effect so it can hand focus back on close.
A descendant that focuses itself synchronously is therefore captured *as* the opener,
so closing the surface "restores" focus to a node that is unmounting and drops it on
`<body>` (WCAG 2.4.3). Defer such a focus by a microtask so it lands after the whole
commit. This is invisible on the surfaces whose input is a direct child of the dialog,
which is why it survived until a shared list component was focused from two depths.

**One cursor per list, assigned where the rows are built.** A choice list renders
non-selectable rows (group headings) when unfiltered and not when filtered, so a
cursor counted over "enabled items" and a renderer numbering "all items" disagree —
and the highlight, `aria-activedescendant`, and the committed value then each point at
a different row. The symptom is a click that appears to do nothing, three layers from
the cause. `lib/select-list.ts` returns each row with its own index, or `null` when it
cannot be chosen, so no renderer computes one and the two spaces cannot exist.

The corollary for a surface holding *two* lists: **one cursor per list, plus an
explicit owner for the focused pane.** The history overlay ran a single cursor across
a workspace pane and a session pane, so `Tab` and `ArrowDown` disagreed about which
was active — the same dual-cursor defect one level up. Two `useSelectList` instances
with a named owner cannot disagree, because there is no shared index to disagree
about.

**A navigation list must omit `aria-selected` entirely, not report `false`.** Reporting
`false` on every row claims a selection state the list does not have, and a screen
reader repeats it per row. The combobox-over-listbox pattern does not require the
attribute on rows that are navigation targets rather than choices — so leave it off,
and keep it only where a row genuinely is or is not the current value.

**`aria-activedescendant` belongs on the focused control, not on the list it names.**
This is the one an automated checker cannot catch, and it cost a VoiceOver pass to
find. Where the input and the listbox are the *same* node, putting it on the listbox
is accidentally correct — which is why the command palette worked. Where they are two
nodes, as in the history overlay, focus sits on the input while the attribute sits on
the popup, and arrowing through sessions announces **nothing at all**: the cursor moves
without focus moving, and the announcement follows focus. axe passes either way,
because it verifies the id resolves and cannot verify that a screen reader will walk
the relationship. Third instance this session of well-formed ARIA that a real reader
still fails — **axe checks that the graph is valid; a screen reader walks it.** Both
are needed, and neither substitutes.

**A `role="menu"` may not contain a text field.** Adding a filter to a menu is not a
styling change: a menu may only contain menu items, so an `<input>` inside one is
invalid ARIA that axe reports and that leaves a screen reader with a control its
container says cannot exist. A filterable list has to become the combobox pattern
instead — input as `combobox`, rows as a `listbox` of `option`s, `aria-selected`
rather than `aria-checked`. Audit both states with a list actually open; a static read
will not catch it.

**`pi`'s `set_model` takes `provider` + `modelId`, not `model`.** Measured against pi
0.84.3. Sending `model=` is answered `Model not found: undefined/undefined` — pi
interpolating the two fields it looked for and found absent — and `provider=` plus
`model=` narrows it to `anthropic/undefined`, which is what identifies *which* field is
misspelled. The provider is not part of the value a client sends, so it has to be looked
up from `get_available_models` (372 entries on a real install, so cache it); do not parse
it out of the id, because ids like `stealth/ox-alpha` contain a slash that is not a
provider separator. This is the third instance of the vendor-key pitfall above, and the
most expensive: the refusal came back as a failed RPC that nothing surfaced, so a correct
picker looked broken and the investigation went to the UI twice before reaching here.

**A config value must be unique, because `checked` is computed as a filter.** Every
producer writes `checked: choice.value === currentValue`, which marks *every* match — so
the moment two choices share a value, both rows show a checkmark and neither is
identifiably the real one. Measured on `pi` 0.84.3: of 372 offered models, `deepseek-v4-pro`
and `glm-5.2` are each offered by two providers, and three display names are duplicated
(two "Claude Opus 5", one direct and one via openrouter). A model's identity is
`(provider, id)`, never the id alone, and the bare id also made one of each pair
*unreachable* — a by-id lookup can only answer with one provider. The adapter therefore
issues provider-qualified values, `lib/select-list.ts` enforces one current row per
single-select group regardless of what an agent reports, and the backend validates the
value against what was enumerated. Three layers, because the client renders whatever an
agent it has never seen reports — including agents written by other people.

**Do not delimit a composite config value with `/`.** 350 of `pi`'s 372 model ids already
contain one (`anthropic/claude-opus-5`, `stealth/ox-alpha`), so splitting on the first or
last slash mis-parses the majority of the catalogue. A space works because no provider and
no id contains one. A `ConfigChoice.value` is opaque to the client by contract — a token to
echo back, never to parse — which is what lets the encoding change without a server or UI
change.

**An error that names the wrong layer sends the investigation to the wrong layer.**
Answering a malformed value with "'model' is not settable on a pi session" says the option
does not exist, when the option was fine and the value was not. `pi`'s adapter now
separates `UnknownConfig` from `InvalidConfigValue` for exactly this reason. The general
form cost two rounds here: a refused write with no error path in the client presents as a
dead control, so the layer that is *not* at fault is the one that gets debugged.

**An adapter must read current values from the vendor, not from what it last set.**
`pi`'s `config()` reported `currentValue: self.current_model`, a field only `surface()`
ever populated — so any surface built by another path (a re-attach, a `set_config` reply)
reported `null` while the session plainly had a model, and the chip fell back to "agent
default" with the checkmark on no row. The vendor is also the only thing that knows
whether a set was *honoured*, and a session can be steered by the agent's own TUI
besides.

**A mutation with no error path turns any backend refusal into a phantom UI bug.** The
config mutation had `onSuccess` and nothing else, so a rejected set left the cached
surface untouched, the control re-rendered from it, and the sole observable outcome was
that clicking a model did nothing. Every layer was behaving correctly and the symptom
pointed at the one layer that was not at fault. A refused write must invalidate and be
rendered; the E2E fixture therefore refuses one value on purpose so the failure path
stays covered.

**A two-item fixture cannot observe a list-length bug.** Every symptom of the model
catalogue being unscrollable and unsearchable was invisible to a green suite because
the E2E fixture declared model options two at a time — below any panel's height and
below the filter threshold, so no spec could reach either behaviour. Real agents report
a dozen to twenty-one. When a control's defect is a function of how much it holds, the
fixture's size is part of the test, not a detail of it.

**An accessible-name collision is a defect, not test fragility.** Labelling a
button "Add to this message" beside a textarea labelled "Message" leaves two
controls a screen reader cannot distinguish — and `getByLabel("Message")` then
matches both, which is how 40 E2E specs failed at once. When a trigger needs a
short name and its panel needs a phrase, pass them separately rather than
reusing one string.

**A nested payload cannot describe a stream.** The v3 event table said a subagent
carried "the child's own events nested" inside it, and that was unimplementable
rather than merely unimplemented: an envelope has to be closed before it is sent,
and a trajectory streams for minutes. It also contradicts the journal, where an
event is a row — a nested payload makes one row hold a subtree, so a delta inside
it cannot be addressed, folded, or pruned. Structure travels as a flat stream with
a parent pointer (`span`), which is what distributed tracing settled on and what
the vendor already does with `parent_tool_use_id`. A spec sentence describing a
shape nobody built is worth checking against the storage model before implementing.

**One fact with two derivations diverges the moment either grows a case.**
`StreamEvent::is_stable()` re-computed stability from the phase while the struct
carried a `stability` field computed from the same input. Equivalent, until
`subagent` became stable at *every* phase for structural reasons — after which a
trajectory's `started` was classified stable, published as stable, and silently not
journaled, because the method disagreed with the field beside it. The reload then
flattened every delegated subtree onto the root, which is the exact bug the field
was added to prevent, reappearing one layer down. Read the field.

**Consolidation keeps the closing event's fields, so a name given at open is
lost.** `history::consolidate` folds a block's phases into one row keyed by the
`completed` event. That is right for text and wrong for identity: a delegated
trajectory is named on `started` and renamed by the `delta` carrying its parsed
brief, and its close reports an outcome. Every subagent card therefore read
"Subagent" once a turn consolidated — a symptom that looks exactly like a frontend
fallback firing, two layers above where the name was dropped. Any per-block
attribute that only an early phase carries needs the same treatment.

**Vendor block state keyed by the vendor's own index corrupts concurrent
trajectories.** `claude` numbers content blocks per message, so two subagents
running at once both open index 0. One index-keyed map made each one's deltas
append to the other's text — data corruption, not a display fault, and invisible
until two delegates run simultaneously. Key such state by `(trajectory, index)`.

**Visible text beats `title` for a control's accessible name.** A per-card "focus"
button with `title="Focus Explore"` is announced as "focus" — so twenty delegations
present twenty controls a screen reader cannot tell apart. Use `aria-label`.

**Playwright's name and text matching is a case-insensitive substring, and this has
bitten three times in one file.** `getByRole({ name: "Explore" })` also matches
"Focus Explore"; `getByText("slowest")` also matches the "slowest first" button
beside it, so a count assertion read 1 whether or not any row was badged. Distinct
names need `exact: true`.

Two harder forms, both of which pass indefinitely first:

- **A name that is a substring of a *fallback* name.** `trajectoryName` answers
  "Subagent" for a nameless node, and `{ name: "Agent delegation" }` matches
  "Subagent delegation". The collision exists only in the states that render a
  nameless node, so it passes until it fails in a state that is never the one under
  test — presenting as the card being wrong rather than the locator being loose.
  This appeared in four places in `delegation.spec.ts` while the *button* two lines
  below one of them was already guarded with `exact`: the rule was known, applied to
  the control, and missed on the region it labels.
- **A negative assertion on a container.** `toContainText` walks the subtree, so any
  `not.toContainText(X)` on a trajectory card is coupled to copy *anywhere beneath
  it* — including fixture summary text a later change adds. Such a locator can also
  be satisfied for the wrong reason: one asserting a delegate's outcome was matching
  the agent's prose the whole time, so the spec named the thing and never observed
  it. A locator that passes for the wrong reason is worse than one that fails,
  because it retires the question.

**A collapsed subtree must report what it hides, or collapsing is lossy.** Hiding
a delegation's work by default is only defensible if a failure three levels down is
still visible at the top: otherwise the user must open every branch to learn
something broke, which is the work collapsing was supposed to save. A descendant
failure escalates its ancestors' status, and a failed branch expands itself to the
tool that failed. Keep `abandoned` distinct from `failed` — the server closing a
trajectory an adapter left open means nobody reported an outcome, and colouring that
red reports an interrupted turn as a broken one.

**An agent speaks between turns, and "no turn is running" was read as "nothing can
be said".** Events arriving with no turn in flight were dropped — defensibly, since
an event carries no turn id and a guessed one misplaces it in the transcript forever.
The error was the conclusion: measured on `claude` 2.x, a session with backgrounded
work emits the delegate's completion *after* its turn's `result` and then an entire
exchange of its own to report it, stdout still open, process still alive. So the drop
discarded exactly the completion a user had been promised, and no monitor, schedule,
or background job could reach a client however the rest of the stack was built. The
server now opens an **agent-initiated turn** (`origin: "agent"`) on the first
out-of-turn event — the first of *any* kind, because the vendor's own `turn`/`started`
arrives after the payload, so waiting for the bracket keeps the announcement and drops
what it announces. Three consequences worth holding onto: such a turn has no waiter,
so settling it means recording terminal state directly rather than waking one, and
leaving it unsettled wedges the session (a live turn refuses new messages); its
`requestText` is empty, so a client that does not render `origin` shows an answer to a
message the user never wrote; and a delegate closed by the turn-settle sweep may later
report its *real* outcome on that turn, so `abandoned` is not final.

**A waiter's absence means two different things.** `settle_turn` returned early when
no turn was parked on an outcome, which was accidentally correct and became wrong the
moment agent turns existed. Claiming the waiter first is how a user's stop wins over
the link teardown it causes — so settling every waiter-less turn made a deliberate
stop get reported as `LINK_LOST`, because the teardown reached the closer while the
supervisor was still on its way there. An agent turn never had a waiter; a user's turn
whose waiter is gone has one owner already. `origin` is what separates them, and the
integration suite catches it: `stopping_a_running_turn_cancels_it` failed the moment
the distinction was missing.

**Delegation is only knowable retroactively, so nothing may decide it at open
time.** An adapter learns a tool call delegated when a child arrives carrying its
id — after the `tool_call` event was published. `claude`'s adapter guessed by tool
name (`Task`), which was right until the vendor shipped `Agent` and `Workflow`, and
an MCP server can add another whenever it likes: the whitelist could not be
completed, only extended after each bug report. Two real `Workflow` cards therefore
read *running* permanently with no interior, and the four `Agent` calls beside them
had their children rooted beside the call instead of inside it. A trajectory is now
defined by *use* — any `streamId` another event names as its `span` is one, and a
`tool_call` so named is promoted in place (`docs/session-protocol.md` § Promotion).
The corollary is that a promoted host's lifecycle is its `tool_result`: a
`tool_call`'s `completed` phase means its *arguments* finished streaming, so reading
the phase reports every delegation as finished the instant its input parsed.

**`--forward-subagent-text` only forwards for the built-in `Task`.** Measured on
claude 2.x by printing one real delegated turn: an `Agent` delegation produced *zero*
`stream_event` envelopes carrying `parent_tool_use_id`, and the delegate's own `Read`
arrived as a top-level assistant message indistinguishable from the root agent's
work. So for every delegating tool except `Task`, the interior cannot be attributed
at all and the `task_*` notifications are the whole record. A trajectory with no
children is therefore a real terminal state rather than a loading one, and the UI has
to say so on the card — presented as expandable it invites opening a branch that can
never fill, and saying it only after the click is the lossy collapse the tree exists
to avoid.

**Not every `task_*` notification is a delegation, and `task_type` is the only thing
that says so.** `run_in_background` on Bash reports through the same channel a
subagent does. Measured on claude 2.x: `task_type: "local_bash"` for a backgrounded
shell job against `"local_agent"` for a real delegation, and neither `task_updated`
nor `task_notification` carries the field at all — so it has to be remembered from
`task_started`, or a close opens the very node its start was filtered out of.
Classifying by the notification alone published a *shell command* as a subagent: a
card named `find / -maxdepth 8 -ipath "*workflows*" …`, whose error flag escalated the
Bash call hosting it, so a turn that had done nothing wrong read `failed` from its
first moment. Filter narrowly and let an unknown type through — guessing wrong that
way costs a missing subtree, while the other way mislabels a tool call as an agent,
which is what shipped. `adapters/claude/adapter_test.py` pins it, and exists because
`verify.py` cannot: it spends real tokens and only sees whatever shapes one live turn
happens to produce, so every *classification* bug in this adapter's history was
invisible to it.

**`is_backgrounded` inverts the turn-settle sweep.** The sweep closes trajectories an
adapter left open so that silence never has to explain a gap — right in general, and
wrong for work the agent says runs in the background, which is *expected* to outlive
its turn and to report its own outcome on a later agent-initiated turn. Closing it as
abandoned writes a contradiction that the real completion cannot undo: both records
then exist, on different turns, and the earlier card still says the work never
finished when it demonstrably did. The flag is reported only by `task_started`, so the
adapter re-attaches it to every later phase — the server reads whichever row it finds.
Keep the exclusion narrow: an ordinary delegation left open is the permanent spinner
the sweep exists to prevent. Startup recovery deliberately *ignores* the flag, in both
tiers, because a dead process cannot report — so "abandoned after a restart" stays a
truthful terminal state and must remain distinguishable from "still running in the
background".

**Skipping a step that writes a wrong record can delete the right one.** That
exclusion removed the contradiction and, unnoticed, the delegation: a trajectory with
no close carries no text, so it folded into no block and its children reattached to the
root — the flattening `span` exists to prevent, reappearing one layer down. The
roadmap recorded the item as closed. When a fix is "stop writing X", check what still
writes the *record*: an exception to a producer can make its consumer's default the new
bug, and the observable difference between "wrongly described" and "absent" is one card
nobody looks for. It also only ever helped `claude`; every other adapter reports no
flag and kept getting the contradicting close.

**A cross-turn `span` fabricates a trajectory on the turn it lands on.** A late outcome
names its host from an earlier turn, and promotion-by-reference then made the sweep
close a host that had already succeeded — a failure record for work that never ran
there. The obvious repair, "skip an id another turn declared", is *not available*:
consolidation moves the earlier turn's rows to `history.db`, so the evidence of which
turn owned the host is gone by the time the late outcome arrives. The decidable rule is
that some row *on this turn* must carry the id as its `stream_id`. Anything inferring
structure from a pointer has to ask whether the pointer's target is even in scope.

**When correct behaviour is the *withholding* of a signal, a timing failure is
indistinguishable from the feature working.** A fan-out spec's live window closed
mid-run under load, so a badge that is deliberately withheld after settling was simply
absent — reading as a broken overview rather than as a hold that was too short. The
window is consumed from both ends: it opens before the browser paints, and the whole
assertion sequence has to fit inside it rather than merely start there. Assert the
window is still open immediately before each dependent assertion, so a closed one fails
saying so.

**A fixture's *timing* is part of the contract, not only its size.** The server stamps
`createdAtMs` on arrival, so a scenario emitting its whole tree inside one millisecond
gives every bar equal length — and a client computing durations from the wrong field
then passes the entire suite. Related: a delegate that forwards nothing cannot be timed
from its interior at all, so "each delegate forwards at least one event" is a fixture
*requirement* rather than a nod to realism. This is the sibling of the size rule
recorded above; both say the fixture's shape is the test.

**Focus mode re-roots to `children`, which discards a promoted host's own row.** Right
for a declared `subagent`, whose row carries no content of its own; wrong for a
promoted `tool_call`, whose row *is* the call defining what its delegates are. Focusing
a `Workflow` whose interior the vendor never forwards showed a breadcrumb over an empty
pane — so a delegation that had genuinely reported nothing looked identical to a broken
one. Anywhere the tree is re-rooted or collapsed has to answer the same question the
card does: what does this node hold that is not in its children?

**A vendor's notification subtypes are not guessable, and a wrong guess is
silent.** `task_completed` and `task_failed` do not exist. The real set is
`task_started`, `task_progress`, `task_updated`, `task_notification` — a close
arrives either as `task_updated` with `patch.status`, or as `task_notification` with
`status` plus the delegate's `summary`. Branching on the invented names compiled,
never fired, and left every delegate open forever; the symptom was a spinning card,
three layers from the missing branch. Worse, `task_updated` carries **no
`tool_use_id`**, so the host has to be remembered from `task_started` or the close
lands at the root while the trajectory it closes sits inside a container — which
leaves the container open, the same spinner one level up. This is the fourth instance
of the vendor-key pitfall in this file: print one real payload instead of inferring
field names.

**One host multiplexes N delegates, so a trajectory id must be the delegate's.**
`claude`'s `Workflow` fans out under a single `tool_use_id`. Keyed by the host, a
two-agent workflow rendered as *one* card reading `Demo: agent-2` — the last name to
arrive won and `agent-1` was absent from the timeline entirely. The per-delegate
identity is `task_id`, with the host as its `span`. Same rule as the content-block
index pitfall above, one level up: never key a trajectory by something the vendor
multiplexes. A fixture with one delegate per host cannot observe this — with N=1 the
collapse is indistinguishable from correct.

**A cache keyed on an object the producer reallocates never hits.** The timeline's
`NODE_CACHE` was keyed on `TimelineRow`, and `buildRows` reuses a settled row's
*event* while always allocating a fresh row to hold it — so the cache could not hit
once, and it sat declared-and-unread with a comment describing a guarantee
`buildRows` does not make. Keying on the event fixed it, except for trajectory rows,
whose event is re-folded from its phases every build: those are the rows most worth
skipping and were the only ones that could never be skipped. Before trusting a memo,
check which object the producer actually preserves — `lib/timeline-rows.test.ts` pins
it.

**`useState(derived)` captures a snapshot, and with streamed data the snapshot is
usually stale.** React uses a `useState` initializer once. Both
`useState(status.failed)` and `useState(node.status === "failed")` in `timeline.tsx`
read a flag that arrives *after* mount: a tool call mounts when its `tool_call`
arrives and its failure is known when the `tool_result` does, and a delegation's
status escalates when a descendant fails, which is always later. So
auto-expand-on-failure fired only when both landed in one render — meaning only
against a mock fast enough to batch them, which is the whole suite. The trajectory
instance was the worse one: a failed branch stayed collapsed over the tool that
failed, which is the collapsing-must-not-be-lossy rule breaking silently.

The repair is derived state with a *remembered override*, not an effect. An effect
that force-opens on the transition re-opens a branch the user deliberately collapsed
every time another descendant fails. And note the second bug hiding in the same
expression: `expandAll ?? open` silently outranked the per-row toggle. **Three inputs
to one boolean need a stated precedence, or two of them are latent bugs** — here
expand-all, then the user's toggle, then the derived default.

**A publishing gap is indistinguishable from a missing measurement.**
`history::consolidate` computed and stored `started_at_ms`; `block_to_event` did not
put it on the wire. A consolidated block is keyed on its close, so every settled
trajectory reported an end and no beginning — and a turn switches to consolidated
history moments after settling, so that is every delegation a user scrolls back to.
The delegation overview was therefore *designed* around durations being
unreconstructible after settle, with a documented lower-bound fallback, because the
value looked impossible to obtain while it sat in the table the whole time. Before
building around a limit, check whether the producer already records what you need and
only the wire drops it.

**Making the honest rows exact is what lets a dishonest one be crowned.** Publishing
that field turned every settled duration exact, which for the first time let the
overview offer a rank — and the badge landed on a row labelled "never finished". A
trajectory still open when its turn ended acquires an end timestamp from the server's
close or the turn's own end, so the subtraction is exact over a span whose real end
nobody reported; its true duration can only be longer, so it cannot lose a comparison
and must not win one. **An "is it sound" check on the arithmetic is not an "is it
finished" check on the subject**, and the rank needs both. While every row was a
bound no rank was offered at all, so the gap was unobservable: removing one
degradation can expose a second that the first was masking.

**Two nodes naming each other silently delete both.** A cycle in `span` pointers is
not merely a recursion hazard. Attachment holds the first as an orphan waiting for
the second, which then adopts it *and* is adopted by it — so neither is a root,
neither is reachable, and the pair vanishes from the timeline with nothing reporting
it missing. No single-pass attachment can detect this, so `buildTree` repairs it by
reachability afterwards, and the rollup carries a visited guard because unbounded
recursion in a render path is a frozen tab.

**A control's disabled state usually encodes more than one fact, so it is a bad
"is it finished" signal.** Waiting for `Send` to re-enable looks like waiting for the
turn to settle, and is not: `Send` is also disabled whenever the composer has no text,
which is exactly the state a spec is in right after sending. The wait could therefore
never succeed, and 33 specs failed on their deadline with nothing wrong in the code
under test. A single run hid it because an earlier `toBeVisible` had already absorbed
the wait — only `--repeat-each` exposed it. The composer swaps Send for **Stop** while
a turn is live, which means one thing and is the signal to use. Before asserting on
`toBeEnabled`/`toBeDisabled`, check every input to that boolean; prefer a control whose
*presence* changes.

**A layout bug below a breakpoint is invisible to a suite that runs above it.**
Three mobile failures survived 41 green specs because every one of them ran at
1280px, where the same code is merely cramped. A viewport is an instrument, not
a variant. Note also that a `devices["iPhone …"]` profile runs in *Chromium*
unless WebKit is installed, so it claims iOS coverage it does not have — prefer
a Chromium device profile and be explicit about what is therefore untested.

**Writing an answer into a vendor field that does not exist is silent.** The
`AskUserQuestion` reply set `questions[0].selectedOptions`, a key that occurs
**nowhere** in the 321 MB `claude` binary. The CLI builds its tool result by reading
`answers[question]`, found nothing there, and told the model *"The user did not answer
the questions."* — so a correct pick, correctly rendered and correctly transported,
presented as a UI that had lost the click. Nothing errored at any layer, and
`verify-dialogs.py` passed throughout, because it asserted that a dialog *arrived* and
that answering unblocked the turn — both true while the answer was being dropped.
Grep the binary for the key and round-trip one real payload; and assert on the tool
result the model actually reads, since "the panel closed" and "the answer arrived" are
different claims and only the second is the feature. This is the fourth instance of
the vendor-key family above.

**A client cannot guess what input a request wants.** The approval dialog chose what
to collect by sniffing `detail` for field names it knew (`command`, `file_path`,
`url`). That is right for an approval, whose subject *is* one proposed tool input, and
unfixable for anything else: a question's detail carries `questions`/`multiSelect`, so
an agent's own question rendered as bare buttons with nowhere to type a note or an
answer outside the list, and an `elicitation` — whose entire purpose is free text —
rendered as a read-only dump of its own placeholder. Only the agent knows what it is
asking for, so the request declares a `form` and the client renders exactly that.
Note the shape this forced apart: `choice` had overloaded `options` as both the answer
and the submit button, which is *why* picking had to submit immediately and why a note
had nowhere to live. When a surface cannot express something, check whether one field
is carrying two meanings.

**Answering a multi-part vendor ask partially degrades the answer, quietly.**
`AskUserQuestion` carries up to four questions; forwarding only the first left the
rest unanswered, and the CLI's reply to the model differs by wording rather than by
error: all answered is *"Your questions have been answered … you can now continue with
these answers in mind"*, while one of two is the weaker *"The user answered …"*. Both
look like success from the outside.

**A setting that carefully bounds one endpoint makes the unbounded one invisible.**
`SYLPH_BROWSE_ROOTS` was documented in three places with 16 tests behind it, so
containment read as solved — while `POST /sessions` called `Path::exists` and spawned
an agent in any directory a client named. The *narrower* capability being visibly
bounded is what kept the wider one from being looked at, and the roadmap's own note
that enumeration "is the part that matters" was load-bearing in the wrong direction.
When a bound exists, ask which operations it does *not* cover.

**A write cannot canonicalize its own target, and a check on a resolved path does not
govern an operation on an unresolved one.** Two holes of the same shape, found a day
apart. An `overwrite: true` upload wrote *through* a pre-existing symlink out of the
workdir, because the target does not exist yet — so containment sits on the parent and
the final component is appended verbatim. And a read canonicalized for the check and
then opened the original path, so a symlink *inside* the workdir pointing out of it
passed and was followed.

The rule that fixes both: **derive the path you operate on from the check itself** —
canonicalize once, into one variable, and use that variable. A `contain_in_any` check
paired with a path it did not produce is not a bound. Note also that `remove` was
already correct and its doc comment stated the exact invariant `upload` violated, two
hundred lines away: when one function documents an invariant about a shared idiom,
every other user of that idiom needs checking against it.

**A default that is one machine's absolute path is wrong everywhere else.**
`TRACEDB_BIN` defaulted to `$HOME/Workspace/trace-db/target/release/trace-db` — a
sibling checkout that exists on exactly one machine, so every other install got a
silently unavailable archive that looked like a TraceDB bug. A dependency on a
separate project resolves through `PATH`, which is where its own install puts it;
an absolute default is only ever right by coincidence, and the coincidence holds
just long enough to be believed.

**A test that hardcodes what the code hardcodes proves only that they agree.**
`workspaces_group_sessions_and_always_include_quick_start` asserted the Quick Start
directory ends with `/.agents`, so the endpoint and the test were wrong about the
product together and neither could catch the other. Assert against the *configured*
value read back from the harness. The same rule caught a second one: a test removed
an adapter via `browse_root().join("runners")`, and once the harness layout moved it
silently deleted nothing while still asserting the detached path.

**A settings key the bind ignores is worse than one that is not offered.**
`SYLPH_WEB_HOST` was read into `Config` and then discarded — `main` bound
`([127,0,0,1], port)` literally — so setting it did nothing, with no error. Either
honour a setting or do not accept it. It now binds what was asked for and warns
loudly when that is not loopback, which is a decision the operator can see rather
than one the code quietly overrules.

**Two writes that settle one logical fact will be observed between.** A turn's
terminal status and its session returning to `idle` are separate rows and cannot be
one transaction, so the only question is which inconsistency a client can see.
Settling the turn first left a finished turn on a `running` session — the state that
makes the composer refuse the next message — and the window was two statements wide
and merely improbable until closing delegated trajectories put a query and a
per-trajectory emit inside it. Three integration tests then failed *by timing*, a
different subset each run, which reads as flakiness rather than as ordering. Write
the row that makes the next action legal first.

**A setting that bounds one endpoint makes the unbounded one invisible.**
`SYLPH_BROWSE_ROOTS` bounded what `/api/v1/fs/*` may *enumerate*, was documented as
such in three places, and had 16 containment tests behind it — so path containment
read as solved. Meanwhile `POST /sessions` called `Path::exists` on `workdir` and
nothing else, and that string is the `current_dir` of a spawned agent: any client of
this backend could start an agent anywhere on the filesystem. The *narrower*
capability being carefully bounded is what kept the wider one from being looked at,
and the roadmap note saying enumeration "is the part that matters" was true when
written and then load-bearing in the wrong direction. When a setting bounds one
endpoint, enumerate every other endpoint consuming the same kind of value, and rank
them by what the value lets a caller *do* rather than by how the setting is named.
Both paths now share `paths::contain_in_any`, because a containment rule written
twice is one that will diverge.

**A write cannot canonicalize its own target, so containment on the parent leaves
one component unchecked.** `canonicalize` needs an existing path and the file
usually does not exist yet, so every write endpoint here contains the *parent* and
appends the final name verbatim. That is correct and incomplete: a pre-existing
symlink at that last component is inside the root by name, points wherever it
likes, and `fs::write` follows it — so `overwrite` was a write outside the workdir,
arriving through the one component the check could not see. `remove` was already
safe by construction (unlinking never touches a target) and its doc comment even
asserted that a final escaping symlink is "only ever unlinked, never opened or
written to" — an invariant that was true of `remove` and false of `upload` two
hundred lines away. Inspect an existing entry with `symlink_metadata`, then resolve
and re-check; and when one function documents an invariant about a shared
containment idiom, check every other function that uses the idiom against it.
Verify such a fix by reverting it and watching the test fail — otherwise the test
only proves it agrees with the code.

**Canonicalizing a stored path changes every comparison against it.** Journalling
the canonical `workdir` — right, since it defeats a symlink re-pointed after the
check and stops two spellings becoming two sidebar groups — silently split the Quick
Start group, because `/workspaces` compared that canonical value against the
*configured* `quick_start_dir` and reported `isQuickStart: false` for every Quick
Start session. Nothing errored: the row and the sessions it counts simply stopped
being the same directory. Anything that groups, compares, or displays a path has to
be re-checked when the recorded spelling changes, and on macOS this is not an edge
case — `/tmp`, `os.tmpdir()`, and everything under `/var` all differ from their
canonical form, so the bug is invisible on Linux CI and certain on a developer's
machine.

**A fixture's comment is not evidence about the fixture.** The E2E config set
`SYLPH_BROWSE_ROOTS: "/tmp"` under a comment saying "the specs create sessions in
`/tmp`". They have not since `mkdtemp(os.tmpdir())` was introduced — that is
`/var/folders/…/T` on macOS, not under `/tmp` — and nothing could observe the
disagreement while the value was unenforced. Enforcing it would have failed a dozen
specs at once, reading as a product regression. Read what a fixture's values
actually resolve to rather than what its comment claims, especially before making
something depend on them.

**Seeding a file keyed on its own absence overrides a deliberate deletion.**
`SOUL.md` is seeded on first run, and deleting it means "no identity" — which the
protocol reserves. A seeding rule that only checked whether the file existed
restored it on every restart, silently re-enabling injection. "First run" has to
mean *the config file did not exist*, and restoring identity belongs behind an
explicit `config init`.

## Spec and Code Evolution

Neither code nor spec wins automatically when they diverge. Stop, identify
whether behavior or intent should move, update the governing spec first when the
design changes, then update code.

Do not silently rewrite a spec to bless an implementation accident, and do not
contort code around a design that evidence has invalidated.

Before implementing:

1. Read the governing section in `docs/` and any linked open decision.
2. Verify behavior against source or a spike. Never turn an unverified
   assumption into an invariant.
3. If several materially different shapes remain viable, research reference
   repositories, dependency source, and prior art. Ask subagents for independent
   or contradictory reads when useful.

## Design Decisions

When a decision is uncertain, research it rather than guessing. That means
reading the dependency's source, studying how mature projects solved the same
problem, and measuring with a disposable spike where behavior is unclear.
Uncertainty is not permission to stall: investigate, then decide.

Aim for the best available answer across architecture, performance, UI and UX,
aesthetic judgment, code quality, and documentation quality — not merely the
first thing that works.

Prefer proven patterns and battle-tested libraries. Deviate when Sylph's own
requirements demand it, when profiling shows the standard pattern does not
address the bottleneck, or when security calls for something stricter. Document
the deviation and its evidence where the code lives.

Record decisions as comments next to the code they govern, including what was
rejected and why. A future reader needs the reasoning, not just the result.
`frontend/components/event-list.tsx` shows the intended density: it explains why
memoization was chosen over virtualization, and what evidence would reverse that
call.

Do not research basic language syntax, standard library usage, or trivial
implementation details.

## Compatibility

Before a live deployment, databases and local provider state are disposable:
change schemas and internal APIs in place, and do not add migrations or
compatibility branches for imagined users.

Once the project is deployed and carrying real data, migrations become mandatory
for schema changes. Treat the presence of installed state, not a date, as the
signal that this has flipped.

## Testing

Write tests for core business logic (session lifecycle, turn supervision, event
normalization), API endpoints (request validation, error responses, status
codes), and edge cases (concurrent access, process crashes, malformed input).

Name a test after the behavior it pins, so a failure reads as a broken promise
rather than a broken function. Assert on observable contract — status, error
code, ordering — not on incidental structure.

## Language

Code, documentation, and commit messages are English only: every `.rs`, `.ts`,
and `.tsx` file, everything in `docs/`, all comments, all identifiers and
endpoint names, all test names, and both commit subject and body.

Conversation happens in whatever language the user writes in. Durable artifacts
stay English regardless.

## Commits

Use Conventional Commits scoped to the affected component, and explain what
changed and why — plus how, when the approach is not obvious.

```
feat(backend): add SSE streaming for turn events
fix(db): correct session status transition logic
docs(arch): clarify runner discovery protocol
```

Stage files explicitly. Never `git add -A` or `git add .`, since unrelated
concurrent work may be sitting in the tree. Check `git status` and
`git diff --staged` before committing to confirm exactly what is included.

## Generated Agent Files

`frontend/AGENTS.md` and `frontend/CLAUDE.md` are written by `next dev`, not by
hand. The generator is
`frontend/node_modules/next/dist/server/lib/generate-agent-files.js`, and it
re-creates them when removed. Commit them with your work; reverting them from a
diff only produces the same uncommitted change again.

Their content is a real instruction, not boilerplate: this Next.js version has
breaking changes against older training data, so consult
`frontend/node_modules/next/dist/docs/` before writing frontend code.

The root `CLAUDE.md` is a symlink to this file. Edit `AGENTS.md`.
