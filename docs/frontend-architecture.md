# Frontend Architecture

The frontend consumes the unified trace read model. [unified-trace-foundation.md](unified-trace-foundation.md)
owns meaning, [unified-trace-storage.md](unified-trace-storage.md) owns snapshots and
bounded reads, and [unified-trace-ui.md](unified-trace-ui.md) owns reading and interaction.
[ui-layout.md](ui-layout.md) owns shared layout tokens, files and workspace surfaces.
This document describes the implementation and its verification.

## Technology Stack

The runtime dependencies in `frontend/package.json` are Next.js App Router, React,
TanStack Query and TanStack Virtual. TypeScript is strict, Tailwind supplies styles,
Vitest checks pure logic, and Playwright checks browser behavior. Installed Next.js
documentation is authoritative for framework behavior; generated frontend agent files
point to it.

Markdown is rendered through React elements in `frontend/components/markdown.tsx`.
It supports the documented subset of headings, paragraphs, lists, tables, links and
fenced code without injecting HTML. Link schemes are checked. File diffs, fuzzy
picker matching, SVG icons and accessibility primitives remain local modules.

## Application Structure

| Surface | Implementation |
| --- | --- |
| Workspace shell, navigation and overlays | `frontend/components/app-shell.tsx`, `frontend/lib/workspace-ui.tsx` |
| Addressed root or embedded conversation | `frontend/app/agents/[ref]/page.tsx`, `frontend/components/agent-reader.tsx` |
| Agent directory, history, search and object inspection | `frontend/components/trace-inspector.tsx` |
| Immutable retained body range | `frontend/components/trace-payload.tsx` |
| Current authorized controls and command receipts | `frontend/components/trace-composer.tsx` |
| Shared composer, configuration, slash commands and attachments | `frontend/components/composer.tsx` |
| Ephemeral prepared execution before native identity binding | `frontend/app/sessions/draft/[id]/page.tsx` |
| Explicit file context and local file-view choices | `frontend/lib/trace-focus.ts`, `frontend/lib/file-panel-state.ts` |

Agent URLs carry opaque references, a selected history and an optional object anchor.
The browser echoes these values; it never constructs identity from text, a path, a
vendor payload or a parent session. Search navigation keeps its returned read lease
in bounded local state so the opened object belongs to the same retained result.
The shared URL itself contains no Subject credential or transient lease token.

A prepared execution has no trace address. Submitting to its commands endpoint yields
a durable acceptance that can precede native identity binding. The draft surface polls
the accepted command, preserves the pending submission, and navigates only when the
server returns the resulting session's `traceAgentRef`.

## State Management

### Server State (TanStack Query)

TanStack Query owns workspace metadata, environment inventory, settings resources and
filesystem reads. Trace bodies and history pages use dedicated bounded stores instead
of retaining every visited query in a generic query cache.

`frontend/lib/use-trace-resource.ts` gives each mounted read an AbortController. It
ignores late responses after unmount, preserves the current visible result while a
replacement is pending, and clears protected values on access invalidation. Only
explicit `projection_pending` and `search_indexing` GET failures receive bounded,
abortable backoff. A command is never retried by a read helper.

Reader bootstrap and operational control responses are fetched fresh. Immutable
metadata cache keys include server, authorization scope, generation, revision,
evidence cut, view token and query scope. A cached trace fact is never substituted
for a current control authorization decision.

### Client State

`frontend/lib/trace-window.ts` keeps at most two complete history pages and merges
only pages from the same read view. Eviction drops a complete far page, preserving
the exact cursor on each retained boundary. Paged assistive mode presents a bounded
slice and exposes earlier/later navigation without accumulating the entire history.

`frontend/lib/trace-cache.ts` accounts for encoded metadata and keys. The payload
cache in `frontend/lib/trace-payload-cache.ts` reserves bytes for opened body windows;
a pinned window cannot be evicted merely to make the reported budget look smaller.
Closing a window releases its reservation. Immutable bodies retain their revision
key across unrelated projection updates, while every new body read authorizes its
current `via_ref`, containing agent, selected history and representation.

The server supplies cache, page, payload, interest and mounted-row limits. Hidden
readers release subscriptions and requests, retaining bounded anchors and local view
choices. Draft text and staged attachments are separate from the evictable history
cache. Snapshot expiry preserves them; changing Subject clears protected state.

View ownership is shared across mounted readers and bounded inspection/navigation
entries. The last owner releases a view after one task, allowing Strict Mode and
navigation handoffs. Document `pagehide` and protected-cache clearing flush releases
immediately through keepalive requests; they cannot rely on React unmounting or a
later timer. Restoring a cached document invalidates those released views and
reacquires them around retained anchors. Server expiry covers abrupt process loss.

