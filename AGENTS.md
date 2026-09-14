# Sylph Development Guide

Sylph is a coding-agent workspace: a server holding the authoritative record of every
session, an envagent on each machine that runs agents, and a web frontend as one
transport onto the server.

This file holds rules and facts that stay true as the project moves. `docs/roadmap.md`
owns progress and status; `docs/` owns design.

## Product Invariants

- **There is one class of session.** Sylph does not divide sessions by who started them.
  The one behavioural flag is `inject_identity`, settable at creation and refused
  afterwards, because a transcript records what produced it. Nothing resets a session,
  which would replace its identity mid-life.
- **Identity is `(environment, agent, native_id)`**, encoded with `:` as the delimiter.
  There is no Sylph-minted UUID, so there is one id space. `environment` is an opaque id
  minted at pairing.
- **What varies is the link, not the session:** two typed facets, `observe` and `control`,
  each able to report unavailable with a reason. An empty capability surface means Sylph
  cannot reach the session. **A facet describes the environment and the vendor, never the
  caller** — one session has one link state and every observer sees the same one, which is
  what makes a facet reason cacheable. A Subject's own authorization is a separate field,
  never cached.
- **`fidelity` is a property of the record** (`stream` | `blocks` | `messages`). A
  renderer reads it before drawing a duration.
- **The server's store is the one authority, with one event vocabulary.** Native history
  is parsed by an adapter into the same events a live turn emits: one normalizer, one id
  space, one archive.
- **The envagent dials outward to the server**, and adapters run inside it — on the
  machine whose vendor files they read. Execution always goes through an envagent,
  including on the server's own machine, so a single-machine install is two processes.
- **Sessions do not migrate between environments.** Moving work elsewhere is a fork.
- **Widening `browse_roots` widens where arbitrary code may run.** It bounds what
  `/api/v1/fs/*` may enumerate *and* what a client may name as a session's `workdir`,
  which is the `current_dir` of a spawned agent. It belongs to the envagent, since only
  the machine holding a path can canonicalize it.
- **Bind to loopback.** A non-loopback deployment requires a separate authentication and
  threat-model change, covering who owns an envagent.
- **Three agents: `claude`, `codex`, `pi`.** Which of them an environment can run is a
  per-environment, time-varying fact discovered by that envagent.
