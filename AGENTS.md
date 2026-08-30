# Sylph Web Development Guide

Sylph is a coding-agent workspace: a server holding the authoritative record of every
session, an envagent on each machine that runs agents, and a web frontend as one
transport onto the server.

This file holds rules and facts that stay true as the project moves. It
deliberately records no progress, phase, or status: `docs/roadmap.md` owns
those, and duplicating them here only creates a second copy to forget.

## Product Invariants

- **There is one class of session.** Sylph does not divide sessions by who started them,
  and no column records it. The one behavioural flag is `inject_identity`, a boolean
  settable at creation and refused afterwards, because a transcript records what produced
  it. Nothing resets a session, because that would replace its identity mid-life.
- **Identity is `(environment, agent, native_id)`.** No Sylph-minted UUID, so no
  second id space and no comparison that is silently always false. `environment` is an
  opaque id minted at pairing, never a hostname. The delimiter is not `/`.
- **What varies is the link, not the session:** two typed facets, `observe` and
  `control`, each able to report unavailable with a reason. An empty capability surface
  means "Sylph cannot reach it", never "there is nothing there".
- **`fidelity` is a property of the record** (`stream` | `blocks` | `messages`), not of
  who started the session. A renderer reads it before drawing a duration.
- **The server's store is the one authority, with one event vocabulary.** Native
  history is parsed by an adapter into the same events a live turn emits. There is no
  separate archive, no second per-vendor normalizer, and no id translation.
- **The envagent dials outward to the server, never the reverse**, and adapters run
  inside it — on the machine whose vendor files they read.
- **Sessions do not migrate between environments.** Moving work elsewhere is a fork.
- Bind to loopback by default. Any non-loopback deployment requires a separate
  authentication and threat-model change, which now also covers who owns an envagent.
- **Three agents: `claude`, `codex`, `pi`.** `gemini` and `opencode` are not supported.

## Orientation

- `crates/protocol/` — the event vocabulary and the adapter contract, as types. No
  runtime: no `tokio`, no `sqlx`, no `axum`, and no `chrono`.
- `crates/adapters/` — the three adapters. Each owns whatever process its vendor CLI
  needs, and reads that vendor's native store.
- `crates/fts5-jieba/` — the SQLite FTS5 tokenizer, linked into the server so Chinese is
  searchable. Vendored rather than depended on by path: an absolute path into a sibling
  checkout is only ever right on one machine.
- `backend/` — Rust, Axum, SQLx. HTTP and SSE on port 4317 by default. Holds the store,
  the supervisor, and the API; becomes `crates/server` when the envagent splits out
  (`docs/roadmap.md` § What remains).
- `frontend/` — Next.js App Router, React, TanStack Query, Tailwind.
- `docs/` — the governing specs. Read the relevant section before implementing.
- `scripts/` — install and deployment tooling.

**`crates/envagent` is not started**, so anything in `docs/` describing a paired remote
machine, an SSH bootstrap, or a sandbox is intent rather than description; `docs/roadmap.md`
§ What remains is the boundary. Everywhere else, treat a disagreement between `docs/` and
the code as a bug in one of them, and read § Spec and Code Evolution before deciding
which.

| Question | Authoritative source |
| --- | --- |
| What is built, what is next | `docs/roadmap.md` |
| System design, session model, identity, authority | `docs/architecture.md` |
| A vendor CLI's shapes, quirks, control surface | `docs/agents.md` |
| Endpoint contracts | `docs/api-design.md`, `docs/api-v1.openapi.json` |
| Tables, indexes, migrations | `docs/database-schema.md` |
| How the frontend is built | `docs/frontend-architecture.md` |
| Panes, surfaces, components, tokens | `docs/ui-layout.md` |
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
# rust — the whole workspace, not just the crate you edited
cd backend && cargo run
cargo test --workspace && cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps      # warns on a doc link that resolves to nothing