Scroll restoration uses a stable object anchor and pixel offset. Programmatic scroll
events cannot replace that anchor while row measurements settle. Resize compensation
and virtualization use the same anchor, and native CSS scroll anchoring is disabled
on that container. A focused row stays mounted within the shared row budget.

Shell preferences use the existing local-storage external store. File expansion and
selection belong to an explicit file context returned for the focused agent; an
embedded agent does not borrow its anchor session's files or command recipient.

### Internationalization

English is the source of truth and the fallback; Chinese is optional. The locale is a
settings field, so it rides the store above — switching language re-renders exactly the
components that read a translation.

```
lib/i18n/locale.ts   Locale set, tags, message lookup, format, formatParts.
                     No React, no settings import.
lib/i18n/messages.ts en — the `Dictionary` type and the source of truth
lib/i18n/zh.ts       zh — typed as a total `Dictionary`
lib/i18n/index.ts    "use client": useT(), useLocale(), re-exports
```

**The `locale.ts` / `index.ts` split makes the dependency run one way.** `lib/settings`
needs the `Locale` type to store and validate one, and `useT` needs `lib/settings` to know
which locale is active, so the primitives stay free of both React and the store. That half
is the testable one: substitution and fallback are pure functions.

**An untranslated key is a build error**, since `zh` is typed as a total `Record` over the
key set. Placeholder agreement across catalogues is a unit test, because dropping a
`{placeholder}` still type-checks.

Three rules keep a surface fully translated:

- **A rendered group heading is a key**, even when the same literal also drives sorting.
- **A label built from a stored enum reads that enum's own translation**
  (`settings.density.*`) rather than interpolating the raw value.
- **A component with translated labels lists `t` among its `useMemo` dependencies**, so a
  language change repaints the labels a filter matches against.
- **A sentence wrapping a value in a mono face is one key.** English precedes the value
  ("Inside `src/`") and Chinese wraps it ("在 `src/` 内"), so the halves are not
  independently translatable. `formatParts` splits the *translated* string around one
  placeholder; `Around` in `lib/file-operations.tsx` renders the result.
- **Pure modules take `t` as a parameter** rather than reading the locale, which puts their
  plural rules in the catalogue and keeps them unit-testable; `describeKind` in
  `lib/file-kind.ts` returns a `MessageKey` for the caller to translate. `next/dynamic`'s
  `loading` fallbacks are the exception, created at module scope where no hook can run.

**No framework.** `next-intl` and `next-i18next` are built around locale-in-the-URL — a
`[locale]` segment, negotiating middleware, per-request catalogue loads — which would mean
restructuring `app/` around a value `localStorage` already holds for one user's loopback
tool. What they otherwise buy is either already in `Intl` (`lib/format.ts`) or lighter to
inline: both catalogues together are smaller than the machinery to load one lazily.
Revisit at a third locale with translators who need a standard file format, or past
roughly a thousand keys.

Three details worth stating:

- **Locale names are endonyms** (`English`, `中文`), so the picker's labels stay
  untranslated: someone who has switched into a language they cannot read finds the way
  back only if the entry they want is written in the language they know.
- **`<html lang>` syncs client-side** via `applyLocale`, because a screen reader picks its
  voice from that attribute. The document ships `lang="en"` for the default.
- **Substitution is one regex pass**, since these values reach the DOM. An unmatched
  placeholder is left verbatim, so it surfaces in the UI.

**Backend `message` strings pass through untranslated.** They are one process's report of
what it refused, and localizing them would mean cataloguing every string the API can emit.


## Real-Time Event Streaming

### SSE Connection Management

`frontend/lib/trace-subscriptions.ts` owns one multiplexed fetch stream for visible
trace interests. Each interest supplies its bootstrap view token as a scope proof;
the global replay cursor alone cannot authorize a paused reader's older scope.
The server checks those proofs against current grants and deletion epochs.

`frontend/lib/sse-parse.ts` handles event-stream framing incrementally, including
named events, split line terminators and multiline data. Trace consumers supply byte
limits and reject overlarge frames or change arrays instead of buffering unbounded
catch-up work.

A complete revision invalidates a coherent window. A reader following the tail may
refresh; focus or selection in the reading area pauses replacement and exposes
`History updated`. Accepting the update reads around the current anchor. No changed
object causes automatic descendant traversal or body downloads.

Transport EOF triggers cursor replay and says nothing about execution completion.
An explicit reset requires a fresh view. Access changes clear protected content and
subscriptions. A view-expired response also clears retained previews, reacquires an
around-anchor bootstrap, and preserves the local draft and editor selection.
Commands are never replayed when the observation connection reconnects.

The Next rewrite proxies SSE without a buffering route handler. Compression must
exclude the streaming response; [configuration.md](configuration.md) and
[api/README.md](api/README.md) own transport configuration and API conventions.

## Controls and Requests

