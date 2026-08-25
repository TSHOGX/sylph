# Frontend Architecture

Sylph Web frontend is a Next.js 16 application providing a local-first UI for
multi-agent session management. It is an **agent workspace**, not a chat client:
tool execution is the primary content, prose is connective tissue, and navigation
is keyboard-first.

> **Reading this document.** Sections below describe the shipped design. Where an
> earlier plan was not followed, the deviation is stated with its reason rather
> than quietly deleted — a spec that hides why it diverged from the code teaches
> the next reader nothing. The largest deviations are: no Zustand (React context
> plus an external store), no Radix UI, no Shiki, no `react-markdown`, and no
> virtualization. All four are bundle-budget decisions, explained in
> **Performance Optimization**.

## Reference Implementations and Research

**DeepSeek Harness is a reference, no longer the directive**

DSH source is still worth reading when a UI question has an established answer:

```
/Users/hsw/Workspace/oss/deepseek-harness/repo/apps/web/
/Users/hsw/Workspace/oss/deepseek-harness/repo/packages/web/
```

This section originally read "PRIMARY DIRECTIVE: default to DSH's patterns" and
scoped itself to Phase 2. Three of its instructions have since been overtaken by
measurement, so it is recorded as history rather than followed:

- **Not the canvas.** DSH renders message bubbles; Phase 6 rejected that shape for a
  flat tool-anchored timeline. See "Conversation canvas: agent-first, not chat-first"
  and `docs/agent-ui-design-analysis.md` for the argument.
- **Not the transport.** The `EventSource` design copied from here shipped a bug that
  dropped the entire stream. `lib/use-sse.ts` reads the body with `fetch` instead —
  reasons at "Why not EventSource" below.
- **Not the dependency choices.** DSH is measured against a different bundle; the
  no-dependency decisions here are measured against this one.

What still transfers: layout structure, streaming-text handling, and the general
lesson that reading a production implementation beats guessing.

---

**GENERAL PRINCIPLE: Research Before Designing**

As Sylph Web enters continuous development, **any uncertain design decision requires a research phase**:

1. **Identify the problem domain** — What are you trying to solve? (e.g., "efficient virtual scrolling", "SSE reconnection strategy", "toast notification UX")

2. **Search for established patterns:**
   - GitHub: search repos with high stars in the relevant domain
   - NPM/npm trends: find popular, actively-maintained libraries
   - Articles: Dev.to, Medium, official docs for best practices
   - Real implementations: read source code of production apps

3. **Study mature open-source practices:**
   - How does TanStack Query handle cache invalidation?
   - How does Radix UI implement accessible dialogs?
   - How does Vercel's Next.js app structure its API routes?
   - How does Linear/GitHub handle optimistic updates?

4. **Document your findings:**
   - What patterns did you find?
   - Which libraries/approaches are production-proven?
   - What are the tradeoffs?

5. **Refine your design** based on research:
   - Adopt proven patterns unless you have strong evidence for deviation
   - Prefer libraries over custom implementations when quality is equal
   - Document why you chose approach X over Y

**Example research workflow:**

```
Problem: "How should we handle SSE reconnection with exponential backoff?"

Research:
1. Search: "SSE reconnection pattern" + "EventSource reconnection"
2. Find: eventsource library on npm (3M weekly downloads)
3. Read: source code shows exponential backoff with jitter
4. Find: Mozilla MDN docs on EventSource best practices
5. Check: How does DSH handle reconnection? (read their code)

Conclusion: Use exponential backoff (1s, 2s, 4s, 8s, max 30s) with
UI feedback after 3s. Pattern proven in both eventsource lib and DSH.
```

**When to research:**
- ❌ Don't research: "How to create a React component" (too basic)
- ✅ Do research: "How to virtualize 10K+ items efficiently"
- ✅ Do research: "Best pattern for optimistic mutations with rollback"
- ✅ Do research: "How to handle file upload progress with SSE"
- ✅ Do research: "Accessible keyboard navigation in complex lists"

**Why this matters:**
- Saves time by avoiding reinventing solved problems
- Reduces bugs by using battle-tested patterns
- Improves quality by learning from mature implementations
- Builds team knowledge through documented research

---

## Design Philosophy

**Core principles:**
1. **Origin-aware UI** — Visual distinction between Sylph-managed and native sessions
2. **Real-time first** — SSE-driven updates with optimistic rendering
3. **Local-first** — No external dependencies, works offline after initial load
4. **Accessibility** — WCAG 2.1 AA compliance, keyboard navigation, screen reader support
5. **Performance** — Keep bundles compact, optimize rendering, maintain responsiveness
6. **Architecture quality** — Balance performance, user experience, maintainability, and extensibility

**Taken from DeepSeek Harness, and what was not:**

| Borrowed | Diverged, with the reason recorded below |
| --- | --- |
| Layout: rail plus canvas, now three panes | Message bubbles → flat tool-anchored timeline |
| Streaming text handling | `EventSource` → `fetch` body reading |
| Attachment handling in the input area | Input area → one composer surface carrying agent, model, effort |
| Thinking block collapse/expand | Uniform expansion → expansion by role (edits open, shell output on failure, reads closed) |
| Error-state and loading patterns | Syntax-highlighting library → hand-rolled `code-block.tsx` |

**Sylph-specific, with no DSH equivalent:**
- Dual-origin session indicators (green Sylph / violet native)
- Preservation policy warning on native session resume
- Per-session agent, model, and effort flags, read from `--describe`
- TraceDB archive browsing and search, with lineage rollup and deduplication
- Session incarnation history (backend lifecycle tracking)

Lifecycle indicators (24h idle, 04:00 reset) are modelled in the backend and surfaced
nowhere in the UI yet.

## Technology Stack

**Core framework:**
- Next.js 16 (App Router)
- React 19 (with Server Components where applicable)
- TypeScript 5.7+ (strict mode)

**State management:**
- TanStack Query v5 (server state, caching)
- React context for shell state; `useSyncExternalStore` over `localStorage` for
  preferences. **No Zustand:** two contexts and one external store cover every
  case here, and a state library would add a dependency for a store that holds
  about a dozen fields.
- EventSource API (SSE streaming)

**UI components:**
- Tailwind CSS 4 (utility-first styling)
- **No component library.** The primitives needed — modal, menu, drawer, bottom
  sheet, disclosure, listbox — are implemented directly in `components/ui/`,
  `components/overlay.tsx`, native `<details>`, and the two picker overlays. Radix UI would have supplied them with better
  edge-case coverage, at roughly the whole remaining first-load budget. Native
  `<details>` in particular is *better* than a scripted disclosure: it gives
  keyboard operation, `aria-expanded`, and find-on-page behaviour with no script.
- Inline SVG and text glyphs for icons. **No icon package** for the handful of
  shapes in use.
- **No animation library.** Tailwind's `animate-pulse` covers the only motion in
  the UI, which is streaming and running indicators.

**Code rendering:**
- `components/markdown.tsx` — a block-then-inline parser covering the subset agent
  output uses: fenced code, inline code, headings, lists, emphasis, links,
  blockquotes, tables. **Not `react-markdown`** (~45 KB gzipped with `remark-gfm`),
  which alone would exceed the budget. Every node is a React element, so
  agent-authored text cannot inject markup and no sanitizer is needed; link
  schemes are still allowlisted, because a React-rendered `javascript:` href
  executes.
- `components/code-block.tsx` — monospace, line numbers, copy, truncation.
  **No syntax highlighting.** Shiki ships a WASM regex engine plus grammars;
  `react-syntax-highlighter` with Prism is ~35 KB. The way to add it later is a
  dynamic `import()` behind a settings toggle, so it never lands in first-load JS.
- `lib/diff.ts` — LCS line diff for rendering agent edits. **Not jsdiff** (~30 KB)
  or `diff-match-patch` (~70 KB), whose value is character-level diffing and patch
  formats that this UI does not render.
- `lib/fuzzy.ts` — subsequence matching with cluster-based ranking. **Not fuse.js**
  (~12 KB); typo tolerance is deliberately absent, because in a short visible list
  a near-miss costs one keystroke while a promoted wrong match costs trust.

**Testing:**
- Vitest (`npm test`) for pure logic: the diff, the Markdown parser, tool-shape
  interpretation, fuzzy ranking, the shortcut grammar, and timeline row building.
  These are the components where hand-rolling replaced a dependency, so they carry
  the test burden that dependency would have carried.
- Playwright (`npm run e2e`) for rendered behaviour, focus management, keyboard
  paths, and render cost.

**Development:**
- Turbopack (dev server)
- ESLint + Prettier (linting, formatting)
- TypeScript strict mode

## Application Structure

There is no `src/` directory: the app, component, and library roots sit at the
top of `frontend/`, which is what `create-next-app` produces and what the `@/`
path alias resolves against.

```
frontend/
├── app/
│   ├── layout.tsx              # Root layout: providers + AppShell
│   ├── page.tsx                # Start screen: begin, resume, learn the keys
│   ├── search/page.tsx         # TraceDB archive search and import
│   ├── sessions/[id]/page.tsx  # Conversation canvas
│   └── globals.css
│
├── components/
│   ├── app-shell.tsx           # Three-pane frame, top bar, Quick Start
│   ├── workspace-sidebar.tsx   # Left pane: sessions grouped by workdir
│   ├── artifacts-pane.tsx      # Right pane: file tree, swapping to the viewer
│   ├── file-viewer.tsx         # Right pane: one file, header and viewer choice
│   ├── file-viewer-overlay.tsx # The same viewer, full screen
│   ├── viewers/
│   │   ├── grid-view.tsx       # Spreadsheets and CSV
│   │   ├── document-view.tsx   # Word and PowerPoint
│   │   ├── html-view.tsx       # Sandboxed page rendering
│   │   └── media-view.tsx      # Images and PDF
│   ├── timeline.tsx            # The conversation canvas itself
│   ├── markdown.tsx            # Markdown parser and renderer
│   ├── code-block.tsx          # Code and command output
│   ├── diff-view.tsx           # Unified inline diff
│   ├── overlay.tsx             # Modal shell, focus trap, list navigation
│   ├── search-overlay.tsx      # Cmd+K
│   ├── command-palette.tsx     # Cmd+P
│   ├── settings-panel.tsx      # Cmd+,
│   ├── history-overlay.tsx     # Cmd+Shift+K, plus the workspace switcher
│   ├── directory-picker.tsx    # Server-enumerated folder chooser
│   ├── composer.tsx            # Message entry, + menu, config controls, attachments
│   ├── attachment-picker.tsx
│   ├── slash-menu.tsx          # The agent's own commands, as the user types
│   ├── config-picker.tsx       # Typed session config; what a control command opens
│   ├── approval-dialog.tsx     # Answering what an agent is blocked on
│   ├── badges.tsx              # Origin and status badges
│   ├── icons.tsx               # ~40 hand-authored SVG paths, no dependency
│   └── ui/
│       ├── button.tsx          # Button, IconButton, Kbd
│       ├── menu.tsx            # Context menu, dropdown, mobile sheet fork
│       ├── dialog.tsx          # AlertDialog, PromptDialog (not window.confirm)
│       └── sheet.tsx           # Bottom sheet and side drawer (mobile)
│
├── lib/
│   ├── api.ts                  # Typed client; mirrors the wire shape exactly
│   ├── providers.tsx           # QueryClient + AuthGate
│   ├── settings.tsx            # Preferences over localStorage
│   ├── create-session.ts       # Dialog-free session creation + default resolution
│   ├── workspace-ui.tsx        # Panes, overlays, drawers, artifact selection
│   ├── shortcuts.ts            # Combo grammar and platform resolution
│   ├── theme.ts                # Pre-paint theme bootstrap + application
│   ├── viewport.ts             # Is this a phone; the breakpoint constant
│   ├── use-dialog.ts           # Focus trap, restoration, Escape
│   ├── register-sw.tsx         # Service-worker registration (production only)
│   ├── timeline-rows.test.ts   # (tests live beside the code they cover)
│   ├── diff.ts                 # LCS line diff
│   ├── tool-shape.ts           # Interprets tool_call payloads per agent
│   ├── file-kind.ts            # Extension -> viewer family, in one table
│   ├── file-operations.tsx     # New folder, upload, delete: one owner, one dialog
│   ├── grid-format.ts          # Column labels, alignment, paging arithmetic
│   ├── sandbox-html.ts         # CSP injection for previewed HTML
│   ├── fuzzy.ts                # Subsequence ranking
│   ├── use-sse.ts              # Streaming fetch reader with after_seq resume
│   ├── sse-parse.ts            # Incremental event-stream frame decoder
│   ├── use-debounced.ts
│   ├── error-boundary.tsx
│   └── format.ts
│
├── e2e/                        # Playwright specs + mock runner fixture
├── next.config.ts              # Proxy rewrite, standalone output
├── vitest.config.ts
└── playwright.config.ts
```

**Why the shell lives in the root layout.** `AppShell` wraps `children` in
`app/layout.tsx` rather than being placed per page. A layout does not remount on
navigation, so the sidebars keep their expanded groups and scroll position when
moving between sessions; a shell mounted inside a page loses both on every route
change. The consequence is that pages render only their own scrolling content and
do not declare `<main>` — the shell owns `<main id="main">`, which the skip link
targets.

## UI Layout & Design

### Three-pane layout

```
┌──────────────────────────────────────────────────────────────────────┐
│ ▣ Sylph  [⌕ Search sessions and archive        ⌘K] ⚡Quick Start +New ⚙ ▣│
├──────────────┬────────────────────────────────────┬──────────────────┤
│ Workspaces   │ Conversation                       │ Artifacts        │
│ (240px)      │ (flex, only this scrolls)          │ (320–384px)      │
│              │                                    │                  │
│ ⚡Quick Sess. │ › Add logging to the auth module   │ Files            │
│   scratch    │                                    │ ▾ backend/       │
│              │ ▸ ✻ Thinking  84 words             │   ▾ src/         │
│ Workspaces   │                                    │     ▫ lib.rs     │
│ ▾ sylph-web8+│ ▤ Read  backend/src/auth.rs    ✓   │     ▫ auth.rs    │
│   ● session A│                                    │ ▫ Cargo.toml     │
│   ◆ session B│ I'll add logging at three points.  │                  │
│   ⌗ past work│                                    │ ─────────────────│
│   ⌗ older  ⑂2│ ✎ Edit  backend/src/auth.rs    ✓   │ auth.rs          │
│   Browse all→│                                    │                  │
│ ▾ trace-db 3+│                                    │                  │
│   ● session C│                                    │                  │
│              │  ┌──────────────────────────────┐  │ [preview]        │
│              │  │ 45 45   pub async fn auth(   │  │                  │
│              │  │    46 + info!("attempt");    │  │                  │
│              │  └──────────────────────────────┘  │                  │
│              │                                    │                  │
│              │ ❯ Bash  cargo test  ✕ failed (101) │                  │
│              │  ┌──────────────────────────────┐  │                  │
│              │  │ error[E0425]: cannot find... │  │                  │
│              │  └──────────────────────────────┘  │                  │
│              │ ──────────────────────────────────  │                 │
│              │ completed  23.4s  Copy             │                  │
│              │ [composer]                         │                  │
└──────────────┴────────────────────────────────────┴──────────────────┘
```

