# Sylph Web Development Guide

Sylph is a coding-agent workspace: a server holding the authoritative record of every
session, an envagent on each machine that runs agents, and a web frontend as one
transport onto the server.

This file holds rules and facts that stay true as the project moves. It
deliberately records no progress, phase, or status: `docs/roadmap.md` owns
those, and duplicating them here only creates a second copy to forget.

## Product Invariants

- **There is one class of session.** Sylph does not divide sessions by who started
  them. `origin`, `identity_mode`, and `lifecycle_mode` are deleted, along with
  `backend_incarnations` — measured, they had four write sites and one behavioural
  branch between them. What survives is `inject_identity`, a boolean settable at
  creation and refused afterwards, because a transcript records what produced it.
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
- `backend/` — Rust, Axum, SQLx. HTTP and SSE on port 4317 by default. Holds the store,
  the supervisor, and the API; becomes `crates/server` when step 4 splits the envagent
  out, which `docs/roadmap.md` § Implementation order sequences.
- `frontend/` — Next.js App Router, React, TanStack Query, Tailwind.
- `docs/` — the governing specs. Read the relevant section before implementing.
- `scripts/` — install and deployment tooling.

`docs/` was rewritten from first principles on 2026-08-27 and the code has since
followed it through step 3: `protocol` and `adapters` exist, adapters are linked into
the server rather than spawned, and `POST /sessions/adopt` reads native stores.
`crates/envagent` (step 4) is not started, so anything describing a paired remote
machine is still intent rather than description. Elsewhere, treat a disagreement
between `docs/` and the code as a bug in one of them and read § Spec and Code Evolution
before deciding which.

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

There is no adapter directory. Adapters are linked into the envagent, so the escape
hatch for "a vendor changed faster than the shipped adapter" is upgrading the envagent
on that one machine — which touches no server, no schema, and no other environment. The
old `SYLPH_AGENTS_DIR` hatch carried a hazard of its own: copying a bundled adapter into
it froze that adapter at install time and broke the next upgrade.

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

The journal is always `journal.db` inside the data dir, with `history.db` and
`blobs/` beside it; all three are derived rather than separately configurable,
because they are one store split for SQLite's sake. **No `live.db` rename runs on
startup** — `3dff4e5` deleted it as compatibility code for an older on-disk layout,
having first verified `~/.sylph/data/` held no such file, so nothing was at risk.
The checkpoint-then-rename *rule* survives that deletion and is below under
Pitfalls, because the envagent's `spool.db` is WAL-mode too and will need it.

**There is no archive binary and no setting naming one.** History is one store the
server owns, so `tracedb.bin`, `SYLPH_TRACEDB_BIN`, its `doctor` row, and
`tracedb.available` are deleted together — a setting naming an external binary is only
meaningful while an external binary answers, and keeping it offers a lever that changes
nothing. What replaces "is the archive reachable" is **per-environment health**, which
is a better question: it distinguishes a missing CLI from a sleeping laptop from an
exhausted sandbox node.

Retention's authority moved with it. The only downstream copy is now the vendor's own
native store, so a confirmation comes from the envagent that holds it, and an
unreachable environment means the question was never put — recorded as withheld, never
as a zero bound.

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
scenario in `crates/adapters/src/e2e.rs` spaces its stages a
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

**Deleting a route leaves `.next/dev/types/` behind, and the stale copy fails the
type check.** `npm run build` reported `error TS2307: Cannot find module
'../../../app/search/page.js'` after that route was deleted — naming a module that
no longer exists and that no source file references. The generated validator in
`.next/` still declared it, so the error describes the build directory rather than
the code. `rm -rf .next` and rebuild. Worth recognising because the message points
at a path in `app/`, which reads as an unfinished deletion rather than as a cached
artifact, and because `.next` is gitignored so the state is invisible to `git
status`.

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

**Zero agents is a silent, total failure that reads as a UI bug.** With none available,
no session can be created and every existing one reports `{"attached":false}` — so
`commands` is `[]`, and the composer's slash menu, which required a non-empty list to
open at all, made `/` a dead keystroke. The cause is three layers from the symptom.
Three things make it legible: the startup log says so at `error` level, `/health`
reports `agents.available`, and `LinkState` carries a typed `failure` so the composer can
say *which* of "still connecting", "agent not installed", or "this agent has none" it is.
Check `agents.available` before believing an empty composer is a frontend bug.

The E2E fixture is a compiled-in adapter behind the `e2e` cargo feature, and the
exclusivity is load-bearing: a build offering both it and the real three would let a spec
pick `claude` and spend real tokens. So `Agents this build can run: e2e` in the backend
log is what confirms the harness is testing the fixture — a suite failing every
session-creating spec is worth checking there before reading it as a product bug.

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