- **Khora is an external sandbox project.** Its source of truth is the checkout in the local
  OrbStack `sbx` microVM, mirrored by [TSHOGX/khora](https://github.com/TSHOGX/khora), not this
  repository. Changes at the Sylph–Khora seam may require coordinated work in both checkouts;
  read [`docs/environments.md`](docs/environments.md) § Sandbox-provisioned (Khora) before
  implementing that boundary.

## Orientation

- `crates/protocol/` — the event vocabulary and the adapter contract, as types. No
  runtime: no `tokio`, no `sqlx`, no `axum`, and no `chrono`.
- `crates/adapters/` — the three adapters. Each owns whatever process its vendor CLI
  needs, and reads that vendor's native store.
- `crates/envagent/` — local framing, durable spool, reconnect epochs, path containment,
  credential storage, and adapter hosting.
- `crates/fts5-jieba/` — the SQLite FTS5 tokenizer, linked into the server so Chinese is
  searchable. Vendored rather than depended on by path, which is only ever right on one
  machine.
- `crates/server/` — Rust, Axum, SQLx. Holds the store, the supervisor, and the API.
- `frontend/` — Next.js App Router, React, TanStack Query, Tailwind.
- `docs/` — the governing specs. Read the relevant section before implementing.
- `scripts/` — install and deployment tooling.

**`docs/README.md` routes every question to its owning document.** Read it first; it is one
table, kept in one place, so a route cannot be right there and stale here. `docs/roadmap.md`
holds what is built and what is next, and `docs/architecture.md` holds the session model,
identity, and authority — those two orient everything else.

Record progress in commit messages and the roadmap. Do not add progress, handoff, or
summary documents. Carry-forward context belongs in the roadmap's open items; a durable
toolchain gotcha belongs below; reasoning about a specific decision belongs in a comment
beside the code.

## Running and Verifying

```sh
# rust — the whole workspace, not just the crate you edited
cargo run -p sylph-server
cargo test --workspace && cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps

# frontend
cd frontend && npm run dev
cd frontend && npm run build && npx eslint . && npm test && npm run e2e
```

Run the checks for every part of the repo you touched, not only the one you edited most.
Backend and frontend share a wire contract, so a change on either side can break the
other's tests.

The server runs nothing on its own: start an envagent and pair it
(`docs/development.md` § No environment connected).

E2E starts both servers itself on the ports in `docs/configuration.md` § Ports, with a
throwaway `SYLPH_HOME` and the `e2e` cargo feature, which compiles in a fixture agent
*instead of* the three real ones — so it never touches your store or spends tokens.
`Agents this build can run: e2e` in the backend log confirms the harness is on the
fixture. Do not start servers by hand for it; it shells out to `cargo`, and a cold run
compiles the backend first.

**Nothing the E2E launcher spawns may be `detached`, and that is a hang, not a leak.**
Playwright spawns the launcher itself detached with piped stdio and waits for its `close`
event, which needs every inherited pipe to reach EOF — so one detached child survives the
group SIGKILL, holds the write end open, and the run blocks *after* every spec has
passed. A handler cannot rescue it either, because the signal is SIGKILL. This cost three
commits of handler-shaped fixes before the mechanism was measured, and what removed the
temptation was building the binaries first and spawning them directly:
`cargo run` is a parent of the process you want to control, so killing it orphans the
binary, and reaching for a process group to fix *that* is what broke teardown.

```sh
# clear ports an interrupted run left behind, before diagnosing a startup failure
for p in 4327 4328 3101 3199; do lsof -ti tcp:$p | xargs kill 2>/dev/null; done
```

`npm run e2e` wipes `.e2e-home/` first, and the npm script is the only place that can.
Invoking `npx playwright test` directly skips the wipe and accumulates fixture state that
degrades into rotating timeouts, so run `node e2e/reset-home.mjs` before believing a
timing or ordering result from a direct invocation.
`docs/frontend-architecture.md` § E2E owns the rest, including how to write a spec that
fails for one reason.

### Pointers are checked, so prose cannot rot quietly

This file, `docs/`, and every comment cite each other three ways: a `path.md` § Heading
pointer, a backticked repo path, and a markdown link. All three are checked mechanically,
and a citation that resolves to nothing is a test failure in `cargo test --workspace`:

```sh
cargo run -p sylph-docs-check   # list what to fix, with file:line
```

It runs in the suite because rot has no other symptom — nothing reads prose, so a heading
renamed today misleads a reader months later with no failure in between. Renaming a
heading and running the suite tells you every citation of it. Prefer fixing the pointer to
loosening the checker: a shortened form is already accepted (a pointer may name the part
of a heading before its `:`, `,`, `—`, or `(`), and `fixtures/` is skipped on purpose,
since a vendor payload legitimately contains paths that are not ours.

## Configuration

Everything the server owns is under `~/.sylph` and configured in `~/.sylph/config.toml`;
an envagent has its own root, `~/.sylph-env`. Both are created on first start, including
under `cargo run`. Resolution is **environment, then file, then a default derived from
the root**, and nothing outside a root is consulted.

`docs/configuration.md` is authoritative for the settings table, the port allocation, and
what each value bounds. Three surfaces report what resolved, all from one producer:
`doctor`, `GET /api/v1/config`, and Settings → Configuration.

## Toolchain

**SQLx runs queries at runtime here, not at compile time**, so a SQL mistake surfaces in
the store tests rather than in the build (`docs/database-schema.md` § Schema evolution).

**`cargo doc` is a gate, and a cheap one.** An intra-doc link that resolves to nothing
fails silently — nothing else in the build mentions it. Two rules cover every instance
this repo had: a private item cannot be linked at all, so use a plain code span rather
than brackets; and in an inner (`//!`) comment paths resolve from the **crate root**, so
write the absolute path.

**`npm run build` is the TypeScript gate**, and ESLint is invoked directly (`npx eslint
.`) since `next lint` was removed in Next 16.

**Unpacked debug objects can make dependency discovery dominate Rust builds.** Repeated
macOS builds leave old `.rcgu.o` files in the dependency search directory. If `rustc`
is barely using CPU and spends time enumerating or dropping search-path entries, inspect
that directory before diagnosing a compiler hang. A fresh build directory confirms the
cause; generated build caches are separate from Sylph and vendor data.

**macOS ships bash 3.2, so `wait -n` does not exist.** The launcher generated by
`scripts/install.sh` polls both PIDs with `kill -0` instead. Any new shell script here
holds to 3.2 syntax.

**A clean `git merge` can still lose code.** Both sides adding methods to the same object
literal merges without a conflict marker and keeps one set. Run both sides' full checks on
a merge that reports itself clean.

## Where Design Rules Live

**A defect that produces a plausible value rather than an error is the expensive kind.** A
crash is found immediately; a wrong number, a silently dropped event, or a check that
cannot fail survives for months and is confirmed by whatever test was meant to catch it.
Every entry below is a family of that defect, owned by one document.

| Class of defect | Owner |
| --- | --- |
| Reading a vendor payload: wrong key, wrong unit, wrong shape | `docs/agents.md` § Reading a vendor payload |
| An inherited environment that redirects a vendor's authentication | `docs/agents.md` § A host-managed auth switch inherited from another agent session |
| A vendor's `success` that did not do what you asked | `docs/session-protocol.md` § Three rules about reading a vendor's answer |
| Delegation, `span`, promotion, `abandoned` vs `failed` | `docs/session-protocol.md` § Delegated trajectories |
| Retention, watermarks, withheld vs a zero bound | `docs/storage.md` § Retention is watermark-driven |
| Identity, one id space, no session class | `docs/architecture.md` § Identity |
| Why a turn stopped short: `CloseReason`, reason vs status | `docs/session-protocol.md` § Outcomes, and what a turn may close |
| Whether a fact is per-environment or per-caller, and what may be cached | `docs/architecture.md` § A facet describes the environment, never the caller |
| Who may authorize, and whether ungrantable means unreachable | `docs/api/admin.md` § `administer` is ungrantable |
| Path containment, symlinks, where agents may run | `docs/configuration.md` § `browse_roots` bounds where agents run |
| SSE framing, compression, and the streaming client | `docs/frontend-architecture.md` § Real-Time Event Streaming |
| Layout, viewport, ARIA, choice lists | `docs/ui-layout.md` |

## Rules That Keep Costing Time

These recur across subsystems and belong to no single spec.

**One fact with two derivations diverges the moment either grows a case.** Read the
field; never recompute beside it. The same holds for a count: cite a magnitude once and
point at it, and prefer stating the *shape* a piece of code depends on, since the shape is
what stays true. A measurement is quotable only while what it measured still exists.

**A wrong measurement is harder to distrust than wrong code, because it produces a number
and a number reads as evidence.** When a reproduction and the real thing disagree, the
reproduction is the more likely to be wrong — it was written from an understanding of the
artifact rather than from the artifact. Read the bytes.

**"It parses without error" is not evidence a parser reads a store correctly.** A missing
key is indistinguishable from a field the vendor omitted, so the result is a plausible
record. Verify by asserting on *values it should have found* against a payload you have
printed.

**A degraded dependency is one fact about the run**, not one per item or per attempt.
Probe once per pass and report an absent dependency once at `error`, then at `debug`. The
report belongs in whatever holds the connection.

**A subsystem whose correct behaviour is "nothing happened" needs a report**, or a working
pass cannot be distinguished from a broken one. Retention keeping every block is right for
a terminal session and a symptom for a backed one.

**Deleting a layer orphans its bounds.** Enumerate the bounds it held one at a time and
name the new owner of each; a bound with no owner is not a bound. The same applies across
endpoints: when a setting bounds one endpoint, enumerate every other endpoint consuming
the same kind of value and rank them by what the value lets a caller *do*.

**When a fix is "stop writing X", check what still writes the record.** An exception to a
producer can make its consumer's default the new bug.

**A placeholder written into durable data cannot be withdrawn by fixing the producer.**
Rows already journaled with a guess are indistinguishable from real values, so a field
meaning "what this is called" is absent when unknown. Where two layers both supply a
default, the layer that writes data stops first and the layer that renders changes second.

## Testing

Write tests for core business logic (session lifecycle, turn supervision, event
normalization), API endpoints (request validation, error responses, status codes), and
edge cases (concurrent access, process crashes, malformed input).

Name a test after the behavior it pins, so a failure reads as a broken promise rather than
a broken function. Assert on observable contract — status, error code, ordering — not on
incidental structure.

**A new test is only proven by watching it fail.** Write it, break the thing it covers,
confirm it goes red, restore. Otherwise it may be asserting something that was never in
doubt.

**Run the suite after the last edit, and paste what it printed.** Not "run the suite" —
after the *last* edit.

**Running one spec in isolation is a different measurement from running its file**, and
running its file is different from running the suite. A spec that passes with `-g` and
fails in its file is reporting shared state or a shared window, not itself.

**A property observable only through timing needs the window opened on purpose.** Make
the peer emit, sleep, and only then answer, so the loop provably sees the bad state. A
test that catches a race by luck stops catching it silently, and the likeliest thing to
retire it is a later fix to the test itself — so before changing the mechanism a test
depends on, ask which property each assertion stands on. Two assertions resting on two
mechanisms belong in two tests that fail for different reasons.

**Assert against the configured value read back from the harness**, not against a literal
the code and the test share, which proves only that they agree. Read what a fixture's
values resolve to rather than what its comment says: `mkdtemp(os.tmpdir())` is
`/var/folders/…` on macOS.

**A fixture's size and timing are part of the contract.** A two-item list cannot observe
an unscrollable, unsearchable catalogue; a scenario emitting its whole tree inside one
millisecond gives every bar equal length (`docs/agents.md` § Testing an agent module).

## Spec and Code Evolution

Neither code nor spec wins automatically when they diverge. Identify whether behavior or
intent should move, update the governing spec first when the design changed, then update
the code. Do not rewrite a spec to bless an implementation accident, and do not contort
code around a design that evidence has invalidated.

Before implementing:

1. Read the governing section in `docs/` and any linked open decision.
2. Verify behavior against source or a spike. Never turn an unverified assumption into an
   invariant.
3. Where several materially different shapes remain viable, research reference
   repositories, dependency source, and prior art.

Research an uncertain decision rather than guessing: read the dependency's source, study
how mature projects solved the same problem, measure with a disposable spike where
behavior is unclear. Uncertainty is not permission to stall — investigate, then decide.
Skip this for language syntax, standard library usage, and trivial details.

Aim for the best available answer across architecture, performance, UI and UX, aesthetic
judgment, code quality, and documentation quality. Prefer proven patterns and
battle-tested libraries; deviate when Sylph's requirements demand it, when profiling shows
the standard pattern does not address the bottleneck, or when security calls for something
stricter, and document the deviation and its evidence where the code lives.

Record decisions as comments next to the code they govern, including what was rejected and
why. `frontend/components/execution-timeline.tsx` shows the intended density: it explains how
virtualization and stable node identities preserve the visible reading position.

## Compatibility

**This project has no other consumer, so breaking changes are free and compatibility code
is forbidden.** Change the schema, the wire format, or the internal API in place. Do not
write a migration, a dual-spelling alias, a version branch, or a fallback for a caller
that does not exist. Discarding the store is the normal answer to schema churn: wipe
`~/.sylph/data`, re-adopt, move on.

The cost of a compatibility branch is that it makes the defect it papers over survivable,
so nobody removes it — a fallback absorbing a rename is how a vendor-key defect becomes
permanent. A hard break fails at the one moment someone is looking.

So when a change would be improved by restructuring something older, restructure it. When
code exists only to tolerate a shape no longer produced, delete it. A refactor that leaves
the old path in place has not landed.

Two things this does **not** license. It is not permission to break a *vendor's* contract:
an agent CLI is a real external dependency with real users, and an adapter still adapts.
And it is not permission to **ingest** lossily. A vendor's native store is that vendor's
to prune, rotate, and reshape, so a parser reads and keeps the vendor's whole record,
including fields it has no use for yet. The record is the one thing that cannot be
recovered later, and keeping it is what makes a schema break cheap: the store rebuilds by
re-projection.

## Language

Code, documentation, and commit messages are English only: every `.rs`, `.ts`, and `.tsx`
file, everything in `docs/`, all comments, all identifiers and endpoint names, all test
names, and both commit subject and body.

Conversation happens in whatever language the user writes in. Durable artifacts stay
English regardless.

## Commits

Use Conventional Commits scoped to the affected component, and explain what changed and
why — plus how, when the approach is not obvious.

```
feat(backend): add SSE streaming for turn events
fix(db): correct session status transition logic
docs(arch): clarify runner discovery protocol
```

Stage files explicitly. Never `git add -A` or `git add .`, since unrelated concurrent work
may be sitting in the tree. Check `git status` and `git diff --staged` before committing to
confirm exactly what is included.

## Generated Agent Files

`frontend/AGENTS.md` and `frontend/CLAUDE.md` are written by `next dev`, not by hand. The
generator is `frontend/node_modules/next/dist/server/lib/generate-agent-files.js`, and it
re-creates them when removed. Commit them with your work; reverting them from a diff only
produces the same uncommitted change again.

Their content is a real instruction, not boilerplate: this Next.js version has breaking
changes against older training data, so consult `frontend/node_modules/next/dist/docs/`
before writing frontend code.

The root `CLAUDE.md` is a symlink to this file. Edit `AGENTS.md`.