Flexbox with fixed sidebar widths, not a grid with `fr` units: sidebars must not
shrink as conversation content grows, and only the centre pane scrolls. Panes
toggle with `hidden`/`flex` rather than conditional rendering, so a hidden sidebar
keeps its expanded groups and scroll position.

**Height is `h-dvh`, never `h-screen`.** See "The viewport" below — this is the
single most consequential line in the shell.

### Below `md`: the panes become surfaces

The three-pane layout cannot lay out in flow on a phone. A 240px sidebar on a
390px viewport leaves ~150px for the conversation *and* the composer, and because
`showLeftSidebar` defaults to true, that was the default state.

```
 phone (< 768px)                          drawer open
┌─────────────────────────┐              ┌─────────────────────────┐
│ [≡] Sylph      ⌕ ⚙ ▣    │              │┌───────────────┐        │
├─────────────────────────┤              ││ Workspaces    │ scrim  │
│                         │              ││ ▾ sylph-web   │        │
│  conversation           │   tap [≡]    ││   ● session A │        │
│  (full width)           │  ─────────▶  ││ ▾ trace-db    │        │
│                         │              ││   ● session C │        │
├─────────────────────────┤              │└───────────────┘        │
│ [+] [Model ⌄]      (↑)  │              │                         │
└─────────────────────────┘              └─────────────────────────┘

 tap [▣] — files as a bottom sheet, drag down to dismiss
┌─────────────────────────┐
│  conversation (visible) │
├═════════════════════════┤  ← grab handle; the sheet does not reach the top,
│ ▬ Files            ✕    │    because files are inspection *of* what is above
│ ▾ backend/              │
│   ▫ auth.rs             │
└─────────────────────────┘
```

**Sessions get a left drawer; files get a bottom sheet.** Not a width decision —
it follows from what each pane is *for*. Sessions are navigation, so a left
drawer is where a phone user reaches for it and it matches the hamburger that
opens it. Files are inspection of the thing currently on screen, so the sheet
leaves the conversation visible above it and a vertical drag is the gesture for
"show me more / put it away". This is the split Claude's and ChatGPT's iOS apps
use.

The sheet is drag-dismissable and the drawer is not, deliberately: a rightward
drag from the left edge is iOS Safari's own back-navigation gesture, and
competing with it means the drawer sometimes closes and sometimes navigates
history.

**The split is a component swap, not a stylesheet.** An overlay surface has to
trap focus, close on Escape, and dismiss on navigation, and none of those three is
expressible as a CSS rule. `lib/viewport.ts` answers "is this a phone" through
`useSyncExternalStore` over `matchMedia`, and exports the breakpoint constant that
the `md:` classes in the shell must match — a focus trap running while CSS still
lays the pane out in flow would trap focus inside a visible sidebar.

Drawer state is **transient and separate from the persisted pane preferences**.
`showLeftSidebar` means "I work with the sidebar hidden" and survives restarts; a
drawer must not, because an app that reopens with a drawer over the conversation
because that is how the tab was closed a week ago is broken. It is also derived
rather than synchronised: the state holds the open drawer *plus the route it was
opened on*, and `drawer` is computed from that against the current pathname and
breakpoint. Written as effects, "navigating closes it" and "leaving mobile closes
it" were both `setState` in an effect body — a cascading render, and what
`react-hooks/set-state-in-effect` flags.

### The viewport

Three separate bugs presented as one symptom: "scrolling drags the whole screen
up and reveals blank space". All three are mobile-only, and all three had to be
closed together — fixing the height alone leaves the chain, and fixing the chain
alone leaves a shell that does not fit.

| Cause | Fix |
| --- | --- |
| `100vh` is the *large* viewport height — it measures as though the browser's collapsible toolbars were hidden, so a `h-screen` shell is taller than what is visible and the document scrolls to make up the difference | `h-dvh` on the shell, which tracks the toolbars as they collapse |
| Nothing pinned the document, so a drag that began in a pane continued into the document once that pane hit its end. The blank area *is* the document scrolled past the shell | `html, body { height: 100%; overflow: hidden }` — the shell owns all scrolling |
| Overscroll chained upward, so even a pinned document rubber-banded on iOS | `overscroll-behavior: none` on the document, and the `pane` utility carries `overscroll-behavior: contain` for every scroll container |

Two further contributors, both of which move the page on their own:

- **Mobile Safari zooms when a focused input's text is under 16px, and does not
  zoom back out.** The composer's textarea is `text-base` on mobile (`md:text-sm`
  above), and `globals.css` floors input text at 16px under `pointer: coarse`
  only — dense chrome legitimately uses 11–12px, so the floor is not applied to
  everything.
- **The on-screen keyboard.** `interactiveWidget: "resizes-content"` makes it
  shrink the layout viewport instead of sliding the page up, which is what keeps
  `100dvh` honest while the keyboard is open.

`viewportFit: "cover"` draws into the notch and home-indicator areas, and the
`pb-safe` / `pt-safe` / `px-safe` utilities pad the chrome back out. They *add* to
existing padding rather than replacing it, so a composer can be `pt-2 pb-safe`
and get both. Zoom stays enabled (`maximumScale: 5`): pinch-zoom is an
accessibility affordance under WCAG 1.4.4, and the usual reason to disable it is
the small-input zoom, which is fixed properly above instead.

### Touch targets

`tap-target` and `tap-row` set a 44px minimum — Apple's HIG figure, close to
Material's 48dp — under `pointer: coarse` **only**. The same control should not be
finger-sized on a machine driven by a mouse: the desktop app keeps its dense 24px
tree rows, and the phone gets hit areas it can actually hit.

The corollary is that **every right-click-only action needed a button.** There is
no right-click on a touch screen, so pin, rename, browse-history, copy-path, and
the per-file actions were not merely inconvenient on a phone — they were
unreachable. Every sidebar group, session row, archived row, and file-tree row now
carries an overflow (`⋯`) button: revealed on hover with a fine pointer to keep
the pane quiet, always present with a coarse one, where there is no hover to
reveal it with. It is a *sibling* of the row rather than a child, because a button
inside an anchor is invalid HTML and makes the row's own activation ambiguous.

### Artifacts pane: tree and viewer swap

Selecting a file replaces the tree with a full-height viewer rather than splitting
the pane:

```
browsing                            viewing
┌─ Artifacts ─────────┐             ┌─ Artifacts ─────────┐
│ Files      Reload   │             │ ‹ Files  auth.rs ⛶ ✕│
│ /Users/you/proj     │             ├─────────────────────┤
│ ▾ backend/          │  select     │                     │
│   ▾ src/            │  ────────▶  │   full-height       │
│     ▫ lib.rs        │             │   viewer            │
│     ▫ auth.rs       │             │                     │
│ ▦ sales.xlsx        │  ◀────────  │                     │
└─────────────────────┘  ‹ Files    └─────────────────────┘
```

Locating a file and reading it are sequential, not simultaneous, and every mature
tool treats them that way: VS Code's explorer hands off to a full-width editor,
Finder's column view to a full-screen Quick Look, GitHub's tree collapses when a
blob opens. The earlier layout gave the preview a `max-h-[55%]` strip of a 320px
pane — roughly 350x300px — which no spreadsheet, document, or rendered page can be
read in.

**Both views stay mounted and toggle with `hidden`.** Expansion state lives in the
pane and scroll offset lives on the DOM node, so unmounting would discard both and
every file you looked at would cost you your place. `hidden` also
removes the tree from the accessibility tree and the tab order, so a keyboard user
cannot tab into an invisible tree. `e2e/file-viewer.spec.ts` pins this by
expanding a directory, opening a file, and asserting the directory is still
expanded after Back.

Back keeps the file *selected* rather than clearing it, because the tree derives
its expanded ancestors from the selection — clearing would collapse the path the
user navigated through.

**Fullscreen reuses `Overlay`**, because the focus trap, opener restoration, and
Escape-that-stops-propagating are WCAG 2.4.3 obligations that are easy to get
subtly wrong; there is one implementation rather than two. It is tracked by its own
boolean on the shell rather than a fourth member of the `Overlay` union: the union
answers "which overlay is up", while a viewer also needs to know *which file*, and
that already lives beside it in `artifact`. `runAction` dismisses it alongside the
overlays, keeping at most one modal surface up — the dismissal sits outside that
switch rather than in each arm, because a per-arm call is a rule every overlay
added later can forget, and two of them already did.

**The dialog sizes to what it holds**, through two variants, because content types
differ in a way one rule cannot cover:

| Variant | Sizing | For |
| --- | --- | --- |
| `viewer` | `w-fit h-fit`, capped at 92vh × 95vw, floored at 32rem | Tables, documents, images, text |
| `viewer-full` | 92vh × 95vw outright | Rendered HTML, PDF |

A fixed box strands a seven-row sheet in an acre of white, which reads as broken
even though nothing is: measured, that file now gets 512×265 while a 3000-row
sheet still fills at 578×874, because its own height asks for that. The
`viewer-full` variant exists because an iframe reports *no* intrinsic size, so a
page or a PDF would collapse to the floor under `w-fit` — and those are also the
formats authored for a viewport, so filling one is what they expect.

The 32rem floor is measured, not guessed: the header needs 510px for a filename
plus Reload, expand, and close, so the dialog cannot go narrower without cramping
its own controls. A narrow table therefore leaves real space to its right, which
is why the grid's scroll region is tinted and its cells explicitly white — past
the last column has to read as outside the sheet rather than as cells that failed
to load, which is how every spreadsheet application draws it.

One consequence worth knowing: while the dialog is open the pane shows a
placeholder instead of a second copy of the viewer, so the expand button is
unmounted and `Overlay` cannot restore focus to it — its `isConnected` guard
correctly refuses a detached node. The pane holds a live reference and restores
focus once React remounts the button.

### Artifacts pane: managing files, not only reading them

The pane creates folders, uploads files, and deletes entries. `lib/file-operations.tsx`
owns all three, and the pane mounts it **once** rather than per row.

That is not tidiness. A directory can hold 2000 rows; giving each row its own
dialog state, its own mutation, and its own hidden `<input type="file">` would put
three pieces of machinery on every row to serve the one the user acts on, and would
mount 2000 file inputs. One owner also makes the dialog unambiguous — there is one
modal question on screen because there is one place able to ask one.

It also makes confirmation unskippable. A row cannot *call* delete; it can only
`requestDelete`, and the hook is what decides a dialog comes first. Delete is
irreversible here — the backend has no trash, deliberately — so "every caller
remembered to confirm" is not a property worth relying on.

**Reaching each destination.** The root is the one directory the tree never draws a
row for, so it needs its own affordances:

| Gesture | Destination |
| --- | --- |
| Header **New folder** / **Upload files** buttons | The working directory |
| Right-click the tree's empty space | The working directory |
| Drop files on the tree's empty space | The working directory |
| A row's overflow menu or right-click | That entry |
| Drop files on a folder row | That folder |
| "add files" beside an **Empty** listing | That empty directory |

The buttons are the keyboard and touch route; right-click and drop are the mouse
idiom, added *alongside* them rather than instead. The container carries `min-h-full`
so the blank area below the last row belongs to it — sized to its content it ended
at the final row, and the region that looks most like "the folder's background" did
nothing. Row handlers call `stopPropagation`, or one gesture opens two menus and one
drop uploads twice.

**A mutation reveals its own result.** Uploading into a collapsed folder otherwise
reports success and changes nothing on screen, which reads as a broken feature
rather than a hidden one. That is why expansion state was lifted from the tree into
the pane: `useFileOperations` has to be able to expand the directory it just wrote
to, and its dialogs must sit outside the `hidden` wrapper — a dialog inside a
`hidden` ancestor is both invisible and unfocusable, so a background `showArtifact`
from a tool call would strand it.

**Cache invalidation is deliberately broad**: every `["files", sessionId]` query,
not the parent alone. Only *mounted* directories refetch, so the cost is the subtree
the user is looking at, and it is correct where a targeted invalidation is not — an
overwrite changes a file's size, and a recursive delete removes descendants whose
own queries are still cached.

**Uploads are sequential, not parallel.** Each file is base64 inside a JSON body, so
five 8 MiB files at once is ~53 MiB of encoded payload held in memory while the
event loop is also doing the encoding. Sequential also makes failure attributable:
the user learns *which* file collided, which a `Promise.all` rejection cannot say. A
collision is then treated as a question — a Replace dialog — rather than as an
error, because the user picked those files; retrying with `overwrite` automatically
would destroy a file they never chose to replace.

### Reading a file the pane cannot preview

The preview is bounded on purpose: 1 MiB read, 256 KiB of text returned. A side
panel cannot usefully render a 40 MiB log, and pushing one through JSON stalls the
pane. But a limit the user cannot get past reads as a missing feature, so every
bounded case offers the way through:

- **Open in a new tab** — `files/raw`, which truncates nothing and hands the file
  to the browser's own viewer. It streams, finds, zooms, and prints better than
  anything this pane would reimplement, and it renders text whatever the extension.
- **Download** — the same route with `download=true`. Offered for *every* file, not
  only unrenderable ones: whether the browser can display a type is a different
  question from whether the user wants a copy.

Both sit in the viewer header *and* in the truncation notice itself, because the
reader's next question after "showing the first 256 KB of a 40 MB file" is "so how
do I see the rest", and the answer belongs in the thing raising it. The notice names
both numbers so the reader can tell how much is missing.

`window.open(..., "noopener,noreferrer")` rather than an anchor: these are menu
items, and an anchor inside a `menuitem` either breaks the role or nests interactive
elements. `noopener` is load-bearing rather than habitual — the raw route sandboxes
its response into an opaque origin, and pairing a sandboxed document with a live
`window.opener` handle back to the app is the shape that becomes a navigation
attack. Download assigns `location` rather than opening a tab, since no window is
needed for a response the browser hands to its download manager; and it relies on
`Content-Disposition` from the backend rather than a `download` attribute, which is
same-origin-only and silently ignored otherwise.

### File viewers

| Family | Formats | Endpoint |
| --- | --- | --- |
| Text / code | anything UTF-8 | `files/content` |
| Markdown | `.md` | `files/content` |
| Image | png jpeg gif webp bmp ico avif | `files/raw` |
| Image (SVG) | `.svg` | `files/content`, base64 |
| Grid | xlsx xlsm xlsb xls ods csv tsv | `files/document` |
| Blocks | docx pptx | `files/document` |
| Web page | html htm | `files/content`, sandboxed |
| PDF | pdf | `files/raw` |