This is the most-repeated defect in this file, and every instance compiled and
surfaced one or two layers from its cause. The instances are catalogued
individually below (`pi`'s `set_model`, the invented `task_*` subtypes,
`AskUserQuestion`'s `selectedOptions`, `requestedSchema` versus `requested_schema`,
`pi`'s `agent_start` versus `agent_settled`, reading `response` off an *error* envelope,
and `pi`'s `message.timestamp` read in the wrong *unit* — `docs/agents.md` § `pi`)
because the *specific* keys are what a reader needs. The general rule is the one above,
plus three shapes worth knowing before writing any adapter:

- **The right key in the wrong unit fails the same way, and is harder to catch.** The
  `pi` timestamp instance was not a misspelling: an integer read from the correct key
  and divided as though it were microseconds yielded a plausible number that decoded to
  1970, on 3880 records. A wrong key yields absent, which a presence check finds; a wrong
  unit yields a *value*, which passes every such check. Establish a payload's units by
  comparing the number against a sibling human-readable stamp, not by eyeballing its
  magnitude.

- **"It parses without error" is not evidence a parser reads a store correctly, and
  measuring the wrong thing here is cheap comfort.** Measured 2026-08-28 while scoping
  the parser port: trace-db's three parsers processed all 4986 real sessions across the
  `claude`, `codex`, and `pi` stores with **zero** parse failures — while `claude`'s tool
  results all carried a null call id (the id is on the content block, and the parser
  looked in four other places that occur zero times in 3226 files), all 2100 real tool
  failures parsed as successes (`is_error` is only ever on a `user`-role block, and the
  parser read it on the assistant branch), and `codex`'s single most common current
  record was discarded outright. A store-reading defect does not raise an error, because
  a missing key is indistinguishable from a field the vendor omitted — it produces a
  plausible record. So verify a parser by asserting on *values it should have found*
  against a payload you have printed, never by observing that a run completed.

- **A census of a vendor's keys must walk the parsed structure, never grep the
  serialized line.** Measured while scoping the `pi` parser port: a substring test for
  `"parent_id"` over serialized records also matches those words appearing inside
  message *text*, and agent transcripts discuss field names constantly — so the census
  reported keys the records did not have, and inflated the very count the port branches
  on. This is the defect class appearing in the *measurement* rather than in the code,
  which is worth naming because a wrong measurement is harder to distrust than wrong
  code: it produces a number, and a number reads as evidence.

  The same pass showed why a vendor's own version field is not a shortcut. `pi` stamps
  `"version": 3` on all 527 files across both spelling eras, so it discriminates
  nothing; and because that store's rename landed *field by field*, there is no single
  era per file to detect at all. Resolve such a spelling per field, and fail loudly on a
  combination no known era emits — a per-file era flag is unsound rather than merely
  coarse, and it passes on 462 of 527 files, which is exactly long enough to be believed.

- **A reproduction is a second implementation, and a second implementation of the thing
  under test is not evidence about it.** *Measured while chasing a scripted-peer failure in
  `codex`'s tests:* the peer was reproduced in Python to inspect it, where `json.dumps`
  preserves insertion order while `serde_json` sorts keys — so the reproduction disagreed
  with the artifact it was reproducing, and three wrong diagnoses followed from trusting it.
  Printing the actual bytes settled it in one step.

  This is the sibling of the census rule above, and the same reason applies: the defect is in
  the *measuring* instrument, so it produces a confident answer rather than an error. When a
  reproduction and the real thing disagree, the reproduction is the more likely to be wrong,
  because it was written from an understanding of the artifact rather than from the artifact.
  Read the bytes, the payload, or the file before building a model of it.

- **A type in a vendor's event union is not a promise that anything emits it.** Grep
  the implementation for emit sites, not the types for declarations — a declared-only
  event ships an adapter branch that never fires, and a branch that never fires is
  indistinguishable from one whose condition is simply rare.

  This file previously claimed `pi`'s `agent_start`/`agent_end` were such a case, with
  zero emit sites. **That was wrong**, re-measured against pi 0.84.3 from its
  unbundled `dist/` tree: `agent_start` is emitted at `pi-agent-core/dist/agent-loop.js:49,67`
  and `agent_end` at `:110,157,172`, and both reach the RPC wire, because
  `dist/modes/rpc/rpc-mode.js:265` funnels every `session.subscribe` event through
  `output(toJsonEvent(event))`. A measured turn emits `agent_start, turn_start,
  message_start, message_update×N, message_end, turn_end, agent_end, agent_settled`.
  The original investigation read a minified bundle; an unbundled source-mapped
  install is available at `~/.local/lib/node_modules/@earendil-works/pi-coding-agent`
  and is far better evidence. Prefer it, and note that "I grepped and found nothing"
  is a claim about your grep as much as about the vendor.

  What remains true is the *adapter's* choice: it closes a turn on `agent_settled`
  rather than `agent_end`, because pi can emit `agent_end` and then continue (see
  `crates/adapters/src/pi.rs`). That is a real distinction between two events that
  both exist, not evidence that one of them does not.
- **An SDK object's attribute names and its JSON wire names are different
  namespaces.** The MCP spec says `requestedSchema`; the Python SDK's dataclass field
  is `requested_schema` with the camelCase form as a Pydantic *alias*, and `claude`
  passes `__dict__` without `by_alias=True`. So the spec-correct key reads `None`
  against a spec-compliant SDK. When the payload came from an SDK rather than off a
  socket, read the Python side.
- **Ask the binary.** Grepping the 321 MB `claude` binary for a key is cheap and
  decisive: `selectedOptions` occurs nowhere in it, which would have caught that
  instance before it shipped.
- **A success and a refusal are different shapes, so a reader for one silently
  swallows the other.** `claude` answers a refused control with `subtype: "error"` and
  **no** `response` member, and the adapter read `response.get("response") or {}` — so
  every refusal became the same empty value a successful no-payload control returns.
  Selecting `bypassPermissions` was refused by the CLI, recorded as applied, answered
  200, and rendered as the current mode while every tool call went on prompting. Read
  the discriminant before the payload; `{}` must never be reachable from two opposite
  outcomes.

**A vendor's `success` is not a promise it did what you asked.** Two facts, both
measured on `claude` 2.1.246, and neither visible to a caller that only checks for an
error. `set_permission_mode` to `bypassPermissions` is **refused outright** unless the
process was launched with `--dangerously-skip-permissions` or
`--allow-dangerously-skip-permissions` — the latter marks the mode *available* without
starting in it, which is what a user selecting it later actually asked for. And
`manual` answers **success** while confirming `default`: the vendor normalizes it
silently, so a picker offering it lands on a different row. So an adapter records
`currentValue` from the vendor's *confirmation*, never from the request, and offers
only values it has watched the vendor apply as given. Writing the requested value made
the adapter the authority on a fact only the vendor holds, and the surface then
reported a policy the session was not running under.

**A second vendor confirms it, and adds the worse case: a value outside the enum is
accepted and quietly means "no".** Measured on `codex-cli` 0.150.1. Its approval decision
enum is `accept` | `acceptForSession` | `decline` | `cancel`; sending `"approved"` — a
plausible guess, and not in the enum — produced **no error at any layer**. The JSON-RPC
reply was accepted, the turn reported `completed`, and the command silently did not run.
Measured three ways: `accept` gave `status: completed` with `exitCode: 0` after 72 ms of
real execution, `decline` gave `declined`, and the bogus token gave `failed` in 3 ms. So
an adapter that guesses this token ships an approval channel that refuses **everything**
while every observable layer reports success, and the only difference is a status two
levels down inside an item. Read such an enum out of the vendor's schema; never infer the
word from what it obviously ought to be.

The same CLI is *inconsistent* about which fields it validates, which decides where
validation lives. Enum-typed fields (`approvalPolicy`, `personality`) refuse a bad value
and list the valid ones — those map to `INVALID_VALUE`. But `effort` is typed as a bare
non-empty string, so `"banana"` is accepted, **echoed back as the current value**, and
fails only when a turn runs; a nonexistent model behaves the same; and an unknown option
name is accepted silently, so the vendor offers no `UNSUPPORTED_FIELD` signal at all.
Anything not enum-typed in the schema must be validated adapter-side against what the
vendor enumerates, or a refused write is indistinguishable from an applied one.

**And the silence is ambiguous, which is what makes adapter-side validation load-bearing
rather than defensive.** *Measured over all 11 `thread/settings/update` calls in
`docs/measurements/codex/captures/s04_settings.jsonl`:* 8 were accepted, and only **6**
produced the `thread/settings/updated` notification that carries the refreshed surface.
The two silent ones are `noSuchOption: "x"` and a `personality` already current — so an
option name the vendor has never heard of answers exactly like a legitimate no-op: empty
reply, no notification, no error. A caller cannot tell them apart at any layer, so a
typo'd option id would be reported as applied. The adapter refusing an id outside its own
enum is the only thing that catches it.

The reply itself carries nothing to fall back on: `thread/settings/update` answers `{}`,
and the refreshed surface arrives *only* as that notification. **`thread/read` is not the
escape hatch, and its shape is a trap.** Its `result.thread` **omits** `model`,
`reasoningEffort`, `approvalPolicy`, and `sandbox` entirely — absent, not null — while
`modelProvider` *is* present. So a reader glancing at that object sees a settings-looking
field and concludes the rest are there, and a presence check written to confirm it
confirms the wrong thing.

Two consequences for how such a wait is built. The notification is fast — measured at
**0.7 to 5.4 ms** on local stdio across every notifying write — so a multi-second timeout
is a thousandfold overestimate. And because a no-op notifies nothing, **the timeout is the
expected path** rather than a degradation: it is what a user re-selecting the current value
hits every time. A wait sized for a network round trip therefore charges that user the
whole of it, and code that treats the timeout as an error reports a failure for a write
that was correctly a no-op.

**The third vendor adds the third variant: it neither refuses nor accepts, it silently
substitutes.** *Measured on `pi` 0.84.3, `agent-session.js:1290`.* `setThinkingLevel`
answers `success: true` for a level the model does not support, having replaced it with
the nearest one it does — the method's own doc comment says "Clamps to model
capabilities". So the three vendors now cover every way a write can not-happen: refused
outright, accepted-and-ignored, and honoured-as-something-else. Only the first is visible
to a caller checking for an error.

This is what makes a pre-flight check against the enumerated list **not** defensive
duplication, which is how it reads until you know a vendor clamps: it is the only place
the difference between "you asked for `high`" and "you got `medium`" can be caught, since
the reply says success either way. Re-reading the surface afterwards catches it too, and
both are worth having — the pre-flight names the value the user chose, while the re-read
reports what is true now.

**And a flag's argument may be interpreted, not just passed.** `pi`'s
`--append-system-prompt` calls `resolvePromptInput` (`resource-loader.js:391`), which
reads the argument **as a file** when the string names one. A one-line identity preamble
that happened to be a real path would be silently replaced by that file's contents. So
the preamble goes to a temp file whose path is passed — not because the flag requires one,
but because passing text to a flag that may resolve it is a substitution nothing reports.
Check what a vendor does with an argument before deciding a string is safe to hand it.

**An adapter that writes a turn's outcome instead of reading it turns every vendor
failure into a silent empty reply.** *Measured 2026-08-28 on `pi` 0.84.3.* Its configured
model had reached the end of a testing period, so every request answered
`404: {"message":"Thank you for participating in the Stealth Ox Alpha testing period…"}`.
The observable result in Sylph was a turn that opened, closed `completed` with
`error_code` NULL, and produced **no assistant block at all** — a session that looks like
it worked and simply said nothing.

The cause was one line in the v1 adapter, inherited by the port: it emitted
`data={"stopReason": "end_turn"}` on `agent_settled`, hardcoded, while pi's own record
carries `stopReason: "error"` and an `errorMessage`. So the adapter asserted a clean
finish over a failed one. **The Rust port inherited it**, which is the part worth
noticing: a hardcoded constant survives a rewrite precisely because it compiles and reads
as a default. Both are fixed — the close now reads the vendor's own `stopReason`, and
`crates/adapters/src/pi.rs` pins it with a test that feeds a failing turn.

Three rules follow, and the third is the one that generalizes:

- **A turn's outcome is a vendor fact, so read it.** `stopReason` is reported; writing a
  literal makes the adapter the authority on the one thing only the vendor knows — the
  same error as recording `currentValue` from a request rather than a confirmation, above.
- **A failed turn must be distinguishable from an empty one.** `error` and `aborted` are
  real `stopReason` values and both are failures. The vocabulary already has somewhere to
  put the vendor's sentence: `EventKind::Notice` is "agent-originated notification worth
  surfacing", and `Turn`'s `completed` carries the stop reason.
- **Silence is the hardest outcome to debug, so nothing may produce it by default.** A
  turn with no content and no error says the same thing whether the model 404'd, the
  agent chose not to answer, or the adapter dropped the stream — three causes with three
  different fixes and one appearance. Any path that can end a turn without content has to
  say *why*, or the first question anyone asks has no answer in the record.

**A reply that waits for the work to finish must not be awaited as an acknowledgement.**
*Measured on `pi` 0.84.3:* `abort()` calls `await this.waitForIdle()` before answering
(`agent-session.js:1171`), so its reply arrives when the turn has actually wound down —
which for a running tool is however long that tool takes to notice. Treating it as a
request/reply pair ties `interrupt` to that duration and, past the 30-second reply
timeout, reports a **failure for an abort that worked**: the user sees their stop fail
while the turn is stopping. Sent fire-and-forget instead, and no close is synthesized
either, because the vendor emits `agent_settled` from the `finally` of its prompt loop
(`:758`) — so the outcome is reported rather than assumed, and `aborted` stays
distinguishable from a failure.

The general rule, which applies to every control an adapter wires: read whether the
vendor's reply means *accepted* or *done*. The two need opposite handling, and a reply
that means "done" looks exactly like a slow acknowledgement until the timeout decides
what it was.

**A launch flag gating a runtime-mutable value bets the user never changes it.**
`--permission-prompt-tool stdio` can only be asked for on the command line, while the
mode it serves changes through `set_permission_mode` — so gating it on the *launch*
mode lost the approval channel permanently to anyone who switched. Measured: launch in
`bypassPermissions`, switch to `default`, and every tool call returns `Claude requested
permissions to use Bash, but you haven't granted it yet`. A session started in a
bypassing mode could never return to an interactive one, which is worse than the bug
that led here. Register such a channel unconditionally; a mode that decides alone
simply never consults it.

**One refusal code is not enough, and the server was erasing the difference.**
`UNSUPPORTED_FIELD` means "no such option" and justifies disabling a control;
`INVALID_VALUE` means "this value, right now" and must leave it usable. Both adapters
already drew that line and `api::links` flattened every adapter error into
`Error::Unsupported` — whose own doc comment says a client may disable the control over
it. So one refused permission mode would have retired a picker whose other five values
work. The code now travels as a typed `AdapterError` through `anyhow`, recovered with
`downcast_ref`, deliberately **not** by parsing it back out of the message: the text is
the adapter's and may be reworded, while the code is declared protocol. Same reasoning
as the degraded-dependency rule below: the flag is the client's own record of having
spoken, never a string match on somebody else's message.

**serde's `rename_all` does not rename fields inside enum variants.** An internally
tagged enum needs `rename_all_fields` as well, or camelCase wire fields inside a
variant parse as `None` while the code compiles and every non-variant field works.

**A clean `git merge` can still lose code.** Both sides adding methods to the same
object literal merges without a conflict marker and silently keeps only one set —
this happened to `api.files` during the Phase 7 merge, dropping two client methods.
Nothing in git reports it; only `npm run build` did. Run both sides' full checks on
a merge that reports itself clean, especially the type check.

**Two id spaces for one conversation is silently always false.** *This one is now
structurally impossible — identity is `(environment, agent, native_id)` throughout — and
it is kept because the failure mode is worth recognizing in any new subsystem that talks
to an external store.* Historically: Sylph's session id and the archive's were disjoint, and confusing them was
silently always-false.** Sylph keys a session by UUID; the archive keys the same
conversation as `<agent>:<native-id>` (`claude:9bdacb6e-…`). Comparing the two
produces no error at any layer — the archive answers "no such session", the caller
concludes nothing is confirmed, and every component behaves impeccably while the
feature does nothing. This is how the first `TraceDbAuthority` shipped fully written
and tested, pruning nothing, with an empty `watermarks` table that looked exactly
like correct conservatism. The translation helpers both directions needed are gone with the second id space; a type that needs the archive
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

**Deleting a layer orphans its bounds, and the surviving half is what hides the missing
one.** The subprocess transport carried two timeouts: ten seconds for the handshake and
thirty for any command. Removing it moved the *command* bound down into
`crates/adapters/src/process.rs` — where a vendor RPC genuinely needs one — and dropped the
handshake bound entirely, while `Adapter::probe` spawns `<cli> --version` and awaits it with
no deadline. So a wedged vendor binary would hang `POST /sessions` for as long as the client
waited, on a path that used to fail in ten seconds.

What made it easy to miss is that the obvious check passes: *"did the timeouts survive the
deletion?"* answers **yes**, because one of them did, in a file whose name suggests it owns
process concerns. When a layer is deleted, enumerate the bounds it held **one at a time** and
name the new owner of each; a bound with no owner is not a bound, and "the timeouts moved" is
not an answer about any particular one.

**A degraded dependency is one fact about the run, not one fault per item — and not
one per attempt either.** With the dependency absent — a supported configuration — probing per
session turned one missing binary into a warning per session per pass. Probe once per
pass. But that is only half of it: the *spawn* failure was also logged at `error` on
every attempt, so a timer-driven caller reproduced the same unbounded stream one
layer down — visible in the E2E log, which is exactly such an install. An absent
dependency is a persistent state, so whatever holds the connection reports it once at `error` and
at `debug` thereafter. Any client polled on a timer needs that shape; the fix belongs
in the client, not in each caller.

The related rule: "cannot ask" must never be recorded as "asked, and nothing is
safe", or a watermark claims a check that never happened.

**A global "is the pipeline healthy" signal cannot replace a per-session bound.** A
monotonic sequence per ingest *run*, committed after the data transaction, is never a false
positive — and still says nothing about which events of which session are covered, so it
cannot answer "is this session's history safe to drop". A per-session commit marker is both
cheaper to obtain and more precise, and a fingerprint of the source it was derived from says
*which version* of that source the marker covers. This is why retention's watermark is
per-session (`docs/storage.md`) rather than a pass-level health flag.

**Renaming a WAL-mode SQLite file means checkpoint-then-rename, not moving three
files.** A crash between renaming `-wal` and renaming `.db` leaves committed data in
an orphaned WAL beside a database that no longer references it — silent loss of
exactly the turns that were in flight. `PRAGMA wal_checkpoint(TRUNCATE)` first, close
every connection, then move the main file alone. The emptied sidecars must also be
deleted, or the next database opened at that path adopts them. Nothing in this repo
performs such a rename today — the one that did was deleted as compatibility code —
so this is recorded for the envagent's `spool.db`, which is WAL-mode and will.

**A cross-process JSON boundary lets a spelling change read as `None`.** *Deleted with
the separate archive — the boundary is now a Rust type, so a rename is a compile error.
Kept because the rule applies to every remaining vendor payload (`docs/agents.md`).*
The archive's spellings were inconsistent per operation: `show` emitted
snake_case (`created_at_ms`, `started_at_ms`), while `stats`, `search`, and
`list` emit camelCase (`totalSessions`, `startedAtMs`). The client pinned the exact spelling each operation emitted so
a spelling change failed a test rather than silently yielding `None`. Do not add
dual-spelling aliases: they mask future renames and suppress parse errors that
would catch breaking changes.

**No vendor's native store holds event-level delegation structure, and a field that
looks like a span is not one.** *This survives the move to one store unchanged: it is a
`fidelity` fact about vendor files, not a fact about any archive.* A live turn carries
`span` on every event; nothing read back from a native store does. `data_json`
is the only field where a raw vendor payload could survive — where `claude`'s own
`parent_tool_use_id` would be — and it is **null on all 36308 events sampled across
all five agents**. `parent_id` is the field that looks like a span and means
something different per agent: for `claude` it is the `parentUuid` conversation
chain (13689 of 15119 pointers name the immediately preceding event, fanout 1), for
`pi` it mixes that with grouping parts under a message (fanout 1–3), and
`codex` never sets it. Honouring `claude`'s as a `span` nests a whole
transcript into one chain thousands deep. Its only real use is pairing a
`tool_result` to its `tool_call`. Attribution to the *specific* delegating call was
measured and rejected too: `claude` names a child `<parent>/agent-a<name>-<hash>`,
but **1918 of 2104** suffixes are hash-only, so matching recovers 8.8% and
misattributes on a collision — structure that is right for one delegation in eleven
is worse than an honest flat view, because nothing on screen distinguishes them. So
archived delegation is session-level in both directions (`parentId`, `delegations`),
and the UI must say the archive records no link to the exact call.

**A count repeated in several places is several chances to drift, and nothing checks them
against each other.** One population's size was carried in four passages here and two
disagreed with the other two — a different figure *and* a different agent — so at least one
pair had always been wrong while reading as measured fact. Neither could be re-derived,
because the store they were measured against no longer exists. Three rules: state the
*shape* a piece of code depends on rather than a ratio, since the shape is what stays true;
where a number is worth quoting, cite it once and point at it from everywhere else; and **a
measurement is only quotable while the thing it was measured against still exists.** This
file's own vendor-key instance counter had drifted to five different values across five
files before it was deleted rather than corrected.

**A path filter must be a path match, not a substring match.** A store that compiles one to
`cwd LIKE '%' || ? || '%'` answers `/w/app-old` and every subdirectory when asked for
`/w/app`. Anything meaning "this one directory" must compare for equality after normalizing
trailing separators — `sylph_protocol::normalize_workdir` is that comparison, shared by the
adapters and the server so the two cannot disagree, and `docs/database-schema.md` carries
the rule for Sylph's own queries.

Two consequences worth keeping, both measured against real stores and both still true of
the native stores the adapters read:

- **An unanchored filter can starve a paged scan.** A parent directory's pattern matches
  everything beneath it, and those sort by recency rather than depth — so a directory's
  own first exact match can land a thousand rows in and an early page cap reports it
  empty. Bound such a scan in rows examined, not pages.
- **A subagent often runs in a different directory than its parent** — typically a
  worktree or a subdirectory. Folding those into a parent that is not in the directory
  being listed deletes them from the only listing that would show them: it emptied 42
  directories, one holding 193 sessions. A cross-directory child is a row of its own,
  because in its own directory it is not delegated work, it *is* the work.

**`parent_session_id` and `forked_from` are different relationships**, and a listing that
treats them alike is wrong in opposite directions. A subagent folds into its parent as a
count. A fork replays its origin's whole prefix and continues past it, so the two share
every early user turn and read as one conversation resumed — listing both shows it twice
under one label. Which one should survive cannot be decided in advance: of two real cases,
one had its later work in the fork and the other in the origin.

**The `/agent-` id suffix is not a reliable lineage signal.** It catches most subagents and
misses those recording only a parent id — `codex` among them, measured from its own store: a
`rollout-…jsonl` with `parent_thread_id` set, `thread_source: subagent`, and no `/agent-`
segment. Read the vendor's own recorded parent; the suffix is a fallback for when that
lookup fails.

**The first `user` event is usually not what the user typed.** Agents inject their
own context as leading pseudo user turns (`<permissions instructions>`,
`<environment_context>`, `<skills_instructions>`, memory files as
`# AGENTS.md instructions`, Claude's `Caveat:` preamble). Taking the first user
event verbatim labels most sessions with boilerplate that is byte-identical across
every session of that agent. Skipping injected envelopes, which over
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
sessions; `codex` and `pi` record none. Any surface showing an archived title needs
the opening-ask fallback, or most rows read as untitled.

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
provider separator. The most expensive instance of the vendor-key pitfall above: the
refusal came back as a failed RPC that nothing surfaced, so a correct picker looked
broken and the investigation went to the UI twice before reaching here.

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

**A placeholder written into durable data cannot be withdrawn by fixing the
producer.** `claude`'s adapter sent the literal `"Subagent"` as a delegation's `name`
whenever its brief had not finished streaming — the usual case at `started`.
`history::consolidate` keeps the best name any phase gave a block, so a later `delta`
with the real brief normally wins; but when the brief never finishes, because the
delegation fails early or the link dies mid-input, the guess becomes the trajectory's
durable name in `history.db`.

The bound on the fix is worth stating exactly, because it sounds better than it is:
**rows already journaled with the literal are permanently indistinguishable from a
genuine name.** There was nothing to migrate not because the data is fine but because
nothing can tell the two apart. So a historical `"Subagent"` row is not evidence that
an agent chose it, and any analysis over archived names has to treat those as
unknowable rather than as data. The general rule: a field meaning "what this is
called" must be absent when unknown, because a journal makes every guess permanent —
and `Option`/`null` is the only value that stays honest after the fact.

Landing such a fix has an order. Renaming a *client* fallback while the adapter still
sends the literal gives one condition two labels, which is worse than one imprecise
label; and the subtraction is safe alone precisely because the client already falls
back to the same string, so the rendered output does not change. Producer first,
presentation second.

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

**A test that catches a race by luck stops catching it silently, and a later fix to that
test is the likeliest thing to retire it.** Measured on `codex`: a test landed a frame in a
window where the read loop dropped it, failing roughly one run in five. The fix to the
*defect* was right; then the test's own shell escaping was fixed by moving its payload to a
file — which removed the timing that made it fire. Re-measured afterwards with the defect
deliberately restored: the test passed 8 of 8, the whole suite 3 of 3, and 3 of 3 again
under concurrent load. Coverage of that property had gone from intermittent to **zero**, and
the commit that did it cited 15 consecutive passes as evidence. The passes were real and
evidence of nothing.

Two rules, and the second is the general one:

- **A property observable only through timing needs the window opened on purpose.** The
  replacement peer emits the frame, *sleeps*, and only then answers the open, so the loop
  provably sees it in the bad state. It fails 10 of 10 with the defect restored, and its
  failure message names the cause. Same rule as "testing it streams needs a runner slower
  than one render", one layer down.
- **Before changing the mechanism a test depends on, ask which property each assertion is
  standing on.** Two assertions in one test can rest on two different mechanisms, and
  repairing one can retire the other with the suite staying green throughout. Split them so
  they fail for different reasons — verified here by restoring the defect and watching the
  new test fail 10/10 while the one beside it passed 6/6, which is what proves neither is
  standing in for the other.

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
which is what shipped. `crates/adapters/src/claude/translate.rs`'s unit tests pin it, and
exist because a live run cannot: it spends real tokens and only sees whatever shapes one turn
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
leaves the container open, the same spinner one level up. Another instance of the
vendor-key pitfall above: print one real payload instead of inferring field names.

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

The repair landed on the trajectory row and **not** on the tool row beside it, and that
went unnoticed for a reason worth naming: a defect whose only symptom is a race is
invisible to whichever fixture happens to win the race. Restoring the broken
`useState(defaultOpen)` on the tool row leaves `timeline.spec.ts` passing 13/13, because
the current fixture delivers a `tool_call` and its `tool_result` close enough together to
land in one render. So a spec asserting the *outcome* of an auto-expand cannot tell a
correct implementation from a snapshot one, and a green suite is not evidence either way.
Two consequences: when a pitfall entry names more than one site, check every site rather
than the one that prompted it; and a fixture that wants to observe an
arrives-after-mount defect has to *delay* the second event, which is the same rule as
"testing it streams needs a runner slower than one render", one layer down.

**Announcing a thing's arrival and not its departure lets a client's view only ever
grow.** A pending request was published when raised and on no path when it left
`pending` — not on `resolve`, not when a dying link abandoned it. So the set a client
believed it was blocked on was monotonic, and it shrank only when the 20-second
backstop poll happened to fire. That poll is the part that makes this severe rather
than merely late: `refetchIntervalInBackground` is false by default, so a backgrounded
tab was not polling at all, and `refetchOnWindowFocus` is off here, so returning to it
did not refetch either. A question answered on a phone therefore sat on the desktop
until the page was reloaded — reported exactly as "要刷新才会消失", and reloading
genuinely was the only reliable cure.

Three things make the fix hold rather than patching the one path. The publisher is
**one** method every transition calls (`LinkManager::announce_request`), because three
call sites each remembering to announce is three chances to forget — which is how this
arose. The payload is the **row**, not the transition, so a client reconciles against a
state (`status`) instead of replaying a vocabulary of verbs, and a status added later
needs no client change. And it is published **before** the forward to the adapter: the
claim is what makes the answer final, so a request answered against a *dead* link must
still retire everywhere — announcing afterwards would tie a display concern to a
delivery one and skip the case where the user most needs telling.

The related trap when testing this: re-reading `GET /requests` proves nothing, because
the *row* was always correct and only the notification was missing. Assert on the
session stream, and end-to-end use two browser **contexts** rather than two tabs — two
tabs share a cookie jar and can pass while a genuinely separate client still hangs.
Keep such a spec's timeout well under the poll interval, or it passes on a foregrounded
tab against the bug it exists to catch.

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

**And `Stop` disappearing is still the wrong signal for anything the settle *sweep*
produces, which is the same rule one layer out.** `Stop` is gone once the turn is
**terminal**, and that is not the same claim as the turn being **fully settled**.
`TurnCloser::close` writes the terminal status first (`closer.rs:58`) and closes open
trajectories ~35 lines later (`:93`), with a session-status write between — deliberately,
because the row that makes the next action legal must be written first. The client's
`finished` polls turn status, so `Stop` vanishes the moment that first row commits, and a
spec waiting on it resumes *before* the sweep has emitted anything.

Every spec asserting on what the sweep produces — a stranded delegation closed, a
descendant status escalated, a rollup counting a subtree — therefore races it, and under
load whichever one loses is the one that fails: `:137`, `:373`, and `:348` in
`delegation.spec.ts` have each been the failing one on different runs of the *same* tree.
That is what makes it read as flakiness, and worse, as a regression on whichever branch
happened to be under test. It was diagnosed as a branch's regression once here, and the
branch was innocent: the spec fails on `main` identically.

Wait on something the sweep itself produces, not on `Stop`. Moving the sweep before the
status write is the other option and it is worse — it fights the ordering rule above, to
fix a test-observability problem.

**An OOM-killed suite is indistinguishable from an interrupted one, so the machine is the
last thing anyone suspects.** A suite killed for memory reports **zero failures at the
point of death** — exit 137 or 143, no summary line, every spec green up to the cut. The
instinct is to blame the harness or the tree. Measured here across three runs that died at
specs 119, 35, and 34 of 156 with 159 MB free, against this file's own record of this suite
dying at ~62 MB: two of the three were self-inflicted (a `Monitor` until-loop SIGTERMing the
run, and an `rm -rf .next` in the worktree the server was serving from), and the third was
genuinely the machine. Check free pages before diagnosing anything else, and run in groups
rather than raising a timeout.

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
A live dialog check passed throughout, because it asserted that a dialog *arrived* and
that answering unblocked the turn — both true while the answer was being dropped.
Grep the binary for the key and round-trip one real payload; and assert on the tool
result the model actually reads, since "the panel closed" and "the answer arrived" are
different claims and only the second is the feature. Another instance of the
vendor-key family above.

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

**Killing a subshell does not kill what it exec'd, and the survivor answers on the
port.** `scripts/measure-bundle.sh` served a route to measure it, backgrounded
`npx next start` in a subshell, and killed the subshell — leaving `next`'s exec'd
child holding port 3199. The *next* run then measured that server, which serves its
own older build, so the served HTML named chunk hashes the current build never
produced.

It surfaced as `referenced chunk is missing from the build`, which points at the
build. Three theories died before the right one: a stale `.next`, a rebuild
mid-serve, and a `next/dynamic` boundary. The chunk existed nowhere in `.next`
because it belonged to a build that no longer existed. `lsof -ti tcp:PORT` is the
one-command answer, and worth reaching for early whenever output describes a tree
that does not match what is on disk — it is the network-facing sibling of the
mid-write phantom-error pitfall below.

**This bit twice in one session, the second time as an unrunnable E2E suite.** A
backend surviving on 4318 from an interrupted run makes `npm run e2e` fail before any
spec, because `reuseExistingServer: false` — correctly — refuses to trust it. Playwright
names the cause plainly (`is already used`), so the cost there is only that a run
looks like a product failure until the message is read; the expensive version is the
one that *succeeds* against the wrong server. Before diagnosing either, clear the
suite's ports:

```sh
for p in 4318 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
```

Any script in this repo that starts a server to measure or test something needs the
process-group kill, the busy-port refusal, and an error that names the survivor. Two
of the three are only discoverable after the fact.

Three things together fix it, and any one alone still fails silently: run the job in
its own process group (`set -m`) and kill the group; refuse a busy port instead of
measuring whatever is on it; and make a missing artifact explain the likely cause
rather than only naming the file.

**A default that is one machine's absolute path is wrong everywhere else.**
`TRACEDB_BIN` defaulted to `$HOME/Workspace/trace-db/target/release/trace-db` — a
sibling checkout that exists on exactly one machine, so every other install got a
silently unavailable archive that looked like a dependency's own bug. A dependency on a
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
arriving through the one component the check could not see. The same shape appeared on
the read path a day apart: a handler canonicalized for the check and then opened the
*original* path, so a symlink inside the workdir pointing out of it passed and was
followed. The rule covering both is **derive the path you operate on from the check
itself** — canonicalize once, into one variable, and use that variable; a
`contain_in_any` result paired with a path it did not produce is not a bound. `remove` was already
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
exist. This held for `trace-db` too while Sylph depended on it; that dependency is deleted
(`docs/architecture.md` § One authority), so the rule now applies to this repo alone.

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