# frontend
cd frontend && npm run dev
cd frontend && npm run build && npx eslint . && npm test && npm run e2e
```

`npm run build` type-checks, so it is the TypeScript gate; there is no separate
`tsc` step.

`cargo doc` is a gate too, and a cheap one: an intra-doc link that resolves to nothing is
a pointer a reader cannot follow, and it fails silently — nothing else in the build
mentions it. Two rules cover every instance this repo had. A private item cannot be linked
at all, so bracketing one is a promise rustdoc will not keep — use a plain code span. And
in an inner (`//!`) comment, paths resolve from the **crate root** rather than the module,
so a bare `fold` link inside `storage/mod.rs` means `crate::fold` and finds nothing; write
the absolute path.

E2E starts both servers itself on offset ports, with a throwaway `SYLPH_HOME` and the
`e2e` cargo feature, which compiles in a fixture agent *instead of* the three real ones —
so it never touches your store or spends tokens. Do not start servers by hand for it. It
shells out to `cargo`, so the `PATH` caveat below applies, and a cold run compiles the
backend first. It refuses a port an interrupted run left behind; clear them with
`for p in 4318 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done`.

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
| `SYLPH_DATA_DIR` | server | `<home>/data` |
| `SYLPH_SOUL` | server | `<home>/SOUL.md` |
| `SYLPH_QUICK_START_DIR` | server | `<home>/scratch` |
| `SYLPH_LOG`, then `RUST_LOG` | server | `info` |
| `SYLPH_ENV_HOME` | envagent | `$HOME/.sylph-env` |
| `SYLPH_ENV_SERVER` | envagent | *(none; written by `pair`)* |
| `SYLPH_BROWSE_ROOTS` | **envagent** | *(none; `$HOME` always included)* |
| `SYLPH_ENV_SPOOL_MAX` | envagent | `512 MiB` |
| `SYLPH_WEB_BACKEND` | frontend | `http://localhost:4317` |
| `SYLPH_WEB_DEV_ORIGINS` | frontend, `next dev` only | *(none)* |
| `SYLPH_WEB_PREFIX` | `scripts/install.sh` | `$HOME/.local/share/sylph-web` |
| `SYLPH_WEB_BIN_DIR` | `scripts/install.sh` | `$HOME/.local/bin` |
| `SYLPH_WEB_FRONTEND_PORT` | `scripts/install.sh` | `3000` |

Each of these has a `config.toml` key except the two that locate the file itself.
There are **two roots and two files**, because there are two products: the server does
not embed an envagent, so "the machine the server runs on" is not a privileged
environment with its own code path.

There is no adapter directory. Adapters are linked into the envagent, so the escape hatch
for "a vendor changed faster than the shipped adapter" is upgrading the envagent on that
one machine — which touches no server, no schema, and no other environment.

`SYLPH_BROWSE_ROOTS` **belongs to the envagent**, not the server: only the machine
holding a path can canonicalize it, resolve `..`, or see whether a component is a
symlink, so a server checking a remote path would be matching a string against a
filesystem it cannot see. It is colon-separated and *adds* to `$HOME`, which is always
browsable. It bounds **two** things, and the second is the larger one: what
`/api/v1/fs/*` may *enumerate*, and what a client may *name* as a session's
`workdir` on `POST /sessions` and `POST /sessions/adopt`. A workdir is the
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

The journal is always `journal.db` inside the data dir, with `history.db` and `blobs/`
beside it; all three are derived rather than separately configurable, because they are one
store split for SQLite's sake.

**No setting names an external history binary**, because history is one store the server
owns. "Is the archive reachable" is replaced by **per-environment health**, a better
question: it distinguishes a missing CLI from a sleeping laptop from an exhausted sandbox
node. Retention's authority follows — the only downstream copy is the vendor's own native
store, so a confirmation comes from the envagent holding it, and an unreachable environment
means the question was never put: **recorded as withheld, never as a zero bound.**