The endpoint is chosen before any request, from the `previewKind` the tree listing
already carries, with an extension fallback for files opened from a tool call
(which have no listing). A mislabelled file is the one case a table cannot settle,
so `document` answering `INVALID_REQUEST` falls back to `content`, which decides
text-vs-binary from the bytes.

Five formats reduce to three viewers, which is what makes this affordable against
the bundle budget: a spreadsheet and a CSV are both a grid of typed cells, and a
Word document and a deck are both a sequence of blocks once a slide is modelled as
a section whose blocks follow it.

**SVG is deliberately not on the raw route.** Served as `image/svg+xml` from our
own origin it can execute script; the same bytes inside an `<img>` cannot, so it
keeps travelling the base64 path.

### Rendering untrusted HTML

Workspace HTML may be agent-generated. Rendered with our privileges it could read
the DOM and issue same-origin fetches carrying the `sylph_session` cookie, which
authorizes reads across the whole workdir — a real escalation, not a theoretical
one. It was measured: serving such a file from a same-origin route with no
hardening, a script in it read the cookie.

Rendering is nonetheless the default, because the capability is *removed* rather
than gated:

```tsx
<iframe sandbox="" referrerPolicy="no-referrer" srcDoc={sandboxed.html} />
```

An empty `sandbox` withholds every permission, including `allow-scripts` and
`allow-same-origin`. Verified in a real browser, and pinned by
`e2e/file-viewer.spec.ts`:

| Check | Result |
| --- | --- |
| Markup renders, inline CSS applies | yes |
| Inline `<script>` runs | no |
| `parent.document` reachable | no |
| `document.cookie` in the frame | throws `SecurityError` |

`srcdoc` rather than a `src` URL: the bytes already arrive through the
authenticated JSON endpoint, so no second route serving `text/html` from our
origin needs to exist. A blob URL was rejected for inheriting the creating origin
— a latent footgun the day someone adds `allow-same-origin` — and for needing
revocation to avoid leaking.

`lib/sandbox-html.ts` additionally injects `default-src 'none'` as defence in
depth, so no fetch happens even though nothing can initiate one; it deliberately
does **not** sanitize, because a hand-written sanitizer is a permanent liability
whose bypass list never ends, and it is unnecessary once script execution is gone.

The honest limitation: `srcdoc` inherits the parent's base URL, so
`<img src="./chart.png">` resolves against the app's origin where nothing answers.
The viewer counts such references and says so rather than showing a silently
incomplete page. Inlining them through the authenticated endpoint is the follow-up
if evidence shows it matters.

### Conversation canvas: agent-first, not chat-first

The canvas follows `agent-ui-design-analysis.md`, which compares Claude Code,
Cursor, and Zed against chat UIs. The distinction that drives every decision here:
in an agent workspace the **tool calls are the content**, and prose is what joins
them. So:

| Decision | Reason |
| --- | --- |
| Flat, left-aligned timeline; no bubbles, no alternating alignment | Bubbles spend vertical space separating speakers, which is not the axis a reader cares about. Alternating alignment breaks scanning. |
| Tool calls get a 2px left border; prose gets no chrome | The border is a visual anchor for the thing being scanned for. Prose does not need to be found. |
| Thinking collapsed by default, expanded while streaming | Reasoning is debugging detail once it is finished, and the interesting thing while it arrives. |
| Reads collapsed, edits always open | A read is context-gathering; an edit is the output. |
| Shell output collapsed on success, expanded on failure | On failure the output *is* the answer to "what happened". |
| Turn boundary is a rule with a status line, not a box | A box per turn fragments one continuous work log. |
| Every file path is a button into the Artifacts pane | The path is the natural link between what the agent did and what it did it to. |

A tool call and its `tool_result` render as **one row**, paired by the shared
`streamId` the runner protocol says they SHOULD reuse. A result whose runner did
not reuse the id still renders on its own, so no output is ever dropped.

Tool payloads are interpreted by `lib/tool-shape.ts`. The runner protocol
deliberately leaves `data` unspecified, so that module normalizes what the four
shipped runners actually emit — `data.input` holding the arguments, with `text` as
a stringified fallback — and degrades to "unknown tool with some arguments" rather
than throwing, because a fifth runner will spell something differently.

### Keyboard model

One document-level listener in `lib/workspace-ui.tsx` dispatches every shortcut.
Per-component listeners would fight over the same combos and make precedence
depend on mount order.

| Combo | Action |
| --- | --- |
| `Mod+K` | Global search (live sessions + archive) |
| `Mod+Shift+K` | Browse this workspace's archived history |
| `Mod+P` | Command palette |
| `Mod+Shift+O` | New session (pick a directory) |
| `Mod+J` | Quick Start in `~/.agents` |
| `Mod+B` / `Mod+\` | Toggle left / right pane |
| `Mod+,` | Settings |
| `Mod+.` | Stop the running turn |
| `Mod+F` | Filter turns in this session |
| `Shift+E` / `Shift+C` | Expand / collapse all disclosures |
| `Escape` | Close the top overlay (not rebindable) |

`Mod` is Cmd on macOS and Ctrl elsewhere, resolved at match time. Bindings are
stored platform-neutral, so a stored `Meta+k` cannot silently stop working for the
same user on another machine.

**Some combos the browser takes first, and a page cannot have them.** New session
and Quick Start were originally `Mod+N` and `Mod+Shift+N`, which Chrome has reserved
for a new window and an incognito window since version 4. `preventDefault` does not
reach them, so *both* handlers ran: a new browser window opened, with the app's own
dialog behind it. The current defaults follow ChatGPT and Claude's new-chat binding,
so a user of those arrives expecting `Mod+Shift+O`.

`RESERVED_COMBOS` in `lib/combo.ts` records the set with what each one does instead
— `Mod+N/T/W` and their shifted forms, plus `Mod+Q`. Three consequences:

- The recorder **refuses** a reserved combo *naming what the browser will do*, which
  is the difference between a refusal a user can act on and an arbitrary
  restriction. That is why `reservedReason` returns a string rather than a boolean.
- `pruneReserved` drops them at hydration — a **migration, not a filter**. A settings
  write persists the whole merged map, so anyone who had ever opened that panel had
  `Mod+N` stored and would keep hitting it.
- `Mod+Q` and `Mod+W` are listed but must never be *offered*: they quit and close, so
  a page that captured them would trap the user.

`Mod+Shift+Enter` was considered for Quick Start and rejected: the composer uses
`Cmd+Enter` for newlines and `Mod` combos fire globally even inside a textarea, so
it would have broken typing.

**The suite could not have caught this**, which is the part worth remembering.
Playwright's Chromium opens no real window, so 41 green specs pressed `Mod+N`
happily — the same shape of blind spot as the mobile-breakpoint entry in `AGENTS.md`.
The spec that covers it asserts on the recorder's refusal, which is observable,
rather than on browser chrome, which is not.

Bare-key combos (`Shift+E`) are suppressed while a text field has focus, so the
composer keeps its keystrokes; `Mod`-prefixed combos are global on purpose.

Session-scoped actions — stop turn, expand all, find — cannot be handled by the
shell, which has no access to the open session's state. The session page registers
a handler with `registerSessionActions` and the shell forwards to it; with no
session open, those actions are simply ignored.

### Creating a session: one mutation, and a directory when it is unknown

There is no create form. Every surface runs the same mutation; the only question
is whether the invocation already names a working directory.

| Surface | Path | Why |
| --- | --- | --- |
| ⚡ Quick Start, `Mod+J`, palette | `useQuickCreateSession` | The directory is fixed: the backend-resolved `~/.agents`. |
| Sidebar group → "New session here" | `useQuickCreateSession` | A group **is** a directory. The right-click already named it. |
| `Mod+Shift+O`, empty-state button | `overlay: "newSession"` → same mutation | No directory yet, so a picker asks for that and nothing else. |

A session needs a directory. Nothing else is worth asking at creation time, and
the four-field form that used to ask is worth recording because each field's
removal has a different reason:

- **`workdir`** — the one field a session cannot start without, and the one a text
  input served worst. An absolute path typed from memory fails as a backend
  rejection; `DirectoryPicker` enumerates server-side, so the path is chosen and
  cannot be mistyped.
- **`agent`** — switchable in the composer until the first turn (`AgentSwitcher`),
  which is strictly more forgiving than deciding before the conversation exists.
- **`model` / `effort`** — composer controls on every turn. Choosing once at
  creation was less capable than the surface that replaced it.
- **`title`** — optional, and the sidebar already labels a session from its
  opening message. It stays on the model and the API, because an imported native
  session carries the originating agent's title.

So whenever the invocation carries a directory, a dialog has nothing left to ask
and is only something to dismiss. `lib/create-session.ts` is that mutation, and it
is what makes "Quick Start" a special case of a general action rather than its own
mechanism — before it, Quick Start had a private hook in the shell and the sidebar
routed to the form, which is why right-clicking the fixed `~/.agents` group still
asked for a working directory.

The sidebar group menu carried both variants for one release: "New session here"
created directly, "New session here..." opened the prefilled form. Two entries
with the same words and different behaviour is a coin flip, and the slower one
reads as the careful choice. `workspace-shell.spec.ts` now asserts the menu holds
exactly one.

One instance lives in the shell and is shared through `WorkspaceUIProvider` as
`createSessionIn`, so every dialog-free path reports one pending state and one
error banner. Its value is memoized and passes `mutate` by reference: it feeds the
shell context's memo, which `runAction` closes over, so an unstable identity would
re-register the document-level keydown listener on every shell render.

`resolveSessionDefaults` checks both stored preferences against live `--describe`
output. A `defaultAgent` whose runner was uninstalled would be rejected as an
unknown agent, and a `defaultModels` entry the runner no longer declares would be
passed as `--flag model=...` and fail at turn time — far from the setting that
caused it. Both degrade to a silent, correct fallback instead, and only "no
runners at all" surfaces as an error, because that is the one case with no
sensible default.

### `/` — the start screen, not a session list

`/` used to be "All sessions": a listing with four filter controls (agent, origin,
workdir substring, updated-after date). It was replaced by an IDE-style start
screen, and the reason is that the list had become a second, weaker copy of
navigation the shell already owned:

| The list offered | The shell already offers |
| --- | --- |
| Every live session, `LIMIT`-capped | The sidebar, grouped by the directory each session runs in, plus that directory's archived history |
| Filter by workdir substring | The sidebar's grouping *is* the workdir axis |
| Filter by agent / origin / date | `Mod+K` searches live sessions and the archive by content, which is the question users actually arrive with |
| A row per session | `Mod+Shift+K` pages one workspace's history, which the 50-row cap could not |

A user opening the app has not asked to filter sessions by date. They have arrived
to **start** work or **resume** it, and the filter form answered neither. It also
cost two `GET /api/v1/sessions` round trips on first paint — the second only to
populate the agent dropdown from an unfiltered listing, since deriving it from
filtered results would collapse the menu to the agent already selected.

What replaced it has three tiers, and two properties that constrain the design:

1. **Identity and what a session is.** One line: a session pairs an agent with a
   working directory, and archived history exists for directories agents have run
   in. This is the only place the app states it, and it is not guessable — a user
   who does not know it points a session at the wrong tree.
2. **The two create paths**, as buttons running `runAction`, each showing its live
   binding. Not links to a route: creation is a mutation, and pending and failure
   belong in the shell's one banner.
3. **The variable slot** — recents, or the first-run guide — then the key bindings.

**It paints before any request settles.** Tiers 1, 2, and the key list are static.
Only the middle slot waits on data, and it reserves its height rather than
collapsing, because a start screen that reflows on every launch reads as broken.

**It adds no request.** The listing is the sidebar's, shared through
`lib/sessions-query.ts` and deduped by TanStack. Rendering `/` costs zero extra
round trips, which is what lets it be the default route.

#### Pending is not empty

`selectStartState` (`lib/welcome.ts`) exists because an in-flight listing and an
app with no sessions are both an empty array, and rendering the onboarding guide
for the first flashes "getting started" at a user with hundreds of sessions, on
every navigation to `/`. A failed listing is treated the same way: failing to load
sessions is not evidence that none exist, and the sidebar already reports the
failure with a retry. It is a pure function with unit tests, because the branch is
one line of JSX away from being wrong and E2E cannot economically hold a listing
open.

#### First run teaches the first task, not the feature set

The empty state is three numbered steps — choose a directory, say what you want
done, watch the work — plus a pointer to browsing existing agent history without
creating anything. Numbered because the order is real: a directory must exist
before a turn can run.

The Notion-style property worth borrowing is not the tour, it is that onboarding is
**the empty state itself** rather than a dismissible layer. There is no "seen"
flag, no banner, and no preference: the guide is what having no sessions looks
like, so it disappears permanently after the first create and can never be stale.

#### Recents are five *directories*, not five sessions

The unit is the directory, because resuming work means returning to a place. One row
per session looked right until a real listing was on screen: a directory worked in
all week filled all five rows with entries differing only by a timestamp — and since
`title` is set only by native import or the API, most sessions are untitled, so
those rows were otherwise identical. Five rows for one destination, and every other
place the user works pushed off the list.

So a row is a directory, labelled by its path, and it opens the newest session
there — which is what "resume" means. The rest are a count (`3 sessions`); reaching a
specific older one is the sidebar's job, and it lists them all under that same
directory. `recentWorkspaces` normalizes trailing slashes, because the backend does
not guarantee a spelling and `/w/app` beside `/w/app/` is the same duplication in
another form.

The path carries the row's identity rather than `sessionLabel`, whose `agent · leaf`
fallback reads as the directory named twice once the path is beside it. `native`
origin is badged because it changes identity and lifecycle policy and must be
recognizable before opening; `sylph` is not, because a marker on every row carries no
information.

A longer list is not the improvement it appears to be: the sidebar lists everything
grouped this same way, and `Mod+K` searches all of it.

#### The pending slot reserves height; the loaded one does not

The reservation lives on the pending branch, not on the container. A container tall
enough for five rows leaves visible dead space above the key list for every user who
works in three directories — on screen every visit, which is strictly worse than the
one-time settle it was preventing. Three rows' worth is reserved, so a cold load
moves by a small amount in either direction rather than always dropping.

### Component Design

#### Timeline rows

`components/timeline.tsx` holds the whole canvas as one flat list of row renderers.
There are no message components, no avatars, and no per-message action bars: those
belong to the chat shape this canvas rejected, and the reasoning is under
"Conversation canvas: agent-first, not chat-first" above.

| Row renderer | Renders | Expansion |
| --- | --- | --- |
| `UserPrompt` | the typed ask, with attachment chips | always open |
| `AssistantText` | prose through `Markdown`, streaming-aware | always open |
| `ThinkingBlock` | reasoning | collapsed by default |
| `ToolCall` | one operation: the call paired with its `tool_result` | by role, see below |
| `OrphanResult` | a `tool_result` whose call never arrived | open |
| `UsageLine` / `StatusLine` | token counts, lifecycle notices | single line |

`ToolCall` is the anchor row. It draws a bordered region carrying the tool's
target (`ToolTarget`) and outcome (`StatusMark`), and decides its own expansion
from the shape rather than from a uniform default: edits open with their diff,
shell output opens only on failure, reads stay closed. The pairing and the target
extraction are not in the component — `lib/tool-shape.ts` normalizes all four
runners' argument spellings into one `ToolShape` (`describeTool`, `resultStatus`),
which is why that logic is unit-tested and the component is not.

`MemoizedRow` wraps each row and is keyed on event identity, so a delta arriving
mid-turn re-renders one row instead of the settled history above it. That is the
mechanism the virtualization decision rests on.

#### Shared primitives

`components/ui/` holds what was previously re-decided per file: roughly forty
hand-written button class strings that were not identical — padding drifted
between `px-2 py-1` and `px-2.5 py-1`, disabled state was sometimes `opacity-50`
and sometimes nothing, and hover was `gray-100` in the top bar but `gray-200/60`
in the sidebar for the same kind of control.

| Module | Exports | Notes |
| --- | --- | --- |
| `ui/button.tsx` | `Button`, `IconButton`, `Kbd` | Variants are semantic (`primary`, `secondary`, `ghost`, `danger`, `chip`), so a caller picks by meaning. `IconButton` requires `label` — an icon-only control with no accessible name is the most repeated shape in the app, so the failure is a type error rather than an audit finding |
| `ui/menu.tsx` | `ContextMenu`, `Menu`, `MenuButton`, `MenuItem` | One behaviour, three positionings: anchored to a point (right-click), to a trigger (dropdown), or as a bottom sheet on mobile |
| `ui/sheet.tsx` | `Sheet`, `Drawer` | The mobile surfaces |
| `ui/dialog.tsx` | `AlertDialog`, `PromptDialog` | Decision dialogs: confirm a destructive action, ask for one short value. See below for why they are two components |
| `lib/use-dialog.ts` | `useDialog` | Focus trap, focus restoration, Escape |

**`ui/dialog.tsx` replaced `window.confirm`**, which was the obvious first answer
for "confirm delete" and wrong on every axis that matters here. It blocks the main
thread, so the SSE stream driving the timeline stalls while it is up. It cannot be
styled, and on iOS it names the origin — "localhost:3000 says" — which is the same
"assembled from different kits" quality that removing the composer's `<select>`
elements fixed. Playwright cannot see into one without a dialog handler that
swallows real failures too. And it offers only OK/Cancel, so the *name* of the
action is not on the button the user presses, which is what makes a destructive
confirm safe to read quickly.

`AlertDialog` and `PromptDialog` are separate because they differ in what holds
focus on open, and that is not cosmetic. A confirm focuses **Cancel**, so a
keystroke already in flight when the dialog appears cannot delete anything; a
prompt focuses its input, because typing is the task. One component with a flag
would put that behind a boolean a caller has to know to set, and the safe default
is the one nobody sets.

Getting that right required ordering the footer as Cancel-then-confirm in the DOM
and right-aligning with `ml-auto`. The first version used `flex-row-reverse` so the
affirmative button could be written first and still land on the right — which made
the *destructive* button the first tab stop, and therefore the one `useDialog`
focuses. `e2e/file-operations.spec.ts` asserts Cancel has focus, which is what
caught it.

`useDialog` exists because three surfaces had implemented this independently and
disagreed in ways only a keyboard user finds. `CommandDialog` queried `"button,
select, input, textarea, [href]"`, which matches a *disabled* button and could
park focus on something inert; `Overlay` filtered on `offsetParent`, which is
correct. Neither stopped Escape from also reaching the shell's document handler,
so closing a dialog could close the overlay behind it. These are WCAG 2.4.3 and
2.1.2 obligations, and the drawer and sheet would have been a fourth and fifth
copy.

