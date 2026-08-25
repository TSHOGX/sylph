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

E2E starts both servers itself on offset ports with a throwaway data directory
and a mock runner, so it never touches your real journal, archive, or agents.
Do not start servers by hand for it. It shells out to `cargo`, so the `PATH`
caveat below applies to E2E too, and a cold run compiles the backend first.

Run the checks for every part of the repo you touched, not only the one you
edited most. Backend and frontend share a wire contract, so a change on either
side can break the other's tests.

## Environment

| Variable | Applies to | Default |
| --- | --- | --- |
| `SYLPH_WEB_HOST` | backend | `127.0.0.1` |
| `SYLPH_WEB_PORT` | backend | `4317` |
| `SYLPH_WEB_DATA_DIR` | backend | `$HOME/.agents/data/sylph/web` |
| `SYLPH_RUNNERS_DIR` | backend | `$HOME/.agents/skills/sylph/runners` |
| `SYLPH_AGENTS_DIR` | backend | first existing of `<exe dir>/adapters`, `<repo>/adapters`, `$HOME/.local/share/sylph-web/adapters` |
| `SYLPH_SOUL` | backend | `<runners_dir>/../SOUL.md` |
| `TRACEDB_BIN` | backend | `$HOME/Workspace/trace-db/target/release/trace-db` |
| `SYLPH_WEB_BROWSE_ROOTS` | backend | `$HOME` |
| `RUST_LOG` | backend | `info` |
| `SYLPH_WEB_BACKEND` | frontend | `http://localhost:4317` |
| `SYLPH_WEB_DEV_ORIGINS` | frontend, `next dev` only | *(none)* |
| `SYLPH_WEB_PREFIX` | `scripts/install.sh` | `$HOME/.local/share/sylph-web` |
| `SYLPH_WEB_BIN_DIR` | `scripts/install.sh` | `$HOME/.local/bin` |
| `SYLPH_WEB_FRONTEND_PORT` | `scripts/install.sh` | `3000` |

`SYLPH_WEB_DEV_ORIGINS` is comma-separated **hostnames**, no scheme and no port.
Next 16 blocks cross-site requests to `/_next/*` as DNS-rebinding defence, so a
browser reaching the dev server from a LAN IP gets 403s on every chunk and the page
freezes before React hydrates — which reads as a backend failure rather than a
dev-server policy. It is not needed for localhost, nor behind the loopback reverse
proxy, which rewrites `Origin`. See `docs/development.md`.

`SYLPH_AGENTS_DIR` holds the *bundled* adapters, which ship with the binary because
an adapter is versioned with the protocol it implements. Unset, resolution prefers
`<exe dir>/adapters` — the only candidate that stays right when the install tree is
moved or copied — then the repo, then the install path. Set explicitly, it is obeyed
even if absent: searching past an operator's own typo would report adapters they did
not choose. `SYLPH_RUNNERS_DIR` is the separate, user-added set, searched after the
bundled ones so a user can shadow a bundled adapter by name.

`SYLPH_WEB_BROWSE_ROOTS` is colon-separated and *adds* to `$HOME`, which is
always browsable. It bounds `/api/v1/fs/*`, the only endpoint that lists
directories the client names — so widening it widens what any client of this
backend can enumerate. Set it when projects live outside the home directory.

`SYLPH_SOUL` points at the identity preamble, and the backend resolves it the way
the runners do so the two cannot disagree about whether injection is active. An
explicit *empty* value is meaningful rather than missing — the runner protocol
reserves it for "no identity" — and a configured path that does not exist also
resolves to none, matching the runner's `[[ -f "$SOUL_PATH" ]] || SOUL_PATH=""`.
Reporting a configured-but-absent file as present would let the UI promise an
injection that never happens. `GET /api/v1/soul` reports what resolved.

All three storage tiers are derived from the data dir, not separately configurable:
`journal.db` (hot), `history.db` (warm), `cas/` (cold). A pre-existing `live.db` is
renamed to `journal.db` on startup, after a truncating WAL checkpoint — never by
moving the three WAL files, since a crash between renaming `-wal` and `.db` strands
committed data in an orphaned WAL.