Three surfaces report what resolved, all from one producer so they cannot disagree:
`doctor`, `GET /api/v1/config`, and Settings → Configuration. The envagent has its own
`doctor`, which also reports which agent CLIs it found and which server it is paired
to.

`SYLPH_WEB_DEV_ORIGINS` is comma-separated **hostnames**, no scheme and no port.
Next 16 blocks cross-site requests to `/_next/*` as DNS-rebinding defence, so a
browser reaching the dev server from a LAN IP gets 403s on every chunk and the page
freezes before React hydrates — which reads as a backend failure rather than a
dev-server policy. It is not needed for localhost, nor behind the loopback reverse
proxy, which rewrites `Origin`. See `docs/development.md`.

## Pitfalls Worth Remembering

These are properties of the toolchain, the test harness, and the platform — the things that
cost real debugging time and that no spec section owns. **Design rules live in `docs/`**, and
this section points at them rather than restating them:

| Class of defect | Owner |
| --- | --- |
| Reading a vendor payload: wrong key, wrong unit, wrong shape | `docs/agents.md` § Reading a vendor payload |
| A vendor's `success` that did not do what you asked | `docs/session-protocol.md` § Three rules about reading a vendor's answer |
| Delegation, `span`, promotion, `abandoned` vs `failed` | `docs/session-protocol.md` § Delegated trajectories |
| Retention, watermarks, "cannot ask" ≠ "nothing is safe" | `docs/storage.md` § Retention is watermark-driven |
| Identity, one id space, no session class | `docs/architecture.md` § Identity |
| Path containment, symlinks, where agents may run | `docs/configuration.md` § `browse_roots` bounds where agents run |
| Layout, viewport, ARIA, choice lists | `docs/ui-layout.md` |

One rule generalizes across every entry below and is worth stating once: **a defect that
produces a plausible value rather than an error is the expensive kind.** A crash is found
immediately; a wrong number, a silently dropped event, or a check that cannot fail survives
for months and is confirmed by whatever test was meant to catch it.

### Toolchain

**`cargo` is not on the default `PATH`.** Use `~/.cargo/bin/cargo`, or export
`PATH="$HOME/.cargo/bin:$PATH"` first. A bare `cargo test` fails with `command not found`,
which reads like a broken repo rather than a shell problem.

**SQLx runs queries at runtime here, not at compile time.** The codebase uses
`sqlx::query`, never the `query!` macros. There is no `DATABASE_URL` and no `.sqlx` offline
directory, so reaching for `query!` will not compile. SQL mistakes surface in tests, which
is why store-level tests matter.

**`cargo doc` is a gate, and a cheap one.** An intra-doc link that resolves to nothing is a
pointer a reader cannot follow, and nothing else in the build mentions it. Two rules cover
every instance this repo had: a private item cannot be linked at all, so bracketing one is a
promise rustdoc will not keep — use a plain code span; and in an inner (`//!`) comment paths
resolve from the **crate root** rather than the module, so a bare `fold` link inside
`storage/mod.rs` means `crate::fold` and finds nothing. Write the absolute path.

**`next lint` was removed in Next 16.** Invoke ESLint directly (`npx eslint .` or
`npm run lint`). `npx next lint` fails with a misleading `Invalid project directory` error
naming a `lint` directory.

**`npm run build` is the TypeScript gate**; there is no separate `tsc` step.

**Deleting a route leaves `.next/dev/types/` behind, and the stale copy fails the type
check.** The error names a module in `app/` that no source file references, so it reads as an
unfinished deletion rather than a cached artifact — and `.next` is gitignored, so the state is
invisible to `git status`. `rm -rf .next` and rebuild.

**`SYLPH_WEB_BACKEND` is read at build time.** Next compiles rewrites into the route
manifest, so it must be set for `next build`, not only for `next start`.

**macOS ships bash 3.2, so `wait -n` does not exist.** `start.sh` polls both PIDs with
`kill -0` instead. Any new shell script here has to hold to 3.2 syntax.