One subtlety worth recording, because it produced an unclosable dialog: the hook
focuses the *container* when nothing inside the surface claims focus, even with
`autoFocus: false`. `onKeyDown` is a React handler on the container, so with
focus left on `<body>` the Escape key never arrives. The full-screen file viewer
has no auto-focusing control of its own, so it opened and could not be dismissed
by keyboard at all.

#### Menus replaced every `<select>`

Not a styling preference. **A `<select>` is sized by its longest option, not by
its current value.** The composer's agent, model, and effort controls were
native selects, so a runner declaring `claude-opus-5-20260501` made that control
~180px wide regardless of what was selected; three of them plus Send exceeded a
390px viewport. Because the width came from content the user could not see, no
amount of `min-w-0` or `truncate` could recover it — the intrinsic minimum was
already too large. This is the direct cause of the reported "composer runs off
the screen".

A button that renders the *current* value can truncate. A `<select>` cannot. The
secondary benefit is that a native select cannot be styled: on iOS it renders as
a system wheel with its own typography, which is exactly the
assembled-from-parts quality this pass removed.

Menus use `menu`/`menuitem` with `aria-activedescendant` for the virtual cursor,
and `menuitemradio` + `aria-checked` where a group is a single choice. Two bugs
found while building this are worth keeping:

- **A menu measured before it is placed cannot take focus.** The panel carries
  `invisible` until its position is measured, and `visibility: hidden` elements
  are not focusable — so focusing inside the measuring effect silently did
  nothing, and Escape never reached the handler. Focus happens in a second
  effect, once `position` commits.
- **A dismiss-on-outside-pointerdown listener must be inert on the sheet path.**
  The ref it tests is the popover's, which the sheet path never attaches, so
  every `contains()` was false and the very pointerdown beginning a tap on a
  menu item closed the sheet before the click landed. Selecting a model silently
  did nothing on a phone.

### Icons

`components/icons.tsx` — 48 hand-authored icons on a 24-unit grid, drawn
at 16px, stroked in `currentColor` at 1.75. No dependency.

The app previously drew its entire interface with Unicode dingbats: `⚙ ⚡ ⌕ ▾ ⌗ ⑂
⚠ ⛶ ✕ ↗ ↪ ✻ ❯ ✎ ◆ ▤ ▦ ▩ ▧ ◈ ▪ ▫ ▮ ⎘ 📎` and more. The budget reasoning behind
that was sound — an icon package did not fit — but the conclusion was wrong,
because those characters are **font data**: what they look like is decided by
the platform, not by this project. `📎` and `⚡` render as full-colour emoji on
iOS and macOS, several of the geometric shapes are absent from the default CJK
fallback chain and came out as `.notdef` boxes, and the ones that did render
were semantically arbitrary — `▩` for an image and `▧` for a PDF are shapes with
no relationship to their meaning, so they had to be learned rather than
recognized.

The real choice was never "dingbats or a 40 KB package". Thirty-odd paths cost
about 2 KB and tree-shake per route.

Conventions worth keeping:

- **Every icon is `aria-hidden`.** None is ever the only thing carrying a
  meaning: the control around it has a label, or a text node sits beside it. One
  rule rather than a per-icon judgement, so an icon cannot become the sole
  accessible name by accident.
- **1.75 stroke, not Lucide's 2 at 24px.** A stroke thinner than ~1.5 device
  pixels dithers on a non-retina display. Round caps and joins throughout,
  because mitred joins at this size produce visible spikes at acute angles.
- **Presentation stays out of the logic modules.** `lib/tool-shape.ts` returns a
  `kind` and `lib/file-kind.ts` returns an icon *name*; the rendering layer maps
  either to a component through a module-level table. Both modules are
  unit-tested in a `node` environment, and a component reference returned from a
  function and rendered as `<Glyph />` is also indistinguishable, to
  `react-hooks/static-components`, from a component defined during render.
- **Settings is sliders, not a gear.** A gear needs eight teeth to read as one,
  and at 16px those merge into a blurred ring.
- **Folder and file are different silhouettes**, not the same box with different
  infill: the tab along a folder's top edge is visible at a glance where an
  interior glyph is not, and the folder opens when its row expands. Colour is
  reinforcement, not the distinction, so it survives greyscale (WCAG 1.4.1).

**Hand-authored paths need a visual check, and nothing else substitutes.** A
transposed coordinate type-checks, renders, and is simply wrong; an E2E test
asserts an icon is *present*, not that it depicts anything.
`scripts/icon-sheet.mjs` renders every icon at 28px and 16px in both themes to
`/tmp/icon-sheet-*.png`. Run it after touching an icon. It found five broken
ones on its first run: `Command` was an inkblot, `Chip` a flower, `Tool` a
dagger, `FileImage` read as "blocked", and `FolderOpen` was indistinguishable
from `FolderIcon` — which is the one distinction the file tree depends on.

That sheet re-parses the source, so it can misreport a shared sub-component.
When an icon looks wrong there, screenshot the real component before changing
it: doing that revealed a genuine layout bug the sheet could not — an expanded
directory's `<li>` contains its whole subtree, so an `inset-y-0` overflow button
positioned against it stretched down the subtree and rendered beside a *child*
row.

A few characters are deliberately *kept*, because they are text rather than
pictures: `›` as the prompt marker (the same convention a shell prompt uses),
`↑↓↵` in keyboard hints, `+`/`−` in diffs (required for colour-blind readers),
and `·`/`⋯` as punctuation.

### The composer

One bordered surface holding attachments, the message, and the runner
configuration that turn will use. The layout rule is that **the message owns its
own row** and the controls sit beneath it, which is what makes it usable on a
phone.

```
┌──────────────────────────────────────────────┐
│ ▣ screenshot.png  1.2 MB              ✕      │  attachments (when present)
├──────────────────────────────────────────────┤
│ Add logging to the auth module…              │  textarea, auto-grows to 35vh
│                                              │
├──────────────────────────────────────────────┤
│ [+]  [▣ thorough ⌄] [✦ high ⌄]      [↑ Send] │  controls; middle scrolls
└──────────────────────────────────────────────┘
   │      └── declared flags, truncating chips
   └── attach · save to workspace · run command · switch agent
```

Split by how often a turn changes it, following ChatGPT and Claude:

- **Always visible**: the message, `+`, model, thinking, Send/Stop. Model and
  thinking are per-turn decisions that change what the message will *do*, so
  hiding them costs a click on the majority path and makes the current model
  invisible mid-conversation.
- **In the `+` menu**: attach a file, save attachments into the workspace, run a
  command, switch agent. Each is occasional or session-scoped.

The `+` replaced a paperclip. A paperclip claims the menu holds only
attachments, so nothing else could ever be put there; `+` is also the affordance
a phone user arrives with from every messaging app.

The control row is `flex-nowrap` with a **horizontally scrolling middle
section**, not `flex-wrap`. Wrapping would make the composer's height depend on
how many flags a runner declares, so the transcript would reflow when a model
name changed. Scrolling keeps the height fixed and Send pinned. Only the `model`
and `effort` categories are promoted to the row; anything else the agent declares
still gets a control, in the menu — so the row is a fixed shape no matter what an
agent reports.

The row is driven by the session's **live typed config**, not by the agent
descriptor's `flags`. The two answer different questions — what can be chosen
before a session exists, against what this running session reports — and only the
live answer is knowable: `claude` declares an empty `model.choices` in its
descriptor, because its catalogue depends on the account and is not known until the
CLI has loaded. A descriptor-driven model control was therefore permanently empty.
A descriptor flag the live config already covers is dropped rather than rendered
twice; two controls for one setting disagree the moment either is used, and nothing
on screen says which won.

### Two kinds of command

The command surface is one inline menu, not a dialog. A slash command is a *way of
writing a message*, so the menu is a listbox the textarea owns and announces
through `aria-activedescendant` — focus never leaves the input, and Tab completes
the way it does in the terminal this is modelled on.

What a command does depends on its `kind`, which the agent reports
(`docs/session-protocol.md` § Two kinds of command):

- A **prompt** command runs as a turn. Its output is durable history.
- A **control** command opens `config-picker.tsx` and never becomes a turn.

That split is the whole reason `/model` works. Sent as prompt text to a headless
CLI it can only answer in prose, because there is no picker on that side of the
wire — so the picker has to be the client's, driven by the typed option the command
names. Nothing in the frontend names a model, an effort level, or a permission
mode; it renders whatever options the session declared, grouped by their own
`category`.

An unknown command is deliberately **not** intercepted: it goes through as text and
the agent says it does not recognize it. Intercepting would mean this UI deciding a
command does not exist, when the agent is the only thing that knows and the list
can be stale by a keystroke. Aliases are matched, because the agent honours them —
`/cost` and `/stats` are `/usage`.

A previous `command-dialog.tsx` ran commands from the *static* descriptor. It was
removed rather than fixed: that list carries no `kind`, so it would have sent
`/model` as a turn, which is exactly the defect the split exists to prevent.

Two mobile-specific behaviours:

- **Return inserts a newline on a phone; the Send button sends.** Enter-to-send
  on a soft keyboard means a message cannot contain a line break at all — Return
  is the only way to type one, and there is no Shift to hold. The "Enter to
  send" hint is hidden there, because a hint describing the wrong platform is
  worse than none.
- **Send is icon-only below `md`.** The label costs the width the chips need,
  and an upward arrow in a filled circle is the least ambiguous control in any
  messaging app. `aria-label` carries the name either way.

The disabled state uses a muted *background*, never `opacity`. Opacity
composites the whole subtree, so every descendant's contrast drops at once:
measured by axe during a live turn, muted text fell to 2.87:1 and the Stop
button to 3.7:1 — both below WCAG 1.4.3's 4.5:1, and Stop is the one control a
user needs *while* the turn runs.

### PWA

Installable, so a phone user gets an icon and a standalone window rather than a
tab they have to find again.

| Piece | Where | Why |
| --- | --- | --- |
| Manifest | `app/manifest.ts` | A route handler, not a static file, so `theme_color` stays in step with the `--canvas` token from one place |
| Icons | `public/icons/*.svg` + generated PNGs | 192 and 512 for the manifest, 180 for `apple-touch-icon`, plus a separate maskable 512 |
| Generator | `scripts/generate-icons.mjs` | Rasterises the SVGs through Playwright's Chromium |
| Service worker | `public/sw.js` | Installability on Android, and an honest offline page |
| Offline page | `public/offline.html` | Standalone, hand-styled, cached by the worker |
| iOS metadata | `app/layout.tsx` | iOS reads these rather than the manifest |