### Two facets, each with a reason

Observe and control are shared environment/vendor facts. A Subject's authorization
is a separate current result. Cached reachability cannot grant an action, and an
ungrantable action does not imply the retained conversation is unreadable.

The control composer displays its focused target. Every action fetches current typed
availability. Stop, interrupt and steer also compare the displayed route, lease and
affected execution with that fresh answer, so they cannot silently target a successor
execution. The server still enforces the submitted lease atomically.

Durable command acceptance clears only the submitted draft revision; newer typing and
attachments remain. Receipts are shown in an operational queue until the trace catches
up. Editable/cancellable flags come from the server, and queue changes send the expected
revision. Ambiguous delivery offers status recovery, without automatic resubmission.

Current questions arrive as typed protocol requests, including declared choices and
forms. `frontend/components/approval-dialog.tsx` renders them only after explicit review.
It does not invent allow/deny options when no permitted answer exists. The request's
native id is submitted with the exact current agent control route. A resolved request
loses its answer controls on the next operational refresh.

## Performance Optimization

### Bundle size management

Measure the scripts referenced by the served route with `scripts/measure-bundle.sh`.
Summing the chunk directory counts code belonging to every route and is not evidence
about first-load cost. Dynamic agent routes must be measured as served pages.

```sh
cd frontend && npm run build
./scripts/measure-bundle.sh
./scripts/measure-bundle.sh --route='/agents/[ref]'
```

A measurement is valid only for the exact build and route it loaded. The script refuses
a busy measurement port to avoid measuring another checkout's server. Dependencies
that add large parser/runtime bundles require a measured product benefit; Office
formats are parsed server-side and sent as bounded document/grid structure.

### Code splitting

`AppShell` survives every navigation, so optional shell overlays use `next/dynamic`.
Companion readers load when opened. Format-specific file viewers split below the file
pane so a source file does not load spreadsheet or document viewer code.

### Render cost

History is virtualized with stable reading addresses and variable row measurements.
The shared maximum includes overscan, focused rows and simultaneously visible readers
or lists. An agent directory reads only a page of summaries; opening it never fetches
all child conversations. Inspecting one operation downloads only explicitly selected
body representations and bounded byte windows.

`frontend/e2e/performance-isolation.spec.ts` measures filesystem interaction beside a
large synthetic trace scope. It reads served limits, browser timings and mounted rows,
and verifies that a multi-gigabyte advertised payload triggers no automatic download.
The synthetic scope is a layout measurement, not evidence about vendor semantics.

### Document previews

Document and grid viewers request explicit row/sheet windows. Paging replaces the
window rather than accumulating it. Wide tables own horizontal scrolling and use fixed
layout where content-driven column measurement would make each append global work.
The file API owns parser work and byte bounds; see [api/files.md](api/files.md).

### File browsing

`frontend/components/artifacts-pane.tsx` requests one directory per expansion and uses
its declared session file context. A readable retained trace need not have a reachable
filesystem. File requests can fail independently of trace reads, and their error names
the failed path or operation without inventing environment connectivity.

## Accessibility

Inspector tabs use manual activation: arrows move focus, Enter or Space changes the
panel. The desktop separator supports keyboard resizing and reports its value. When
available container width cannot preserve the main reader's minimum, inspection becomes
a full-height dialog with Back and focus restoration. Coarse-pointer actions retain
usable hit areas, prose wraps at narrow widths, and code/tables own horizontal overflow.

Paged reading is an explicit option for assistive technology. No screen-reader detection
changes behavior. Focused rows count toward the same mounted bound and remain mounted
until focus leaves. Browser Find keeps its reserved shortcut; product search has a
visible action and command-palette entry.

Playwright checks keyboard paths and axe checks automated WCAG 2.1 A/AA rules. These
checks cover the behavior they exercise and do not establish a complete screen-reader
or real iOS Safari audit. Chromium's phone profile tests reflow and coarse pointers,
not Safari's browser chrome or keyboard implementation.

## Testing Strategy

### Unit — Vitest (`npm test`)

Pure tests cover window consistency, exact cursor retention, byte accounting and pinned
payload reservations, typed command targeting, projection-read backoff, SSE framing,
request forms, Markdown, file formats, picker navigation and shortcut handling. New
behavioral tests are verified by introducing a targeted fault and observing failure,
then restoring the implementation and rerunning.

### E2E — Playwright (`npm run e2e`)

The npm launcher starts a server and a separately compiled envagent with throwaway roots,
pairs them and builds the web frontend with the test proxy address. Nothing needs a
manually started server. The fixture feature replaces the adapter factory with raw
Codex-shaped acquisition; real vendor CLIs and credentials are unreachable.

Browser evidence has three distinct purposes:

- `frontend/e2e/trace-corpus.spec.ts` consumes
  `fixtures/unified-trace/reading/native-codex.json`, regenerated and compared by the
  production Codex mapper, reducer and reading-projector test.
- Synthetic typed fixtures in `frontend/e2e/fixtures/trace.ts` exercise bounded layout,
  correction, access, routing and large-catalogue behavior without a vendor classifier.
- Lifecycle, draft and filesystem tests cross the actual server/envagent/raw-capture
  product path using the fixture adapter.

The suite includes root/embedded readers, independent lifecycle, scoped search, paused
corrections, payload provenance, queue revision, guarded control, requests, viewport
reflow, focus, imports, files and shell behavior. `npx playwright test --list` reports
current coverage counts; this document does not duplicate them.

#### Startup readiness

The launcher requires the exact safety banner `Fixture adapters enabled: codex (no
vendor processes)` and a connected environment advertising the fixture. HTTP server
health alone does not establish an available environment.

Nothing the launcher spawns is detached. Playwright kills the launcher's process group
and waits for inherited pipes to close; a detached child keeps a pipe open and hangs
teardown after every assertion passed. Build binaries first, then spawn them directly.
`CARGO_TARGET_DIR` must name the same target for build and launch.

#### Fixture state

`npm run e2e` runs `frontend/e2e/reset-home.mjs` before Playwright starts the backend.
That npm script is the sole place that wipes the server fixture home. Running Playwright
directly requires the reset script first. A Playwright setup hook runs too late and would
remove files from a store the backend already opened.

Each browser case owns its fixture state and intercepted responses. Real filesystem
cases create unique temporary directories and clean them up. A session-list cap is not
an assertion about a particular session; match the returned identity instead of assuming
shared counts increase forever.

Specs import the shared `frontend/e2e/fixtures/test.ts` fixture. It navigates away
before disposing the browser context so the real document lifecycle can release
server view leases. Playwright context disposal kills the document without `pagehide`;
repeating that abrupt shutdown is a process-loss experiment, not ordinary navigation.
Lease regressions verify server-side invalidation before the advertised expiry.

The two browser projects are desktop Chromium and a Chromium phone profile. Tests of
one breakpoint do not establish the behavior of another. Wait for hydration before
sending a global shortcut, and inspect the actual reported environment and limits
instead of repeating a harness constant in the assertion.

#### Writing a spec that fails for one reason

Use the API for a precondition and the UI for the behavior under test. Intercept typed
server results for a browser branch; use retained corpus projections for semantic
claims. Exact role names avoid accidental matches against another panel's controls.

A timing property needs an intentionally opened window: hold a response while the
reader edits, or keep an observed activation active while measuring. Fixed sleeps that
happen to catch a race do not establish the contract. A command receipt is not an
activation outcome, and an activation outcome does not imply observation is caught up.

Run the full file after isolated debugging, then the suite after the last edit. A test
passing only alone is evidence of shared state. Release intercepted request handlers
at teardown, and use separate browser contexts for separate Subjects.

#### Reading a failure

A launcher failure that executes zero browser tests is inconclusive, not a green run.
An OOM or signal exit without a summary is also not a passing result. Inspect the
failing trace, screenshot, request envelope and measured geometry before changing an
assertion or timeout. `scripts/e2e-repeat.sh` measures recurrence when a failure is not
yet reproducible.

## Development Workflow

[development.md](development.md) owns stack setup. `frontend/package.json` owns runtime
dependencies and check commands. Run build, ESLint, unit tests and E2E for frontend work;
a wire change also requires the Rust workspace tests, Clippy and rustdoc gates.

### API Proxy

`frontend/next.config.ts` rewrites same-origin API requests to the configured backend.
The cookie is SameSite=Strict. `SYLPH_WEB_BACKEND` is compiled into rewrites at build
time, so E2E sets it for both the build and serving process. A prebuilt default rewrite
would address the developer's server instead of the throwaway test backend.

### Types

`frontend/lib/api-types.ts` mirrors operational and filesystem DTOs.
`frontend/lib/trace-types.ts` mirrors `crates/protocol/src/trace/read.rs`, including closed
semantic variants and opaque addresses. `frontend/lib/trace-api.ts` owns trace routes,
read leases and representation/range requests. There is no browser vendor JSON parser,
second event vocabulary, synthetic turn model or session-id compatibility route.

The served API schema and backend route tests establish wire behavior. TypeScript
compilation checks consumers; it cannot prove a hand-written DTO matches a real response,
which is why the production-projection fixture and product-path browser tests are gates.

### Build

`npm run build` runs the TypeScript gate. ESLint runs as `npx eslint .`; `next lint` no
longer exists. After removing a route, stale generated Next development type validators
can still cite it. Regenerate or clear those generated validators, then rebuild; do not
restore a deleted production route to satisfy a generated cache.