**Reading a mid-write file yields a phantom error.** With concurrent work in the tree, a
compiler or linter can capture a half-written file and report an error whose line numbers do
not match the source. Re-run before believing it.

### The E2E suite

**Testing "it streams" needs a runner slower than one render.** Every spec once asserted
final state against a mock that finished in well under a second, so a client that dropped
every mid-turn event passed the entire suite. The `paced` scenario in
`crates/adapters/src/e2e.rs` spaces its stages a second apart, and `e2e/streaming.spec.ts`
asserts an earlier stage is on screen while a later one has not been emitted yet. **Asserting
only the end state cannot distinguish streaming from batching.**

**The E2E fixture is a compiled-in adapter behind the `e2e` cargo feature, and the
exclusivity is load-bearing**: a build offering both it and the real three would let a spec
pick `claude` and spend real tokens. `Agents this build can run: e2e` in the backend log is
what confirms the harness is testing the fixture — check there before reading a suite that
fails every session-creating spec as a product bug.

**Clear the suite's ports before diagnosing a failure that happened before any spec ran.**
A backend surviving from an interrupted run makes the suite fail at startup, because
`reuseExistingServer: false` correctly refuses to trust it.

```sh
for p in 4318 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
```

**`npm run e2e` wipes `.e2e-home/` before the run, and the npm script is the only place that
can.** Each spec's `mkdtemp` workdir becomes a permanent sidebar group, since grouping is by
workdir and the journal was never cleared. At 151 groups the suite ran 35 minutes instead of
1.5 and two or three specs failed per run on the 30s timeout — **a different two or three each
time, which reads as flakiness rather than as accumulated fixture state.** Raising the timeout
does not help, because the cost grows without bound. `SYLPH_E2E_KEEP_HOME=1` opts out to
inspect a journal after a failure.

**So `npx playwright test` directly does not wipe, and a session of targeted re-runs
accumulates exactly the state the wipe exists to prevent.** Measured: 727 sessions and
11,168 blocks in a working checkout against 1 in a fresh one, after a day of running single
specs to verify changes. The spec that then failed was fine — it passes on a clean home, and
failed again as state rebuilt within one wipe cycle. Run `node e2e/reset-home.mjs` before
believing a timing or ordering result from a direct invocation, and **anything asserting on a
comparison across runs must own its reset** rather than assume one happened.

**Neither Playwright hook can do that wipe, and both fail the same way.** A `globalSetup` runs
*after* `webServer`, because `webServer` is a plugin and plugin setup is ordered first; module
scope in `playwright.config.ts` runs again in every worker process. Both delete `journal.db`
out from under a backend that has already opened it, and the suite answers `unable to open
database file` — the module-scope variant eight seconds in, which reads as a mid-run database
failure rather than as a fixture wiping itself.

**The wipe removes the trigger, not the hazard.** A single run creates over a hundred sessions
and the sidebar caps its listing at 200, so a spec late in the order can still meet the cap —
past which a new session *displaces* an older row instead of adding one, and a "the group grew
by one" assertion stays flat. **Assert on identity** ("the session I just created is listed
here", matched by id), or count against a `mkdtemp` directory of the spec's own.

**Playwright's name and text matching is a case-insensitive substring**, and this has bitten
repeatedly. `getByRole({ name: "Explore" })` also matches "Focus Explore"; `getByText("slowest")`
also matches "slowest first". Distinct names need `exact: true`. Two harder forms both pass
indefinitely first:

- **A name that is a substring of a *fallback* name.** The collision exists only in the states
  that render a nameless node, so it passes until it fails in a state that is never the one
  under test — presenting as the card being wrong rather than the locator being loose.
- **A negative assertion on a container.** `toContainText` walks the subtree, so
  `not.toContainText(X)` on a card is coupled to copy *anywhere beneath it*. Such a locator can
  also be satisfied for the wrong reason, which retires the question instead of asking it.