**`purpose: "any"` and `purpose: "maskable"` are separate files.** A launcher
crops a maskable icon to its own shape — a circle on Android, a squircle on iOS
— so its artwork must sit inside the safe zone with bleed around it. Declaring
one file as both means either the browser crops a full-bleed icon and cuts the
mark, or shows a padded icon unpadded and the mark floats small in a large box.
The maskable variant scales the mark to ~62% and keeps a full-bleed square
background.

**The PNGs are committed, not built.** They change roughly never, and a build
step needing a browser to produce a favicon would put Playwright on the critical
path of `next build` — including in E2E and in `scripts/install.sh`. `sips`
cannot read SVG and neither ImageMagick nor rsvg is present, so the generator
uses the Chromium already installed for Playwright, which is also the renderer
that will display them.

**The service worker caches almost nothing, on purpose.** Sylph Web is
local-first but not backend-independent: every screen needs the backend, so a
cached shell without it is a shell that cannot do anything. Aggressive
precaching would buy no working offline mode and cost a well-known failure — a
stale HTML document pinning an old JS chunk hash after an upgrade, presenting as
a blank page a reload does not fix. So it is network-first for navigations, with
one cached document as the offline fallback.

**`/api/v1/*` is never intercepted**, for two independent reasons either of
which suffices. The turn event stream is a long-lived `text/event-stream`, and
passing a streaming response through `fetch()` inside a worker risks buffering
it — the same class of bug the `no-transform` header exists to prevent,
presenting as a rendering fault rather than a transport one. And the session
cookie's token rotates on every backend restart, so a cached API response could
outlive the token that authorised it.

Registration is production-only. Workers are scoped per *origin*, and
`localhost` is shared with every other project on the machine, so a worker
registered at port 3000 in development outlives this app and is served to
whatever occupies that port next.

### Design tokens

Every colour resolves through a semantic token declared in `app/globals.css`. No
component names a palette step: `bg-gray-50` and `text-gray-500` do not appear
outside that file, and a build that reintroduces one is a regression.

This replaced 28 files that each hardcoded `gray-*`, 139 uses of **10
different** ad-hoc pixel font sizes (including `9.5px`, `11.5px`, and `12.5px`),
and 92 copies of one focus-ring class string. The cost of that structure was not
aesthetic: it made every visual change a 28-file edit, which is why several had
been applied inconsistently, and inconsistency at that scale is what reads as
unfinished.

| Group | Tokens | Meaning |
| --- | --- | --- |
| Surface | `canvas`, `sunken`, `raised`, `hover`, `active` | Back to front: the page, a recessed well (sidebars, code blocks), anything sitting on the page (menus, the composer), then two interaction states |
| Border | `line`, `line-strong` | Hairline, and the stronger edge for a control |
| Ink | `ink`, `ink-soft`, `ink-muted`, `ink-faint` | By emphasis. `faint` is **decoration only** — it does not clear 4.5:1 on any surface |
| Accent | `accent`, `accent-hover`, `accent-ink`, `accent-ink-muted` | The primary action and text placed on it. Inverts in dark mode, which is why `accent-ink` exists rather than a hardcoded `text-white` |
| Status | `danger`, `success`, `warning`, `info`, `native`, each with `-bg`, `-line`, `-solid` | One ink per hue; the tint and edge derive from it |
| Focus | `focus` | Blue, not the accent grey — see below |

`*-bg` and `*-line` are `color-mix()` against `--canvas` rather than hand-picked
steps, so the dark theme gets correct tints for free: overriding the base hue
and the canvas re-derives both.

**The light palette is byte-identical to what it replaced.** It is Tailwind's
own `gray`/`red`/`emerald`/`amber`/`blue`/`violet` steps copied verbatim in
oklch. That was deliberate: several components carry measured WCAG contrast
ratios in their comments ("gray-500 on gray-900 measures 3.66:1"), and a token
that merely *looked* similar would silently invalidate every one of them. Only
dark mode introduces new values.

Two consolidations changed meaning rather than just spelling, and both were
deliberate:

- **`ink-muted` is gray-600, not gray-500.** Both were in use for the same job.
  gray-500 measures 4.83:1 on white and fails on `sunken`; gray-600 is 7.4:1 and
  passes on both.
- **`stopping` and `interrupted` share the warning hue.** They were amber and
  orange — two adjacent hues, indistinguishable at badge size, carrying no
  recoverable meaning. The distinction that matters is "is this still moving",
  so `stopping` keeps the pulsing dot that `running` has, and hue is not asked
  to carry it.

#### Why the palette lives in plain `:root`

Tailwind 4 can tree-shake unused `@theme` variables out of the emitted CSS. A
token referenced *only* by another token's `color-mix()` — every `*-bg` and
`*-line` — is exactly what that would drop, and the failure mode is an invisibly
wrong colour rather than a build error. So the values live in a plain `:root`
block Tailwind never rewrites, and `@theme inline` maps them onto utility names.

#### Dark mode

`<html data-theme>`, set by a **blocking inline script** in `app/layout.tsx`
before first paint. It cannot be a React effect: an effect runs after paint by
definition, so a dark-mode user would see a white page for the length of
hydration — long enough on a cold load to read as a flash of the wrong app.
`lib/theme.ts` builds that script from the same `STORAGE_KEY` the settings store
uses, so the pre-paint read and the in-app read cannot drift.

The attribute goes on `<html>` specifically, not `<body>`: a custom property
containing `color-mix()` substitutes on the element carrying the declaration, so
setting it lower would leave every derived tint mixed against the *light* canvas
while the base hues had gone dark.

Dark is not an inversion. `canvas` is a very dark blue-grey rather than black so
`raised` can lift above it without going grey, and the ink ramp is compressed at
the top because pure white on a dark surface halates. Measured against `canvas`:
ink 17.6:1, `ink-soft` 11.4:1, `ink-muted` 6.2:1, every status ink above 7:1,
and `ink-faint` 3.4:1 — decoration-only, exactly as in the light theme.

Settings offers Light / Dark / System. `System` keeps following
`prefers-color-scheme`, so a machine on a light-at-day schedule changes with it;
an explicit choice overrides permanently.

### Type scale

Five steps, all on the scale: `text-3xs` (10px), `text-2xs` (11px), then
Tailwind's `xs`, `sm`, `base`. The two smallest exist because dense chrome
genuinely needs them; what did not need to exist was ten different sizes.
Nothing smaller than `3xs` has a token, because it is not readable.

Timeline density maps onto the same scale — compact `xs`, normal `sm`,
comfortable `base` — replacing two one-off sizes (`13px`, `15px`) that existed
only there and were within a pixel of a step.

### Focus

One utility, `focus-ring`, replacing 92 copies of `focus:outline-none
focus-visible:ring-2 focus-visible:ring-gray-900`.

It is an **outline**, not a ring, and that is a correctness fix rather than a
preference: a ring is a `box-shadow`, so it is clipped by every
`overflow-hidden` ancestor — which is why the focus indicator was invisible on
file-tree rows and inside the code-block header. An outline is never clipped.
The colour is blue rather than the accent grey, because a grey ring is invisible
against the accent surfaces it has to appear on.

### Typography

A system stack, no webfont: `system-ui`, then `-apple-system`, `Segoe UI
Variable Text`, and CJK faces (`PingFang SC`, `Hiragino Sans GB`, `Microsoft
YaHei`) before the generic fallback.

Three reasons, in order of weight. A local-first app should not need the network
to render text; an installed PWA must work offline; and `next build` runs in E2E
on whatever machine is to hand, so a build-time font fetch is a build-time
network dependency. The CJK entries matter because this project's users read
Chinese — without them a mixed string falls back per-glyph and one line ends up
in two faces.

## State Management

### Server State (TanStack Query)

Query keys follow a hierarchical structure:

```ts
// Live sessions. Declared once in `lib/sessions-query.ts` and shared by the
// sidebar, the start screen, and the search overlay, so the three mount as one
// request rather than three. Hand-written keys deduped only while every copy of
// the limit agreed, and the drift was silent.
['sessions', { limit }] → useQuery(liveSessionsQuery())

// Session detail
['sessions', id] → useQuery

// Agents
['agents'] → useQuery

// Turn detail
['turns', turnId] → useQuery
```

**Query invalidation:**
- New session created → invalidate `['sessions']`
- Turn completed → invalidate `['sessions', sessionId]`, `['turns', turnId]`
- Agent refresh → invalidate `['agents']`

**Optimistic updates:**
- Send message → immediately append to local cache
- Stop turn → immediately update status to 'stopping'
- Revert on error via `onError` rollback

### Client State

Two stores, neither of them Zustand.

**`lib/workspace-ui.tsx` — shell state, React context.** Which panes are open,
which overlay is up, which artifact the right pane shows, and the session-action
handler registration. This lives above the page tree because the shell outlives
navigation, and because a conversation event needs to ask the Artifacts pane to
open a file — a request that has to reach it from outside its own subtree.

**`lib/settings.tsx` — preferences, `useSyncExternalStore` over `localStorage`.**
Default agent and model, density, expansion defaults, shortcut bindings, pane
visibility, pinned workspaces, workspace display names.

The external-store hook rather than state-plus-effect, for three reasons:

1. Reading storage in an effect and calling `setState` cascades an extra render on
   every mount, which is what `react-hooks/set-state-in-effect` flags.
2. The server snapshot is a separate stable value, so hydration cannot mismatch —
   the failure mode of reading `localStorage` in a lazy initializer.
3. Subscribing to the `storage` event makes a change in one tab reach the others,
   which a per-component effect cannot do at all.

Stored JSON is merged over the defaults **field by field**, each checked against
its own shape. A blob written by an older build is missing keys a newer one needs,
and a hand-edited one can hold anything; spreading it wholesale would let
`density: 42` through and break rendering. Custom shortcuts layer over the
defaults rather than replacing them, so a binding added in a later release still
works for an existing user.

Preferences are client-side by design: every value is a display or input
preference, and none changes what the backend does with a session. The trade-off is
that they do not follow a user to another browser — acceptable for a loopback-only
single-user app, and not acceptable once a non-loopback deployment adds real
accounts, which is already gated behind a separate authentication change.

Pane visibility is read straight from settings rather than mirrored into local
state: the mirror needed an effect to adopt the stored value after hydration, and
could disagree with storage when a write failed.

### Internationalization

English is the source of truth and the fallback; Chinese is optional. The locale is
a settings field, so it rides the store above — no provider, no context, and
switching language re-renders exactly the components that read a translation.

```
lib/i18n/locale.ts   Locale set, tags, message lookup, format, formatParts.
                     No React, no settings import.
lib/i18n/messages.ts en — the `Dictionary` type and the source of truth
lib/i18n/zh.ts       zh — typed as a total `Dictionary`
lib/i18n/index.ts    "use client": useT(), useLocale(), re-exports
```

**The `locale.ts` / `index.ts` split is load-bearing, not cosmetic.** `lib/settings`
must know what a `Locale` is in order to store and validate one, while `useT` must
read `lib/settings` to know which locale is active. Keeping the primitives free of
both React and the store makes the dependency run one way. It is also the half worth
unit-testing: placeholder substitution and fallback are pure functions, testable
without rendering.

**An untranslated key is a build error, not a surprise in the UI.** `zh` is typed as
a total `Record` over the key set, so adding an English string without its
translation fails `npm run build`. What the compiler *cannot* catch is a translation
that drops a `{placeholder}` — that compiles and renders a sentence with a hole in
it — so a unit test asserts placeholder agreement across catalogues. Both catalogues
currently sit at 263 keys, verified equal.

**No framework.** `next-intl` and `next-i18next` are built around locale-in-the-URL:
a `[locale]` segment, negotiating middleware, per-request catalogue loads on the
server. None of it applies. This is one user's loopback tool, its language sits
beside theme in `localStorage`, and every string lives in a client component.
Adopting that model would mean restructuring `app/` around a segment carrying a
value storage already holds. What a framework would genuinely buy — ICU
MessageFormat, per-locale splitting, plural and date rules — is either already
provided by `Intl` (see `lib/format.ts`) or not worth its weight at this size: both
catalogues together are a few kilobytes gzipped, less than the machinery to load one
lazily. Revisit at a third locale with translators who need a standard file format,
or past roughly a thousand keys.

Three details that are easy to get wrong:

- **Locale names are endonyms** (`English`, `中文`), so the picker's own labels are
  deliberately untranslated. Someone who has switched into a language they cannot
  read still needs a way back, which they only have if the entry they want is
  written in the language they know.
- **`<html lang>` syncs client-side** via `applyLocale`. The attribute is not
  decoration: a screen reader picks its voice from it, so a Chinese UI still tagged
  `en` is read aloud as English attempting Chinese. The document ships `lang="en"`,
  correct for the default.
- **Substitution is one regex pass, never a template literal or `eval`.** These
  values reach the DOM, so a substitution that could execute would turn a
  translation file into a script. An unmatched placeholder is left verbatim, which
  is visible in the UI and therefore gets fixed.

**A sentence with a value in a mono face cannot be a prefix key plus a suffix key.**
English precedes the value ("Inside `src/`"), Chinese wraps it ("在 `src/` 内"), so
the halves are not independently translatable and a translator cannot see the whole
sentence. `formatParts` splits the *translated* string around one placeholder
instead, keeping the sentence whole in the catalogue; `Around` in
`lib/file-operations.tsx` renders the result. An absent slot yields the whole string
as the prefix — visibly wrong rather than silently empty.

**Pure modules take `t` as a parameter rather than reading the locale.**
`describeWindow` and `describeBlocked` do, which moves their English plural rules
("1 row" vs "2 rows") into the catalogue where a translator can see them while
leaving both functions unit-testable. `describeKind` in `lib/file-kind.ts` returns a
`MessageKey` and lets the caller translate it, for the same reason. The one
exception is `next/dynamic`'s `loading` fallbacks: they are created at module scope
where no hook can run, so `LoadingKind` reads the locale itself.

**Backend `message` strings pass through untranslated, by design.** They are one
process's report of what it refused; localizing them would mean cataloguing every
string the API can emit. Both call sites say so in a comment.

## Real-Time Event Streaming

### SSE Connection Management

The stream is read with `fetch` and parsed in the client, **not** with
`EventSource`. This is a correction: the original design here specified
`EventSource` with one `addEventListener` per event kind, and the implementation
that followed it listened on `onmessage` instead — which by the SSE spec receives
only frames named `message`. Since the backend names every frame after its kind
(`event: thinking`), every mid-turn event was dropped, and the transcript
appeared all at once when the turn finished because the only repaint was the
history refetch on completion.

`EventSource` is the wrong primitive here for three reasons, not just that one:

1. Dispatch is by frame name, so a client must enumerate every kind it expects.
   A kind the backend adds later is silently dropped — and silent event loss
   renders identically to an agent that did nothing.
2. A frame named `error` (which the backend emits for a failed turn) arrives as
   the same `error` event the browser fires for a dropped socket, so a runner
   failure and a network failure cannot be distinguished.