`TRACEDB_BIN` defaults to a release build in a sibling checkout rather than a `PATH`
lookup, so TraceDB-dependent features degrade until that binary exists — check
`/api/v1/health` for `tracedb.available`. Retention is one of them, and degrades
safely: with no archive it keeps everything and says so in `/diagnostics` rather than
reporting a failure per session.

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
scenario in `frontend/e2e/fixtures/runners/e2e/runner.sh` spaces its stages a
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

**The E2E data directory is never wiped, so specs must not count shared state.**
`frontend/.e2e-data/` accumulates every session across every run — it passed 240
during Phase 7 and 357 during Phase 9 — and the sidebar caps its listing at 200. A
spec that creates a session in `/tmp` and asserts "the group grew by one" therefore
passes for months and then fails permanently once the shared group reaches that
cap, with a diff-by-one that looks like a UI bug and is not. Past the cap a new
session *displaces* an older row instead of adding one, so the count stays flat.
Assert on identity — "the session I just created is listed here", matched by its id
— or create a `mkdtemp` directory per spec and count against that. This has now
bitten twice, in exactly the same place.

**That same accumulation also makes the suite progressively slower, and it looks
like flakiness.** The `mkdtemp` directory each spec creates becomes a *permanent
sidebar group*, because grouping is by workdir and the journal is never cleared.
After enough runs the sidebar renders 151 groups of which 148 hold one dead
single-session directory each, and every page load pays for all of them.
Measured during Phase 9: the suite went from 1.5 minutes to 35 minutes, and two
or three specs failed per run on the 30s default timeout — a *different* two or
three each time, which is what makes it read as flakiness rather than as a
fixture problem. `rm -rf frontend/.e2e-data` is the fix, and it is free: the
directory is gitignored and the backend recreates `journal.db` on start. Do that
before trusting a timing result or diagnosing an intermittent E2E failure.
Raising the timeout does not help, because the cost grows without bound.

**An E2E fixture that cannot be discovered fails the suite while looking like a
product bug.** Discovery requires `adapter.sh`; after the v1 runner path was deleted,
`frontend/e2e/fixtures/runners/e2e/` still held only `runner.sh`, so the backend
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

**A degraded dependency is one fact about the run, not one fault per item.** With no
TraceDB — a supported configuration — probing per session turned one missing binary
into a warning per session per pass, forever. Probe once per pass and report it once.
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

**TraceDB's `cwd` filter is a substring match, not a path match.** `list` and
`search` compile it to `cwd LIKE '%' || ? || '%'`, so asking for `/w/app` also
returns `/w/app-old` and every subdirectory. Anything that means "this one
directory" must re-filter for exact equality after normalizing trailing slashes —
`api::archive` and `api::workspaces` share `normalize` for exactly this.

**`list` does not collapse fork lineage, but `search` does.** Delegated sessions
are rows of their own and are 51% of a real local archive (2238 of 4416
non-empty), so a listing that does not fold them into their parent is half noise.
`list` carries no parent column, so lineage costs one `show` per candidate —
`Client::session_metadata` is the cheap shape, ~0.5 ms and ~1 KB.

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
present twenty controls a screen reader cannot tell apart. Use `aria-label`. And
note the converse when testing: Playwright's `getByRole` name matching is a
substring match, so `{ name: "Explore" }` also matches "Focus Explore"; distinct
names still need `exact: true`.

**A collapsed subtree must report what it hides, or collapsing is lossy.** Hiding
a delegation's work by default is only defensible if a failure three levels down is
still visible at the top: otherwise the user must open every branch to learn
something broke, which is the work collapsing was supposed to save. A descendant
failure escalates its ancestors' status, and a failed branch expands itself to the
tool that failed. Keep `abandoned` distinct from `failed` — the server closing a
trajectory an adapter left open means nobody reported an outcome, and colouring that
red reports an interrupted turn as a broken one.

**A layout bug below a breakpoint is invisible to a suite that runs above it.**
Three mobile failures survived 41 green specs because every one of them ran at
1280px, where the same code is merely cramped. A viewport is an instrument, not
a variant. Note also that a `devices["iPhone …"]` profile runs in *Chromium*
unless WebKit is installed, so it claims iOS coverage it does not have — prefer
a Chromium device profile and be explicit about what is therefore untested.

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