**A layout bug below a breakpoint is invisible to a suite that runs above it.** Three mobile
failures survived 41 green specs because every one of them ran at 1280px. A viewport is an
instrument, not a variant. Note also that a `devices["iPhone …"]` profile runs in *Chromium*
unless WebKit is installed, so it claims iOS coverage it does not have — prefer a Chromium
device profile and be explicit about what is therefore untested.

**A control's disabled state usually encodes more than one fact, so it is a bad "is it
finished" signal.** Waiting for `Send` to re-enable looks like waiting for the turn to settle
and is not: `Send` is also disabled whenever the composer has no text, which is exactly the
state a spec is in right after sending. 33 specs once failed on their deadline with nothing
wrong in the code under test. **Prefer a control whose *presence* changes** — the composer
swaps Send for Stop while a turn is live.

**A one-shot `isVisible()` is a race wherever an auto-retrying assertion belongs, and the `if`
around it is what hides the race.** `answerReceived` in `requests.spec.ts` probed a `more`
toggle once and clicked it only if already present; the region mounts a beat before the toggle
inside it, so losing that race meant no click, no expansion, and a caller burning its whole
10s against the collapsed line. Measured A/B, that file six runs each: 2 of 6 failed with the
probe, 0 of 6 with `await expect(more).toBeVisible()` before the click.

Two things generalize past the fix. **The error accuses the wrong layer**: it read as *the
answer never arrived*, pointing at the request pipeline, when the answer had arrived every
time and only the expansion was missing — so read what the received string actually *is*
before believing what its absence implies. And **a race in a shared helper distributes itself
across that helper's callers**, so four specs took turns failing and none of them was wrong.
When failures rotate within one file, look at what the file shares before looking at any spec
in it. (Same shape one tier up: `docs/database-schema.md` § A terminal status means the record
is finished, where a rotating set of specs shared one conflated signal.)

An `if` guarding an optional element is still sometimes right — but only when absence is a
real, expected state. Here every caller asserted on a payload that exists only when expanded,
so absence was never valid and tolerating it converted a loud failure into four quiet ones.

**`Stop` disappearing once meant `terminal` but not `settled`, and that gap is now closed — the
lesson it taught is not.** The terminal row used to commit *before* open trajectories were
closed, so every spec asserting on what the sweep produces raced it, and under load whichever
one lost was the one that failed: it read as flakiness and once as a branch regression when the
branch was innocent. A turn now goes terminal only after the sweep
(`docs/database-schema.md` § A terminal status means the record is finished), so `Stop`
vanishing is a settled-turn signal.

What generalizes is the diagnosis, and it is worth more than the fix. **A state that carries
two facts under one name cannot be waited on**, because a waiter cannot say which one it got —
and the symptom is never a failing assertion in the code that conflated them. It is a different
spec failing each run, somewhere else. When a suite flakes on a rotating set with one shared
dependency, suspect the signal before the specs.

**An OOM-killed suite is indistinguishable from an interrupted one, so the machine is the last
thing anyone suspects.** A suite killed for memory reports **zero failures at the point of
death** — exit 137 or 143, no summary line, every spec green up to the cut. Check free pages
before diagnosing anything else, and run in groups rather than raising a timeout.

**Two browser contexts, not two tabs**, whenever a spec covers something reaching a *second*
client: two tabs share a cookie jar and can pass while a genuinely separate client still hangs.

### Testing, generally

**A new test is only proven by watching it fail.** Write it, break the thing it covers,
confirm it goes red, restore. Otherwise it may be asserting something that was never in doubt
— which is how a property with a comment calling it load-bearing can turn out to have no
coverage at all.

**Run the suite after the last edit, and paste what it printed.** Not "run the suite" — after
the *last* edit. Six subagent reports in one session claimed green against trees that did not
compile, had a failing test, or were missing the changes the report described. The check that
catches all six is the order.

**Running one spec in isolation is a different measurement from running its file**, and
running its file is different from running the suite. A spec that passes with `-g` and fails
in its file is telling you about shared state or a shared window, not about itself.