3. It reconnects on its own with no way to advance `after_seq`, so resuming at a
   cursor requires fighting its built-in retry.

Reading the body directly fixes all three: the frame name is parsed but never
used for dispatch, so every frame reaches the consumer whatever it is called.

```ts
const response = await fetch(
  `/api/v1/turns/${turnId}/events?after_seq=${cursor}`,
  { credentials: "include", signal: controller.signal, cache: "no-store" },
)

const reader = response.body.getReader()
// `stream: true`: a chunk boundary can fall inside a multi-byte UTF-8 sequence.
const utf8 = new TextDecoder()
const decoder = createSSEDecoder()   // lib/sse-parse.ts

for (;;) {
  const { done, value } = await reader.read()
  if (done) break
  for (const frame of decoder.push(utf8.decode(value, { stream: true }))) {
    const event = JSON.parse(frame.data)
    if (event.seq <= cursor) continue   // drop replay overlap after reconnect
    cursor = event.seq
    enqueue(event)                      // coalesced, see below
  }
}
```

**Compression must be refused, or none of this streams.** The backend sets
`Cache-Control: no-cache, no-store, no-transform` on the event stream. The
`no-transform` directive is load-bearing rather than cosmetic: Next.js proxies
`/api/v1/*` through the standard `compression` middleware, which gzips any
compressible content type including `text/event-stream`, and a gzip stream cannot
be decoded until its block is flushed. With compression on, every event sat in
the compressor until the turn ended — while `curl`, which sends no
`Accept-Encoding`, saw perfectly paced frames and made the bug look like a client
problem. The header lives on the backend response rather than in
`next.config.ts` so the guarantee survives any proxy in front of the endpoint.

**Repaint is coalesced, not per-event.** Deltas arrive at token rate — the claude
runner measures ~20ms between tool-input deltas — so one `setState` per delta is
one React commit per token. Events are batched into a single flush per animation
frame (16ms), which bounds repaints by the display rather than by the model's
speed while staying far below the ~100ms at which a human perceives lag.

**Event normalization:**
- Parse JSON from `data` field
- Track sequence numbers for replay
- Distinguish `started` / `delta` / `completed` phases
- Merge deltas into live projection
- Persist only `completed` events

**Reconnection strategy:**
- Backoff: 500ms, 1s, 2s, 5s, 10s (capped, so a dead backend is polled gently)
- Resume from the highest `seq` seen, via `after_seq`
- Replay stable events from backend journal
- A clean end-of-body means the turn finished; retrying there would loop
- `AbortController` makes teardown immediate, which `EventSource` cannot offer

### Event Rendering

**Streaming text:**
```tsx
function StreamingText({ turnId, kind }: Props) {
  const liveText = useStreamStore(s => s.liveEvents.get(turnId)
    ?.filter(e => e.kind === kind && e.phase !== 'completed')
    ?.map(e => e.text)
    ?.join(''))
  
  const finalText = useQuery(['turns', turnId, kind])
  
  return (
    <div className="animate-typing">
      {liveText || finalText}
      {liveText && <Cursor />}
    </div>
  )
}
```

**Delta accumulation:**
- Buffer deltas in memory
- Render incrementally (typewriter effect)
- Replace with completed event when received
- Clear delta buffer after completion

**Scroll anchoring:**
- Auto-scroll when user is at bottom
- Lock scroll when user scrolls up
- Resume auto-scroll when user scrolls to bottom
- Smooth scroll during streaming

### Delegated trajectories

An event's `span` names the trajectory it ran in, and a `subagent` event's
`streamId` *is* a trajectory id (`docs/session-protocol.md` § Delegated
trajectories). `lib/timeline-tree.ts` turns that into a tree: `buildRows`
collapses deltas and pairs results, `buildTree` links each row to the trajectory
it names. Both are React-free so they unit-test in a `node` environment, which is
the same reason `lib/sse-parse.ts` is separate from the hook that uses it.

**The tree is navigated, not displayed.** Depth is unbounded — an agent delegates,
its delegates delegate, a dynamic workflow fans out to a dozen agents and then
synthesizes — so one indent level per depth runs out of viewport before it runs
out of tree. Instead:

- A trajectory renders as **one collapsed card**. Opening it shows its immediate
  children, themselves collapsed.
- Indentation stops at `MAX_INDENT = 2`. Past that a phone's content column is
  narrower than a line of code and a tool call's path truncates to nothing, so the
  indent stops conveying structure and starts destroying content.
- **Focus re-roots** the timeline at a trajectory with a breadcrumb back. This is
  the file-manager move, and it works because a user drilling into a delegation has
  stopped caring about its siblings.

**Collapsing is what makes it fast.** A closed card mounts none of its subtree, so
DOM cost tracks what the user opened rather than what the agent did: a
20,000-event workflow costs the same as a small one until someone drills in. An
indented rendering cannot have that property, because it must lay out every node
to know where any node goes. This is a stronger result than the memoization note
in `components/timeline.tsx`, and it is why virtualization is still not needed.

**Every card reports its whole subtree,** because collapsing is otherwise where a
failure goes to die. `Rollup` carries failures, nested trajectory count, tool
count, token usage, and duration, computed bottom-up; a descendant failure
escalates its ancestors' status even when their own result was clean. The rule is
that no state reachable in the tree is unreported at the root — otherwise
collapsing is lossy rather than deferred.

**`abandoned` is not `failed`.** A trajectory the server closed on turn
termination says nobody ever reported an outcome; a failed one says the work
reported an error. Colouring the first red would report an interrupted turn as a
broken one.

## Data Flow Patterns

### Creating a Session

```tsx
function CreateSessionDialog() {
  const createSession = useMutation({
    mutationFn: (data: CreateSessionRequest) =>
      api.sessions.create(data),
    onSuccess: (session) => {
      queryClient.invalidateQueries(['sessions'])
      router.push(`/sessions/${session.id}`)
    }
  })
  
  return (
    <Dialog>
      <form onSubmit={(e) => {
        e.preventDefault()
        createSession.mutate(formData)
      }}>
        <AgentSelect />
        <WorkdirInput />
        <FlagsInput />
        <Button type="submit">Create</Button>
      </form>
    </Dialog>
  )
}
```

### Sending a Message

```tsx
function MessageInput({ sessionId }: Props) {
  const sendMessage = useMutation({
    mutationFn: (data: CreateTurnRequest) =>
      api.turns.create(sessionId, data),
    onMutate: async (variables) => {
      // Optimistic update
      await queryClient.cancelQueries(['sessions', sessionId])
      const previous = queryClient.getQueryData(['sessions', sessionId])
      
      queryClient.setQueryData(['sessions', sessionId], (old) => ({
        ...old,
        turns: [...old.turns, {
          id: 'temp-' + Date.now(),
          status: 'queued',
          requestText: variables.text,
          createdAtMs: Date.now()
        }]
      }))
      
      return { previous }
    },
    onSuccess: (turn) => {
      // Start streaming
      streamManager.connect(turn.id)
    },
    onError: (err, variables, context) => {
      // Rollback
      queryClient.setQueryData(['sessions', sessionId], context.previous)
    }
  })
  
  return (
    <form onSubmit={(e) => {
      e.preventDefault()
      sendMessage.mutate({
        text: inputText,
        attachments,
        idempotencyKey: crypto.randomUUID()
      })
    }}>
      <textarea />
      <AttachmentPicker />
      <Button type="submit">Send</Button>
    </form>
  )
}
```

### Streaming Events

```tsx
function ConversationCanvas({ sessionId }: Props) {
  const { data: session } = useQuery(['sessions', sessionId])
  const currentTurn = session.turns.find(t => t.status === 'running')
  
  useTurnStream(currentTurn?.id, {
    onEvent: (event) => {
      // Update live projection
      streamStore.addEvent(currentTurn.id, event)
      
      // Finalize when completed
      if (event.phase === 'completed') {
        streamStore.finalizeEvent(currentTurn.id, event.seq)
        queryClient.invalidateQueries(['turns', currentTurn.id])
      }
    },
    onComplete: () => {
      queryClient.invalidateQueries(['sessions', sessionId])
    }
  })
  
  return (
    <div className="flex flex-col gap-4">
      {session.turns.map(turn => (
        <TurnView key={turn.id} turn={turn} />
      ))}
    </div>
  )
}
```

## Sylph-Specific Features

### Origin-Aware UI

**Session origin indicators:**

```tsx
function OriginBadge({ origin }: { origin: 'sylph' | 'native' }) {
  const config = {
    sylph: {
      color: 'green',
      icon: '🟢',
      label: 'Sylph'
    },
    native: {
      color: 'violet',
      icon: '🟣',
      label: 'Native'
    }
  }[origin]
  
  return (
    <Badge className={`bg-${config.color}-100 text-${config.color}-700`}>
      {config.icon} {config.label}
    </Badge>
  )
}
```

**Native session warnings:**

When user opens a native session, show a banner:

```tsx
{session.origin === 'native' && (
  <InfoBanner variant="warning">
    <Icon name="shield-alert" />
    <Text>
      This is a native session imported from {session.agent}.
      Identity and context are preserved. No SOUL.md injection or
      automatic resets will occur.
    </Text>
    <Tooltip>
      <InfoIcon />
      <Content>
        Native sessions maintain their original agent's identity
        and lifecycle policy. Sylph Web will not modify the prompt
        or compact context.
      </Content>
    </Tooltip>
  </InfoBanner>
)}
```

### Lifecycle Indicators

**Sylph sessions show lifecycle state:**

```tsx
function SessionHeader({ session }: Props) {
  const idleHours = (Date.now() - session.updatedAtMs) / (1000 * 60 * 60)
  const willReset = session.origin === 'sylph' && idleHours >= 24
  
  return (
    <Header>
      <Title>{session.title}</Title>
      <Meta>
        {session.agent} · {session.flags.model} · {session.flags.effort}
      </Meta>
      {willReset && (
        <Badge variant="warning">
          Will reset at 04:00 ({idleHours.toFixed(1)}h idle)
        </Badge>
      )}
    </Header>
  )
}
```

### TraceDB Search Integration

**Unified session search:**

```tsx
function SessionSearch() {
  const { data: liveResults } = useQuery(['sessions'])
  const { data: traceResults } = useQuery(
    ['search', query],
    () => api.search({ q: query }),
    { enabled: query.length >= 2 }
  )
  
  // The backend annotates archive hits with the matching Web session id. For
  // live rows without an import, nativeSessionId is the durable identity.
  const merged = useMemo(() => {
    const live = liveResults.sessions
    const trace = traceResults?.sessions || []
    
    const traceIds = new Set(live.map(s => s.traceSessionId).filter(Boolean))
    const nativeIds = new Set(live
      .filter(s => s.nativeSessionId)
      .map(s => `${s.agent}:${s.nativeSessionId}`))
    const uniqueTrace = trace.filter(s =>
      !traceIds.has(s.traceSessionId) && !nativeIds.has(s.traceSessionId))
    
    return [...live, ...uniqueTrace]
  }, [liveResults, traceResults])
  
  return (
    <SearchInput
      placeholder="Search sessions..."
      results={merged}
      renderResult={(session) => <SessionCard session={session} />}
    />
  )
}
```

### Archived history in the sidebar

The left pane is built from two sources, because one of them does not know what
the user has done. `GET /sessions` holds only sessions created in this app, so a
directory with years of coding-agent history rendered as **"No sessions yet"** —
the sidebar claimed a workspace was empty while TraceDB held hundreds of sessions
for that same path. Archived history was reachable only through full-text search,
which requires already knowing what to search for.

Each expanded group therefore also queries `GET /archive/sessions?workdir=...`,
and merges:

- **Live rows first**, then archived ones. A session you can continue outranks one
  you can only read.
- **Deduplicated** on `traceSessionId` / `sessionId`, with a backend fallback to
  the originating agent's `<agent>:<native-session-id>` identity. This covers a
  normal live session as soon as TraceDB ingests it, before any explicit import.
  An imported archived session is the same conversation as a live row, so listing
  both would offer two routes into one history.
- **Marked by shape, not colour** (WCAG 1.4.1): `●` sylph, `◆` native, `⌗`
  archived, with `⑂N` for rolled-up subagent sessions. `⑂N` counts only subagents
  that ran in *this* directory — one that ran in a worktree or subdirectory is a
  row in that directory instead, because there it is the work rather than a
  delegation.
- **Fetched only while the group is open** (`enabled: open`), because each row
  costs the backend a lineage lookup and, when the archive recorded no title, a
  label lookup. `staleTime` is 60s: archived history only changes when TraceDB
  ingests, which nothing in this tab drives.

The group count reads `8+` rather than a total. The archive count for a directory
is unknown without paging all of it, and a live-plus-preview sum would look like a
complete tally while being an arbitrary partial one. A `⚠` replaces the `+` when
the archive is unreachable, so "no history" and "cannot see history" stay
distinguishable.

### Browsing history: `Mod+Shift+K`

The sidebar shows a handful of rows per workspace; a real directory can hold over
a thousand sessions. Paging that in a 240px pane is not navigation, so
`components/history-overlay.tsx` is the browse surface: workspace-scoped, cursor
paged, with a client-side filter over the loaded pages.

Scoping is what separates it from `Mod+K`. Search answers "which session mentioned
X" across everything; this answers "what have I done in this project", which is
the question someone has when a workspace looks unfamiliar. The scope comes from
`activeWorkdir` in `lib/workspace-ui.tsx`, set by the open session or by the
sidebar group whose `Browse all` was clicked.

Filtering is deliberately local. TraceDB's full-text search is already `Mod+K`,
and re-querying it per keystroke would spawn a subprocess per character to answer
a narrower question; the footer says the filter covers loaded sessions only.

Changing the scope goes through a **workspace switcher**, not a `<select>`. On a
real archive that select is 200+ options sharing a `~/Workspace/` prefix, ordered
by recency and reachable only by first letter — and it could not name a directory
the archive has never seen at all. The switcher ranks known workspaces by recency,
filters on the full path rather than the leaf name (the segment someone remembers
is often in the middle: `huawei`, `sylph`), shows each one's archived and live
counts so an empty directory is visible before it is opened, and falls back to
`DirectoryPicker` for anything unlisted.

It is a **mode swap inside the same modal**, not a second modal. Nesting `Overlay`
in `Overlay` puts two focus traps on one DOM path: the outer trap's
`querySelectorAll` also matches the inner dialog's controls, so Tab at the
boundary depends on document order rather than on which dialog is on top. Swapping
the content keeps exactly one trap alive, makes "back" a state change rather than
an unmount, and leaves `Escape` meaning the same thing at every depth.