**A test that catches a race by luck stops catching it silently, and a later fix to that test
is the likeliest thing to retire it.** One test landed a frame in a window where the read loop
dropped it, failing about one run in five. The defect fix was right; then the test's own shell
escaping was fixed by moving its payload to a file — which removed the timing that made it
fire. Re-measured with the defect deliberately restored, it passed 8 of 8. Coverage had gone
from intermittent to **zero**, and the commit that did it cited 15 consecutive passes as
evidence.

Two rules follow, and the second generalizes:

- **A property observable only through timing needs the window opened on purpose.** Make the
  peer emit, *sleep*, and only then answer, so the loop provably sees the bad state.
- **Before changing the mechanism a test depends on, ask which property each assertion stands
  on.** Two assertions in one test can rest on two different mechanisms, and repairing one can
  retire the other with the suite green throughout. Split them so they fail for different
  reasons.

**A test that hardcodes what the code hardcodes proves only that they agree.** Assert against
the *configured* value read back from the harness, not against a literal both sides share.

**A fixture's comment is not evidence about the fixture.** Read what its values actually
resolve to — `mkdtemp(os.tmpdir())` is `/var/folders/…` on macOS, not `/tmp` — especially
before making something depend on them.

**A fixture's size and timing are part of the contract.** A two-item list cannot observe an
unscrollable, unsearchable catalogue; a scenario emitting its whole tree inside one
millisecond gives every bar equal length. `docs/agents.md` § Testing an agent module has the
rest.

### The platform

**A proxy that gzips an SSE stream buffers it invisibly.** Next.js proxies `/api/v1/*`
through the standard `compression` middleware, which compresses any compressible content type
— `text/event-stream` included — and a gzip stream cannot be decoded until its block is
flushed. The whole turn then lands in the browser at once, while `curl` sees perfectly paced
frames because it sends no `Accept-Encoding`. **That asymmetry makes a transport bug look like
a rendering bug.** The event stream sets `Cache-Control: no-cache, no-store, no-transform`;
`no-transform` is what forbids the re-encoding, and it belongs on the backend response so it
holds behind any proxy, tunnel, or nginx.

**`EventSource` dispatches by frame name, so `onmessage` sees almost nothing.** Every frame
the backend emits is named after its event kind, and `onmessage` receives only frames named
`message`. Per-kind `addEventListener` is not the fix either: a kind added later is silently
dropped, and a frame named `error` collides with the browser's own connection-error event.
`frontend/lib/use-sse.ts` reads the body with `fetch` and parses frames itself, which is also
what makes the parser unit-testable — `EventSource` is not, which is why the original bug
shipped untested.

**Auth is a `SameSite=Strict` cookie, so the frontend must proxy `/api/v1/*` same-origin.** A
browser will not send the cookie cross-origin to another port. The token rotates on backend
restart, which invalidates open browser sessions by design — do not "fix" it by persisting the
token.

**`output: "standalone"` omits `.next/static` and `public`.** Without those copied in, the
server starts cleanly and every asset 404s — which reads as a broken app rather than a broken
install. `scripts/install.sh` copies them and fails the install if `static` is missing.

**launchd holds the log descriptors, and starts from a bare environment.** It opens
`StandardOutPath` and `StandardErrorPath` before exec'ing `start.sh`, so rotating by rename
sends the current run's output into the rotated file; `start.sh` copies and then truncates in
place to keep the inode. launchd also provides only `HOME`, so every other override has to be
written into the plist — which the installer does for whatever was set when it ran. A moved
Node install therefore needs a re-run.

**Killing a subshell does not kill what it exec'd, and the survivor answers on the port.** A
script that backgrounded `npx next start` in a subshell and killed the subshell left `next`
holding the port; the *next* run then measured that server, which serves its own older build,
so the served HTML named chunk hashes the current build never produced. It surfaced as
`referenced chunk is missing from the build`, which points at the build — three theories died
before the right one. **`lsof -ti tcp:PORT` is the one-command answer**, and worth reaching for
whenever output describes a tree that does not match what is on disk.

Three things together fix that class, and any one alone still fails silently: run the job in
its own process group (`set -m`) and kill the group; refuse a busy port instead of measuring
whatever is on it; and make a missing artifact explain the likely cause rather than only
naming the file.

**A default that is one machine's absolute path is wrong everywhere else.** A dependency on a
separate project resolves through `PATH`, which is where its own install puts it. An absolute
default is only ever right by coincidence, and the coincidence holds just long enough to be
believed.

**Renaming a WAL-mode SQLite file means checkpoint-then-rename, not moving three files.** A
crash between renaming `-wal` and `.db` leaves committed data in an orphaned WAL beside a
database that no longer references it — silent loss of exactly the turns that were in flight.
`PRAGMA wal_checkpoint(TRUNCATE)` first, close every connection, then move the main file
alone; the emptied sidecars must be deleted, or the next database opened at that path adopts
them. Nothing performs such a rename today; this is recorded for the envagent's `spool.db`,
which is WAL-mode and will.

**A clean `git merge` can still lose code.** Both sides adding methods to the same object
literal merges without a conflict marker and silently keeps only one set — this dropped two
client methods from `api.files` once, and nothing in git reported it. Only the type check did.
Run both sides' full checks on a merge that reports itself clean.

### Rules that keep costing time

These are not toolchain facts, but they recur across subsystems and each has been paid for
more than once.

**Zero agents is a silent, total failure that reads as a UI bug.** With none available, no
session can be created and every existing one reports `{"attached":false}` — so `commands` is
`[]`, and a composer requiring a non-empty list makes `/` a dead keystroke. The cause is three
layers from the symptom. Check `/health`'s `agents.available` before believing an empty
composer is a frontend bug.

**A degraded dependency is one fact about the run, not one per item or per attempt.** Probe
once per pass and report an absent dependency once at `error`, then at `debug`. Per-item
probing turns one missing binary into a warning per item per pass, and a timer-driven caller
then rebuilds the same flood one layer down. The fix belongs in whatever holds the connection,
not in each caller.

**A subsystem whose correct behaviour is "nothing happened" needs a report, or it cannot be
distinguished from a broken one.** Retention keeping every block is right for a terminal
session and a symptom for a backed one, and both look identical to never having run at all.

**A count repeated in several places is several chances to drift, and nothing checks them
against each other.** One population's size was carried in four passages here and two
disagreed with the other two — a different figure *and* a different agent — so at least one
pair had always been wrong while reading as measured fact. Neither could be re-derived,
because the store they were measured against no longer exists. Three rules: state the *shape*
a piece of code depends on rather than a ratio, since the shape is what stays true; where a
number is worth quoting, cite it once and point at it; and **a measurement is only quotable
while the thing it was measured against still exists.**

**A wrong measurement is harder to distrust than wrong code, because it produces a number and
a number reads as evidence.** Three forms of this have shipped here: a census by substring
over serialized records matched field names appearing inside message *text* and inflated the
very count a port branched on; a reproduction written in another language disagreed with the
artifact it was reproducing, and three wrong diagnoses followed from trusting it; and a
timestamp pair drawn from the wrong era "confirmed" a timezone reading that was backwards.
**When a reproduction and the real thing disagree, the reproduction is the more likely to be
wrong** — it was written from an understanding of the artifact rather than from the artifact.
Read the bytes.

**"It parses without error" is not evidence a parser reads a store correctly.** A
store-reading defect does not raise an error, because a missing key is indistinguishable from
a field the vendor omitted — it produces a plausible record. Verify by asserting on *values it
should have found* against a payload you have printed. `docs/agents.md` carries the measured
instances.

**Deleting a layer orphans its bounds, and the surviving half is what hides the missing one.**
When a layer goes, enumerate the bounds it held **one at a time** and name the new owner of
each. A bound with no owner is not a bound, and "the timeouts moved" is not an answer about any
particular one — one timeout surviving in a plausibly-named file is exactly what makes the
other's absence invisible.