This overlay carries the one piece of lineage work the client owns: collapsing a
**fork** whose pair straddles a page boundary. A fork and its origin replay the
same prefix and read as one conversation, so only one belongs in the list. The
server drops the loser within a single response, but it keeps no state between
requests, and one measured pair sits 268 rows apart — so only a surface that
accumulates pages can see both. Rows arrive newest-first, so the survivor is
whichever appeared earlier, and the check runs in both directions: a row whose
`forkOf` names an earlier row is a duplicate, and so is a row that an earlier
row's `forkOf` names. Two index maps keep it linear as pages accumulate.

The sidebar deliberately does *not* do this: it fetches one page, where the server
has already resolved any pair.

### Reading an archived session

`app/archive/[...id]/page.tsx` renders an archived transcript read-only, through
the same `Timeline` the live conversation uses — the backend normalizes archived
events into the live `StreamEvent` shape, so there is no second renderer to drift.

A catch-all route because a subagent id contains a slash
(`claude:uuid/agent-name`). Next hands those segments back **still
percent-encoded**, so each is decoded before rejoining; without that the client
re-encodes and the backend looks up a literal `claude%3A...`.

Browsing must not mutate, which is why this is separate from import. Continuation
is offered as an explicit action, disabled with its reason shown when the archive
recorded no `cwd` — `POST /sessions/import` rejects those, and a button that always
fails is worse than one that explains itself. There is no composer.

## Performance Optimization

### Bundle size management

**Measured: 203.9 KB gzipped first-load JS.** The project maintains a compact bundle
through deliberate architecture and dependency choices. Phase 4 measured 187.8 KB;
Phase 6 added the shell, three overlays, the file browser, the Markdown renderer,
the diff view, and the fuzzy ranker for **+8.4 KB**, reaching 196.2 KB. Archive
browsing added **+1.2 KB** across 6.6 and 6.7, and the document viewers **+0.9 KB**:
the history overlay, the read-only archived view, and every format viewer are all
`next/dynamic`, so only their client-side entry points land here.

The design-system pass measured **198.2 KB → 203.9 KB**, `+5.7 KB`, with the same
recipe run on both trees — the baseline was re-measured rather than quoted, and
reproduced at 198.2 KB exactly. What it buys:

| Change | Cost |
| --- | --- |
| 48 hand-authored icons replacing Unicode dingbats | ~1.0 KB gz for the path data alone; the components tree-shake per route |
| `ui/button`, `ui/menu` — the shared primitives | The largest single item. `menu` carries popover placement, arrow navigation, and the sheet fork, and is reached from the sidebar, which *is* first-load |
| `lib/theme`, `lib/viewport`, `lib/use-dialog` | Small; two are `useSyncExternalStore` wrappers |
| Service-worker registration | ~0.2 KB, production-only |
| `Sheet`/`Drawer` split out of the shell | −0.2 KB. Small, but they are unreachable above `md`, so shipping them to every desktop route is wrong regardless of size |

That is a 2.9% increase for the token layer, dark mode, the icon set, the mobile
layout, and PWA installability. Judged against the stated policy — size should
scale proportionally to value delivered, and no arbitrary ceiling should constrain
valuable work — it is a straightforward trade. It is recorded rather than
optimised away because the alternative reading is available too: the primitives
would be cheaper if the sidebar were not in the first-load bundle, and moving it
behind a dynamic boundary is the change to make if this number needs to come down.

Replacing the all-sessions list with the start screen measured **198.4 KB → 198.2
KB** — the two tracks of this work were measured against each other after merging,
with the same recipe on both trees. The screen adds markup, the directory grouping,
and a small pure module; it gives back the four filter controls, the query-building
they fed, the debounce on that path, and a dead `sessionSubtitle` export, which is
slightly the larger side.

**The goal is to keep the bundle lean and fast-loading**, not to enforce a hard
numeric ceiling. As features grow, bundle size should scale proportionally to value
delivered. Monitor growth deliberately, optimize where reasonable, but never let an
arbitrary limit constrain valuable functionality or architectural improvements.

This principle explains the "no dependency" decisions throughout this document. The
alternative implementations — `react-markdown` + `remark-gfm` (~45 KB), Shiki or
Prism (~35 KB), jsdiff (~30 KB), Radix (~30 KB for the primitives used), fuse.js
(~12 KB), an icon package — would have totaled over 150 KB. Each replacement is a
bounded, well-understood algorithm carrying unit tests; that is the trade-off,
stated plainly. This approach remains correct while the implemented subset serves
the product's needs and the bundle stays compact. As requirements evolve, adopting
battle-tested libraries may become the better choice.

**Office formats are parsed server-side**, which is the largest single instance of
this. SheetJS (`xlsx`) is 7.5 MB unpacked, frozen at 0.18.5 on npm, and carries two
unpatched High CVEs — prototype pollution (CVE-2023-30533) and ReDoS
(CVE-2024-22363) — reachable on precisely the "parse a file we did not write" path
this feature needs. `calamine` and `quick-xml` do the same work in Rust, where the
cost is compile time rather than every page load, and the browser receives
structure instead of bytes. See `docs/api-design.md`, Files.

To measure:

```sh
cd frontend && npm run build
# then sum the gzipped size of the scripts referenced by a prerendered route
```

### Code splitting

`AppShell` sits in the root layout, so anything it imports statically lands in the
first-load bundle of *every* route. The three overlays and the Artifacts pane are
therefore `next/dynamic` with `ssr: false`: each is reached by a keypress or a
toggle, by which point the chunk has fetched. Measured effect: **204.3 KB → 195.8
KB**, which is the difference between over and under budget.

The format viewers split again *beneath* the pane, so opening a `.ts` file never
fetches the table or document code. With ~3 KB of headroom that is not an
optimization but the condition for the feature existing: the grid viewer was
confirmed absent from the first-load chunk set and fetched on demand at 1.5 KB.

### Render cost

**Measured: a 1000-event conversation cold-renders in well under the 2s budget**
(`e2e/timeline.spec.ts`, on a reload so it covers durable history rather than
incremental streaming).

The mechanism is memoization by object identity, not windowing. `buildRows`
produces a new row object only for rows whose content changed, and the row
component compares by reference — so a streaming token costs O(new rows) rather
than O(conversation length). A unit test pins that settled rows stay referentially
stable as events arrive, because that property is what the whole approach rests on
and it is invisible if it breaks.

**No virtualization.** A windowing library would consume most of the remaining
budget, and profiling showed re-render count, not DOM size, to be the cost. The
evidence that would reverse this: scroll jank on a large session while *idle*,
which is the signature of a DOM-size problem rather than a render-count one.

Bounded rendering keeps DOM size in check without windowing: code blocks show 60
lines with "show all", command output 40, diffs elide unchanged runs to three lines
of context, and diff alignment above 2000 lines per side degrades to a wholesale
replacement rather than allocating a quadratic table.

### Document previews

**A grid is not virtualized either, but for different reasons from the timeline,
and they were measured directly.** A sticky-header table in headless Chromium:

| Rows × cols | Cells | Build | Scroll |
| --- | --- | --- | --- |
| 200 × 30 (default window) | 6 231 | 46 ms | 0.06 ms/frame |
| 500 × 30 (max per request) | 15 531 | 82 ms | 0.06 ms/frame |
| 2000 × 30 | 62 031 | 302 ms | 0.22 ms/frame |
| 2000 × 50 | 102 051 | 517 ms | 0.18 ms/frame |

Neither axis behaves like the timeline's. A grid does not stream, so the
re-render count that memoization solves there is not a factor. And **scroll cost
is flat** across a 16× range of cell counts, because scrolling a sticky-header
table is compositor work that does not scale with row count — so the DOM-size
problem virtualization would fix does not appear at these sizes.

What does scale is the initial build. Two things keep it bounded:

- **The server windows the data** (200 rows by default, 500 max), so a 50 000-row
  sheet never arrives whole — it would be a ~50 MB payload and a ~600 ms parse.
- **Paging replaces the window rather than accumulating rows.** Accumulation was
  the first design and is wrong twice over: it needs the previous window as state,
  which forces either an effect that cascades a render or a ref read during
  render, and it grows the DOM without bound while `table-layout: auto` re-measures
  every cell in the table on each append. Replacing keeps the table at one
  window — 46 ms — no matter how far the user pages.

`table-layout: fixed` on wide sheets is load-bearing for the same reason: the
default `auto` algorithm resolves column widths by measuring every cell, making
layout cost global rather than proportional to what changed.

The evidence that would reverse this: scroll jank within a single window while
*idle*, the same signal named for the timeline above.

### File browsing

The Artifacts tree fetches **one directory per expansion**. Walking the tree
server-side would spend seconds on a 100k-file repository building a payload whose
first screen is all anyone reads. Each level owns its own query, so an expanded
subtree is cached independently and collapsing does not discard it. Listings cap at
2000 entries per directory and report `truncated`.

Previews cap at 1 MiB, truncating text to a 256 KiB prefix. Images are
all-or-nothing, because a truncated image decodes to nothing. Documents have their
own 25 MiB cap — a legitimate spreadsheet is routinely several MiB, so the 1 MiB
text bound would refuse the files people most want to look at.

Document queries are keyed on `(path, sheet, row)`, so switching back to a sheet
already viewed is a cache hit rather than another parse.

## Accessibility

Target: WCAG 2.1 AA. What that means concretely here, and how each part is
checked.

### Automated: axe-core, in CI

`e2e/accessibility.spec.ts` runs axe-core against every surface with real content —
the start screen in both its states (recents, and the first-run guide, which is a
different subtree reached by intercepting the listing), the create picker, a
conversation canvas holding tool calls, a diff, and a failed command with both
sidebars open, the composer mid-turn, and each of the three overlays while open.
`e2e/file-viewer.spec.ts` adds the file viewer in the pane and the fullscreen
dialog, since a data table and an iframe are both new surface. Tags: `wcag2a`,
`wcag2aa`, `wcag21a`, `wcag21aa`. **Currently zero violations.**

This is a floor, not a conformance claim. axe covers roughly a third of WCAG
criteria; whether focus order makes sense and whether an alternative is
*meaningful* are not machine-checkable. Full validation needs assistive-technology
testing and expert review, which has not been done.

The audit has already earned its place twice, catching a broken
`aria-activedescendant` that pointed at no element, and 2.48:1 text that had been
reviewed by eye and passed.

### Colour