**A setting that bounds one endpoint makes the unbounded one invisible.** `browse_roots`
bounded what `/fs/*` may *enumerate*, was documented in three places, and had 16 containment
tests — so path containment read as solved while `POST /sessions` checked only that a path
existed, and that string is the `current_dir` of a spawned agent. **The narrower capability
being carefully bounded is what kept the wider one from being looked at.** When a setting
bounds one endpoint, enumerate every other endpoint consuming the same kind of value and rank
them by what the value lets a caller *do*.

**Skipping a step that writes a wrong record can delete the right one.** When a fix is "stop
writing X", check what still writes the *record*: an exception to a producer can make its
consumer's default the new bug, and the observable difference between "wrongly described" and
"absent" is one card nobody looks for.

**One fact with two derivations diverges the moment either grows a case.** Read the field;
never recompute beside it.

**A placeholder written into durable data cannot be withdrawn by fixing the producer.** Rows
already journaled with a guess are permanently indistinguishable from real values. A field
meaning "what this is called" must be absent when unknown. And when two layers both supply a
default, **the layer that writes data stops first and the layer that renders changes second** —
reversed, one condition acquires two labels.

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
`frontend/components/timeline.tsx` shows the intended density: it explains why
memoization was chosen over virtualization, and what evidence would reverse that
call.

Do not research basic language syntax, standard library usage, or trivial
implementation details.

## Compatibility

**This project has no other consumer, so breaking changes are free and compatibility
code is forbidden.** Design for the best end state and change
the schema, the wire format, or the internal API in place. Do not write a migration,
a dual-spelling alias, a version branch, or a fallback for a caller that does not
exist. There is one store and one authority, so the rule applies to this repo alone
(`docs/architecture.md` § One authority).

The cost of a compatibility branch is not the branch. It is that the branch makes the
defect it papers over survivable, so nobody removes it — and a fallback that silently
absorbs a rename is exactly how a vendor-key defect becomes permanent (see the
vendor-key family above; the instance that prompted this rule was an upstream rename
absorbed by dual-spelling tolerance). A hard break fails loudly at the one moment someone is looking.

So when a change would be improved by restructuring something older, restructure it.
When existing code exists only to tolerate a shape no longer produced, delete it
rather than extending it. A refactor that leaves the old path in place "just in case"
has not landed.

Two things this does **not** license. It is not permission to break a *vendor's*
contract — an agent CLI is a real external dependency with real users, and an adapter
still adapts. And it is not permission to lose a user's data: a local journal is disposable because it is
re-derivable from agent-native logs, so verify the source still exists before discarding
it. Once Sylph is deployed
and holding data with no other copy — which multi-host upload makes true, since a
remote host's native store may be unreachable — this section's first paragraph stops
applying to *that* data specifically. Treat the presence of irreplaceable installed
state, not a date, as the signal.

## Testing

Write tests for core business logic (session lifecycle, turn supervision, event
normalization), API endpoints (request validation, error responses, status
codes), and edge cases (concurrent access, process crashes, malformed input).

Name a test after the behavior it pins, so a failure reads as a broken promise
rather than a broken function. Assert on observable contract — status, error
code, ordering — not on incidental structure.

**A new test is only proven by watching it fail.** Write it, then break the thing it
covers and confirm it goes red, then restore. Otherwise it may be asserting something
that was never in doubt — which is how a property with a comment calling it load-bearing
can turn out to have no coverage at all, and how a test can be repaired into passing
either way.

**Run the suite after the last edit, and paste what it printed.** Not "run the suite" —
after the *last* edit. Six subagent reports in one session claimed green against trees
that did not compile, had a failing test, or were missing the changes the report
described. The check that catches all six is the order.

**Running one spec in isolation is a different measurement from running its file**, and
running its file is different from running the suite. A spec that passes with `-g` and
fails in its file is telling you about shared state or a shared window, not about itself.

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