`text-gray-400` (#99a1af) measures **2.48:1** on white and fails at every size, so
it is used only on `aria-hidden` decorative glyphs. Visible muted text is
`text-gray-500` minimum, and `text-gray-600` on a `gray-100` highlighted row where
gray-500 lands at 4.39:1 — just under the threshold. On the dark active sidebar
row the scale inverts: muted text there is `text-gray-300`, because gray-500 on
gray-900 measures 3.66:1.

Nothing is conveyed by colour alone (WCAG 1.4.1):

- Session origin: shape (`●` vs `◆`) plus a `title`, not just green vs violet.
- Diff lines: a `+`/`−` sign as real text, plus an `sr-only` "added:"/"removed:"
  prefix — the glyph alone reads as punctuation to a screen reader.
- Tool failure: the word "failed" and the exit code, not a red border alone.

### Keyboard

Checked in a real browser, because none of it is verifiable statically:

- The skip link is the first focusable element, becomes visible on focus, and
  actually moves focus to `<main>`.
- Every overlay traps Tab, closes on Escape, and **returns focus to its opener** —
  without which a keyboard user is dropped at the top of the document.
- Bare-key shortcuts (`Shift+E`) do not fire while a text field has focus.
- Sidebars toggle, the palette runs commands, and search opens a session, all
  without a pointer.

### Semantics

- Landmarks: `<main id="main">`, `<nav aria-label="Sessions by workspace">`,
  `<aside aria-label="Workspaces">`, `<aside aria-label="Artifacts">`.
- Each tool call is a `<section>` with an `aria-label` naming its tool, so a screen
  reader can move between operations instead of reading one undifferentiated
  stream.
- Disclosures are native `<details>`/`<summary>`, which carry `aria-expanded` and
  keyboard operation with no script.
- Pickers follow the combobox pattern: focus stays on the input, rows carry
  `aria-selected`, and the input carries `aria-activedescendant`. This is the only
  arrangement where typing and arrow-navigating do not fight over focus.
- Toggle buttons carry `aria-pressed`; the open session's sidebar link carries
  `aria-current="page"`.
- Streaming status is announced through `aria-live="polite"`.
- Markdown headings inside agent prose start at `<h4>`: the page owns h1–h3, and
  emitting an h1 mid-transcript would break the document outline that screen-reader
  navigation depends on.

### Not done

- Screen-reader passes with VoiceOver and NVDA.
- High-contrast and forced-colours modes.
- Dark theme — deliberately absent rather than half-built. It needs a `dark:` pass
  over every component plus a resolved `prefers-color-scheme` fallback, and a
  settings toggle storing a preference nothing reads would misrepresent that.

## Error Handling

### Error Boundaries

```tsx
class ErrorBoundary extends Component {
  state = { hasError: false, error: null }
  
  static getDerivedStateFromError(error) {
    return { hasError: true, error }
  }
  
  componentDidCatch(error, errorInfo) {
    console.error('Error boundary caught:', error, errorInfo)
  }
  
  render() {
    if (this.state.hasError) {
      return (
        <ErrorFallback
          error={this.state.error}
          reset={() => this.setState({ hasError: false })}
        />
      )
    }
    return this.props.children
  }
}
```

**Error boundaries wrap:**
- Each route (page-level recovery)
- Conversation canvas (isolated failure)
- Each turn (prevent cascade)

### Network Errors

**API call failures:**
```tsx
function useSessionMutation() {
  return useMutation({
    mutationFn: api.sessions.create,
    onError: (error) => {
      if (error.code === 'VALIDATION_ERROR') {
        toast.error(error.message)
      } else if (error.code === 'NOT_FOUND') {
        toast.error('Session not found')
        router.push('/')
      } else {
        toast.error('Failed to create session. Please try again.')
      }
    },
    retry: (failureCount, error) => {
      // Retry only on network errors, not validation
      return failureCount < 3 && !error.code
    }
  })
}
```

**SSE reconnection failures:**
- Show "Reconnecting..." badge after 3s
- Show "Connection lost" after 30s
- Offer manual refresh button
- Log errors to console for debugging

### User-Facing Error States

**Empty states.** The one empty state that matters — an app holding no sessions —
is not a placeholder card. It is the start screen's first-run guide, described under
"`/` — the start screen, not a session list", and the decision worth carrying is
that it is only rendered once a listing has *settled* empty. `selectStartState`
enforces that, because a pending or failed listing is also an empty array and
treating it as "no sessions yet" flashes onboarding at users who have hundreds.

**Loading states:**
```tsx
function ConversationCanvas({ sessionId }: Props) {
  const { data, isLoading, error } = useQuery(['sessions', sessionId])
  
  if (isLoading) {
    return <Skeleton />
  }
  
  if (error) {
    return (
      <ErrorState>
        <Icon name="alert-circle" />
        <Title>Failed to load session</Title>
        <Button onClick={() => queryClient.invalidateQueries()}>
          Try Again
        </Button>
      </ErrorState>
    )
  }
  // render conversation
}
```

## Testing Strategy

Three layers, split by what each can actually establish.

### Unit — Vitest (`npm test`)

```sh
cd frontend && npm test          # once
cd frontend && npm run test:watch
```

**140 tests over the pure logic that replaced a dependency, plus the state
decisions that are one line of JSX away from being wrong.** That is the selection
rule: hand-rolling a diff, a Markdown parser, and a fuzzy ranker means taking on
the test burden those libraries carried, and this is where that debt is paid.

| File | Covers |
| --- | --- |
| `lib/diff.test.ts` | LCS alignment, line numbering against the original files, `-` before `+`, no phantom trailing line, prefix/suffix stripping speed, degradation above the size cap |
| `lib/markdown-parse.test.ts` | Block and inline parsing, code not re-scanned, `javascript:`/`data:` links refused, termination on pathological input |
| `lib/tool-shape.test.ts` | Each shipped runner's payload shape, name classification, hunk extraction, never throwing on malformed `data` |
| `lib/fuzzy.test.ts` | Subsequence matching and *relative ranking* — boundary and adjacency bonuses, field weighting |
| `lib/shortcuts.test.ts` | Canonical combo spelling, Cmd/Ctrl unification, recorder and dispatcher agreeing |
| `lib/timeline-rows.test.ts` | Delta accumulation, tool-result pairing, interleaved streams, referential stability of settled rows |
| `lib/welcome.test.ts` | The start screen's three states — that a pending or failed listing is never mistaken for an empty app — and the directory grouping behind recents: one row per path however many sessions it holds, ranked by newest rather than by count, trailing slashes normalized, the cap applying to directories, and the shared query cache never reordered |

Tests live beside the code they cover, not in a parallel `tests/` tree, so a
reader of `lib/diff.ts` can see its tests without going looking.

`environment: "node"` — these are pure functions, so no DOM is needed. Anything
requiring one is tested in Playwright instead, where the DOM is real. `e2e/` is
excluded from Vitest because those files import `@playwright/test`, whose `test`
export would collide with Vitest's runner.

**No React Testing Library.** Component behaviour here is focus management,
keyboard dispatch, and streaming — all of which jsdom approximates badly enough
that a passing test would not mean much. Those live in Playwright.

### E2E — Playwright (`npm run e2e`)

**58 specs across two projects** — 51 at desktop width, 7 on a phone. Both
servers start automatically on offset ports with a throwaway data directory and a
mock runner, so a run never touches the real journal, archive, or agents.

| File | Covers |
| --- | --- |
| `session-lifecycle.spec.ts` | Create, send, stream, reload-replay, sidebar grouping, resuming from `/` |
| `start-screen.spec.ts` | Usable before the listing settles, first-run guide vs recents, create button matching its shortcut, key hints reflecting a rebind |
| `timeline.spec.ts` | Tool calls as regions, inline diff, failure auto-expansion, Markdown, 1000-event render budget |
| `workspace-shell.spec.ts` | Pane toggles and persistence, palette, search, focus restoration, settings round-trip, Artifacts tree, history browser, dialog-free creation |
| `file-viewer.spec.ts` | Tree/viewer swap preserving expansion, fullscreen and focus return, dialog sizing to content vs filling for a page, single-modal invariant, CSV table, sandboxed HTML refusing script and cookie access, image zoom, axe over both scales |
| `streaming.spec.ts` | Mid-turn painting against a deliberately paced runner |
| `accessibility.spec.ts` | Skip link, focus trapping and restoration, axe-core audit of every surface including both start-screen states |
| `mobile.spec.ts` | Phone-only: composer fits the viewport, sidebars out of flow, drawer and sheet behaviour, document cannot scroll, PWA installability, dark mode before paint |

#### Why a second project exists

The three reported mobile failures — a composer wider than the viewport,
sidebars leaving ~150px for the conversation, and scrolling dragging the whole
shell into blank space — were all invisible to a 41-spec suite that was not
careless. Every spec in it ran at 1280px, where the same code is at worst
slightly cramped. A layout bug that manifests only below a breakpoint can only
be observed below that breakpoint, so a viewport is not an optional variant
here; it is the only instrument that sees the defect.

`devices["Pixel 7"]` rather than an iPhone profile. Only Chromium is installed
in this checkout, and an iPhone profile silently runs *in Chromium* anyway —
which would claim iOS coverage that does not exist. Pixel 7 is a Chromium
device, so what it emulates is what actually runs: `pointer: coarse`, a 412px
viewport, and a device pixel ratio above 1.

**What it therefore does not cover** is iOS Safari specifically: `100dvh` under
a collapsing toolbar, and zoom-on-small-input. Those are two of the mechanisms
behind the scroll bug, so the specs assert the fix through consequences a
Chromium phone *can* observe — nothing overflows the viewport, the document has
nothing to scroll, `window.scrollY` stays 0 after a 4000px wheel — rather than
by simulating a toolbar this browser does not have. A real device still wants a
pass.

**These specs assert invariants, not pixels.** A spec that knows the composer is
358px wide fails on the next padding change and says nothing about whether it is
usable. `overflowingElements()` instead reports every element whose box extends
past the right edge, which is both the actual defect and actionable when it
regresses.

**Wipe `.e2e-data/` before trusting a timing result.** The journal is never
cleared, and each spec's `mkdtemp` workdir becomes a permanent sidebar group —
grouping is by workdir, so a directory used once and deleted still renders a
group forever. At 151 groups (148 of them dead single-session directories) the
suite ran 35 minutes instead of 1.5, and two or three specs failed per run on
the 30s timeout: a different two or three each time, which reads as flakiness
rather than as accumulated fixture state. `rm -rf frontend/.e2e-data` is free —
it is gitignored and the backend recreates it. Raising the timeout does not
help, because the cost grows without bound.

**`scripts/e2e-repeat.sh` separates a flake from a defect**, since a single red run
cannot. It runs the suite N times and reports each failing spec's frequency: every
run is `DEFECT`, some runs is `flake (3/10)`.

It reports `INCONCLUSIVE` when a run executed *no* tests — a backend that fails to
compile, a `webServer` that exits before any spec is reached. Such a run yields
`expected=0, failed=0`, which the summary originally read as "no failures in any
run": a false green, and the worst possible output from the one script whose job is
to tell you whether to trust a result. A run that never started is not evidence about
the code, and the script now says so instead of averaging it in.

**Two states are reached by intercepting the listing, not by emptying the
journal.** Specs share one backend and run serially, so by the time a spec runs
other sessions exist. `page.route` is how the empty-app and slow-listing branches
become deterministic, and it is the *client's* branch under test either way.

The mock runner (`e2e/fixtures/runners/e2e/runner.sh`) branches on its prompt to
emit three scenarios: a plain streamed reply, a tool sequence with a real diff and
a failing command, and a 1000-event bulk transcript. Its payloads mirror what the
shipped runners emit, transcribed from their source — a fixture that emitted a
tidier shape would test a protocol nothing speaks.

**A note on shortcut tests.** Pressing a shortcut immediately after `page.goto`
races hydration: the listener is attached by an effect, so the key can land on
static markup. Tests wait for a client-rendered control first. This is a real race
on a cold load, not a test artifact.

## Development Workflow

### Setup

```sh
# Create frontend directory
cd /Users/hsw/Workspace/sylph-web
mkdir -p frontend
cd frontend

# Initialize Next.js
npx create-next-app@latest . --typescript --tailwind --app --no-src-dir

# Install dependencies
npm install @tanstack/react-query zustand
npm install @radix-ui/react-dialog @radix-ui/react-dropdown-menu
npm install framer-motion lucide-react
npm install shiki react-markdown
npm install -D @types/node vitest playwright
```

### Development Server

```sh
# Terminal 1: Backend
cd backend
cargo run

# Terminal 2: Frontend
cd frontend
npm run dev
```

Frontend: `http://localhost:3000`  
Backend: `http://localhost:4317`

### Implementation Checklist

Before implementing any UI component or feature:

1. **[ ] Read DSH source code first**
   - Locate equivalent component in `/Users/hsw/Workspace/oss/deepseek-harness/repo/apps/web/`
   - Study its structure, props, state management
   - Note styling patterns and interaction handlers

2. **[ ] Implement following DSH patterns**
   - Copy component hierarchy if applicable
   - Use similar state management approach
   - Match animation/transition timing
   - Mirror accessibility attributes

3. **[ ] After implementation, compare with DSH**
   - Open DSH source side-by-side
   - Verify you haven't missed key patterns
   - Check if DSH handles edge cases you missed
   - Confirm styling consistency

4. **[ ] Only deviate with justification**
   - Document why your approach is better
   - Verify Sylph-specific requirement (e.g., origin badges)
   - Ensure no regression in UX quality

**Example: Implementing ThinkingBlock**

```sh
# 1. Read DSH implementation
code /Users/hsw/Workspace/oss/deepseek-harness/repo/apps/web/
# Find thinking block component

# 2. Note patterns:
# - Collapse/expand with smooth animation
# - Icon rotation on toggle
# - Fade-in for content
# - Keyboard accessibility (Space/Enter to toggle)

# 3. Implement with same patterns in Sylph Web

# 4. Compare after completion
# - DSH uses framer-motion for animation
# - Keyboard handler matches
# - ARIA attributes present
```

### API Proxy

Next.js proxy routes to avoid CORS:

```ts
// app/api/[...proxy]/route.ts
export async function GET(req: Request) {
  const url = new URL(req.url)
  const path = url.pathname.replace(/^\/api/, '')
  
  const backendUrl = `http://127.0.0.1:4317${path}${url.search}`
  const response = await fetch(backendUrl, {
    headers: {
      'Content-Type': 'application/json',
      'Cookie': req.headers.get('cookie') || ''
    }
  })
  
  return response
}

export { GET as POST, GET as DELETE }
```

### Type Generation

Generate TypeScript types from OpenAPI spec:

```sh
npx openapi-typescript ../docs/api-v1.openapi.json \
  -o src/types/api.ts
```

Re-run after backend API changes.

### Build & Deploy

```sh
# Production build
npm run build

# Serve static export (optional)
npm run start

# Or let Rust backend serve dist/
```

<!--
  A per-week deliverables checklist used to live here. It has been removed rather
  than updated: `docs/roadmap.md` owns phase status, and a second copy in a design
  document only drifts from it. This file describes how the frontend is built and
  why; what is built and what is next belongs in the roadmap.
-->

## Future Enhancements

This list was written when most of it was unbuilt. What remains genuinely absent:

**Session management** — delete, move, and export have no endpoint behind them. Rename
has one (`PATCH /api/v1/sessions/{id}` accepts `title`) but no menu item calls it.
Forking at a turn would need the runner protocol to support resuming from a prefix.

**Search** — `Cmd+F` filters turns rather than highlighting matches inside them.
Highlighting means reaching into the Markdown, diff, and code renderers to rewrite
their text nodes, which is why it is not a small change.

**Lifecycle** — manual reset, native→Sylph conversion, and an incarnation history
viewer are all unbuilt. Conversion is still an open decision about endpoint shape, not
just missing UI; see `docs/roadmap.md`. Lifecycle timing (24h idle, 04:00) is modelled
in the backend and surfaced nowhere.

**Performance** — virtualization stays deferred, now with measurement behind it rather
than as an aspiration: a 1000-event conversation renders inside budget through
memoization. The evidence that would reverse it is scroll jank while *idle* on a large
session, which indicates DOM size rather than render count. A service worker has no
use case here — the app is already local-first and its data is a localhost backend.

**Collaboration** — sharing, comments, and session-to-session diff remain out of
scope, and each would need a story for a surface that is currently loopback-only.

Already shipped, and listed here only because the original list predates them:
attachments with drag-and-drop and paste, image and PDF viewers, code rendering,
per-route code splitting, `Cmd+K` full-text search across live sessions and the
archive, and inline diffs within a turn.

**Dark theme** is deliberately absent rather than half-built: it needs a `dark:` pass
over every component plus a resolved `prefers-color-scheme` fallback, and a settings
toggle storing a preference nothing reads would misrepresent that.

## Decision Log

### Why Next.js over Vite + React Router?

- SSR capability for future server components
- Built-in API routes for backend proxy
- Image optimization
- Better TypeScript integration
- Production-ready out of box

### Why TanStack Query over SWR?

- More powerful mutation handling
- Optimistic updates are first-class
- Better DevTools
- Wider ecosystem adoption

### Why no component library (neither Radix nor Headless UI)?

Radix would have been the right choice on accessibility defaults and edge-case
coverage. It was not taken because of one measured constraint: Phase 4 left ~12 KB
of first-load budget, and the primitives needed here — modal, context menu,
disclosure, listbox — cost roughly 30 KB of it.

What was built instead: `lib/use-dialog.ts` (focus trap, Escape, focus
restoration), `components/overlay.tsx` (the modal frame), `components/ui/menu.tsx`
(arrow navigation, viewport clamping, outside-dismiss, and the mobile sheet fork),
`components/ui/sheet.tsx` (drag-to-dismiss), native `<details>`, and the combobox
pattern in the two pickers. The axe audit and the focus tests exist to hold that decision
honest, and they have already caught one ARIA bug a library would not have had.

Reverse this if the budget rises or a primitive with real complexity is needed —
a date picker, a drag-and-drop tree, a virtualized listbox. Do not reverse it
piecemeal: a codebase with both patterns is worse than either.

### Why context and an external store, not Zustand?

Two contexts and one `useSyncExternalStore` cover the whole client state: shell
state that must outlive navigation, and preferences over `localStorage`. Zustand's
advantages — less boilerplate, selector-scoped re-renders — pay off at a scale this
store has not reached; it holds about a dozen fields.

`useSyncExternalStore` specifically, rather than state plus an effect: it avoids a
cascading render on mount, cannot mismatch during hydration because the server
snapshot is separate, and gets cross-tab sync from the `storage` event for free.

### Why hand-rolled Markdown, diff, and fuzzy matching?

See **Technology Stack** and **Performance Optimization**. Short version: the three
libraries total ~87 KB gzipped against ~12 KB of headroom, and what each provides
beyond the subset used — character-level diffs, patch formats, typo tolerance,
full CommonMark — is not rendered by this UI. The cost is carrying 81 unit tests
and two bugs already found in review; both are recorded in the test file that now
guards them.

## References

- Backend API: `docs/api-design.md`
- Event protocol: `docs/architecture.md` (Event Streaming section)
- Database schema: `docs/database-schema.md`
- Product roadmap: `docs/roadmap.md`
