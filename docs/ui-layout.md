# UI Layout and Design

What the interface *is*: the panes, how they collapse, the surfaces a user touches,
and the tokens everything is drawn with.

`docs/frontend-architecture.md` is the companion and answers a different question —
how the frontend is *built*: state, streaming, data flow, performance, error
handling. The test for where something belongs is which of those two questions it
answers. A memoization strategy is construction even though it affects the timeline;
a pane's collapse breakpoint is layout even though it is implemented in a hook.


## Three-pane layout

```
┌──────────────────────────────────────────────────────────────────────┐
│ ▣ Sylph  [⌕ Search sessions                    ⌘K] ⚡Quick Start +New ⚙ ▣│
├──────────────┬────────────────────────────────────┬──────────────────┤
│ Workspaces   │ Conversation                       │ Artifacts        │
│ (240px)      │ (flex, only this scrolls)          │ (320–384px)      │
│              │                                    │                  │
│ ⚡Quick Sess. │ › Add logging to the auth module   │ Files            │
│   scratch    │                                    │ ▾ crates/server/       │
│              │ ▸ ✻ Thinking  84 words             │   ▾ src/         │
│ Workspaces   │                                    │     ▫ lib.rs     │
│ ▾ sylph-web8+│ ▤ Read  crates/server/src/auth.rs    ✓   │     ▫ auth.rs    │
│   ● session A│                                    │ ▫ Cargo.toml     │
│   ◆ session B│ I'll add logging at three points.  │                  │
│   ⌗ past work│                                    │ ─────────────────│
│   ⌗ older  ⑂2│ ✎ Edit  crates/server/src/auth.rs    ✓   │ auth.rs          │
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

## Below `md`: the panes become surfaces

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
│ ▾ crates/server/              │
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

## The viewport

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

## Touch targets

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

## Artifacts pane: tree and viewer swap

Selecting a file replaces the tree with a full-height viewer rather than splitting
the pane:

```
browsing                            viewing
┌─ Artifacts ─────────┐             ┌─ Artifacts ─────────┐
│ Files      Reload   │             │ ‹ Files  auth.rs ⛶ ✕│
│ /Users/you/proj     │             ├─────────────────────┤
│ ▾ crates/server/          │  select     │                     │
│   ▾ src/            │  ────────▶  │   full-height       │
│     ▫ lib.rs        │             │   viewer            │
│     ▫ auth.rs       │             │                     │
│ ▦ sales.xlsx        │  ◀────────  │                     │
└─────────────────────┘  ‹ Files    └─────────────────────┘
```

Locating a file and reading it are sequential, not simultaneous, and every mature
tool treats them that way: VS Code's explorer hands off to a full-width editor,
Finder's column view to a full-screen Quick Look, GitHub's tree collapses when a
blob opens. A preview confined to a strip of a 320px pane — a few hundred pixels square —
is too small to read a spreadsheet, a document, or a rendered page in.

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

## Artifacts pane: managing files, not only reading them

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

## Reading a file the pane cannot preview

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

## File viewers

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

## Rendering untrusted HTML

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

## Conversation canvas: agent-first, not chat-first

One distinction drives every decision here, and it is what separates this from a
chat client: in an agent workspace the **tool calls are the content**, and prose is
what joins them. Claude Code, Cursor, and Zed all converged on that shape; chat UIs
optimize for the opposite, spending their vertical space on who is speaking.

So a bubble is not a styling choice this canvas declined, it is the wrong unit. The
decisions below all follow from reading the timeline as a work log:

| Decision | Reason |
| --- | --- |
| Flat, left-aligned timeline; no bubbles, no alternating alignment | Bubbles spend vertical space separating speakers, which is not the axis a reader cares about. Alternating alignment breaks scanning. |
| Tool calls get a 2px left border; prose gets no chrome | The border is a visual anchor for the thing being scanned for. Prose does not need to be found. |
| Thinking collapsed by default, expanded while streaming | Reasoning is debugging detail once it is finished, and the interesting thing while it arrives. |
| Reads collapsed, edits always open | A read is context-gathering; an edit is the output. |
| Shell output collapsed on success, expanded on failure | On failure the output *is* the answer to "what happened". |
| Turn boundary is a rule with a status line, not a box | A box per turn fragments one continuous work log. |
| Every file path is a button into the Artifacts pane | The path is the natural link between what the agent did and what it did it to. |

A tool call and its `tool_result` render as **one row**, paired by the `streamId`
they share (`docs/session-protocol.md`). A result whose adapter did not reuse the id
still renders on its own, so no output is ever dropped.

Tool payloads are interpreted by `lib/tool-shape.ts`. The protocol deliberately
leaves `data` unspecified, so that module normalizes what the three adapters emit —
`data.input` holding the arguments, with `text` as a stringified fallback — and
degrades to "unknown tool with some arguments" rather than throwing, because the next
agent will spell something differently.

## Keyboard model

One document-level listener in `lib/workspace-ui.tsx` dispatches every shortcut.
Per-component listeners would fight over the same combos and make precedence
depend on mount order.

| Combo | Action |
| --- | --- |
| `Mod+K` | Global search over every session the server holds |
| `Mod+Shift+K` | Browse this workspace's full history |
| `Mod+P` | Command palette |
| `Mod+Shift+O` | New session (pick a directory) |
| `Mod+J` | Quick Start in the configured scratch directory |
| `Mod+B` / `Mod+\` | Toggle left / right pane |
| `Mod+,` | Settings |
| `Mod+.` | Stop the running turn |
| `Mod+F` | Filter turns in this session |
| `Shift+C` | Collapse expanded disclosures |
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
- `pruneReserved` **rewrites the stored map** at hydration rather than filtering the view of
  it. A settings write persists the whole merged map, so a reserved combo already in
  `localStorage` would keep firing.
- `Mod+Q` and `Mod+W` are listed but must never be *offered*: they quit and close, so
  a page that captured them would trap the user.

`Mod+Shift+Enter` was considered for Quick Start and rejected: the composer uses
`Cmd+Enter` for newlines and `Mod` combos fire globally even inside a textarea, so
it would have broken typing.

**A spec cannot observe a browser keeping a combo**, since Playwright's Chromium opens no
real window. The spec asserts on the recorder's refusal instead, which is observable in the
DOM.

Bare-key combos (`Shift+C`) are suppressed while a text field has focus, so the
composer keeps its keystrokes; `Mod`-prefixed combos are global on purpose.

Session-scoped actions — stop turn, expand all, find — cannot be handled by the
shell, which has no access to the open session's state. The session page registers
a handler with `registerSessionActions` and the shell forwards to it; with no
session open, those actions are simply ignored.

## Creating a session: one mutation, and a directory when it is unknown

There is no create form. Every surface runs the same mutation; the only question
is whether the invocation already names a working directory.

| Surface | Path | Why |
| --- | --- | --- |
| ⚡ Quick Start, `Mod+J`, palette | `useQuickCreateSession` | Quick Start carries the selected environment and the explicit `__sylph_quick_start__` selector. The envagent resolves that selector to its own local scratch directory, so a server path is never shown as if it existed on a remote machine. |
| Sidebar group → "New session here" | `useQuickCreateSession` | A group **is** a directory. The right-click already named it. |
| `Mod+Shift+O`, empty-state button | `overlay: "newSession"` → same mutation | No directory yet, so a picker asks for that and nothing else. |

A session needs a directory, and nothing else is worth asking at creation time. Each
field a create form would offer has somewhere better to live:

- **`workdir`** — the one field a session cannot start without, and the one a text
  input served worst. An absolute path typed from memory fails as a backend
  rejection; the picker enumerates server-side, so the path is chosen and cannot
  be mistyped. It opens on `WorkspacePicker` rather than on the filesystem,
  because a new session almost always belongs in a directory this machine has
  already worked in — see **Choosing a place** below.
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
routed to the form, which is why right-clicking the fixed Quick Start group still
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

`resolveSessionDefaults` checks both stored preferences against what the chosen
environment reports. A `defaultAgent` whose CLI is not on that machine would be
rejected as an unknown agent, and a `defaultModels` entry the agent no longer
declares would be passed as a flag and fail at turn time — far from the setting that
caused it. Both degrade to a silent, correct fallback instead, and only "no agents at
all" surfaces as an error, because that is the one case with no sensible default.

## Choosing a place

Three flows ask "which directory": `Mod+N`, the sidebar's **New Workspace…**, and
the history browser's scope control. All three open `WorkspacePicker`, and the
reason they share one surface is that they were not sharing one before. The
filterable workspace list existed only inside `history-overlay.tsx`, while the
other two opened `DirectoryPicker` directly — so the two most common ways of
naming a directory started at `$HOME` and navigated down by mouse, even though the
backend already answers "which directories do you work in, most recent first" and
the component rendering that answer was fifty lines away.

`WorkspacePicker` ranks known workspaces by recency, filters on the full path
rather than the leaf name (the segment someone remembers is often in the middle:
`huawei`, `sylph`), and shows each one's session count so an empty directory is visible
before it is opened. It shares the workspace query with the history browser: a directory
whose environment is connected is already one click away in the sidebar, so the picker
remains a view over the server's one authoritative workspace list.

**Browse folders… is a footer button, not a row.** A row would be ranked against
the workspaces and dropped by the very query that failed to match any of them —
which is exactly the moment the filesystem is what the user needs. For the same
reason an empty list does not auto-fall-through to browsing: `Mod+N` opening a
different surface depending on how much history the machine has is worse than one
extra click, and the empty state names the next step.

`DirectoryPicker` is the fallback, and it is filterable too. It was a plain `<ul>`
of buttons — no filter, no arrow keys, no Enter — on the surface that backs every
"where should this go" question in the app, while `~/Workspace` holds dozens of
children and the listing caps at 2000. Both now use `ui/select-list`, so the
cursor, the ARIA pattern, and scroll-into-view are the same code rather than a
fourth hand-rolled copy.

Two things are specific to the folder browser:

- **Enter opens, `Mod+Enter` chooses.** A click has always navigated rather than
  chosen, because one gesture meaning both is the ambiguity that makes
  double-click pickers feel unpredictable. The keyboard inherits that, so the
  modifier supplies the gesture that was missing to make the keyboard path
  complete rather than merely present. It falls back to the listed directory when
  the cursor is on nothing, which is what the confirm button would do.
- **A path-shaped query navigates instead of filtering.** Typing or pasting
  `~/Workspace/x` is the fastest route to a directory the list has never shown,
  and `~` is expanded client-side from the `home` the roots listing carries.
  `pathFromQuery` decides which a query is, and is unit-tested: reading a plain
  filter as a path would request a directory the user never named.

Going up lives in the breadcrumb bar rather than as a row. As a row it was both
filterable — `par` matched "Parent folder" — and the row the cursor opened on, so
the first Enter in a fresh directory navigated *away* from it.

Each surface **replaces** the one before it rather than stacking, so exactly one
focus trap is ever mounted; see the mode-swap reasoning under
**Browsing history**.

## `/` — the start screen, not a session list

**`/` is an IDE-style start screen, not a session list.** A user opening the app has
arrived to **start** work or **resume** it, and a filterable listing answers neither — every
axis it could filter on is already owned by a better surface: the sidebar groups by
workdir, `Mod+K` searches by content, and `Mod+Shift+K` pages one workspace's full history
past any cap a single listing could show.

It has three tiers, and two properties that constrain the design:

1. **Identity and what a session is.** One line: a session pairs an agent with a
   working directory, and history exists for directories agents have run in. This is
   the only place the app states it, and a user who does not know it points a session
   at the wrong tree.
2. **The two create paths**, as buttons running `runAction`, each showing its live
   binding. Not links to a route: creation is a mutation, and pending and failure
   belong in the shell's one banner.
3. **The variable slot** — recents, or the first-run guide — then the key bindings.

**It paints before any request settles**, since tiers 1, 2, and the key list are
static. Only the middle slot waits on data, and it reserves its height rather than
collapsing.

**It adds no request.** The listing is the sidebar's, shared through
`lib/sessions-query.ts` and deduped by TanStack, which is what lets `/` be the default
route.

### Pending is not empty

`selectStartState` (`lib/welcome.ts`) distinguishes three states an empty array cannot:
settled-empty, pending, and failed. Only the first renders the onboarding guide. A failed
listing is no evidence that no sessions exist, and the sidebar already reports it with a
retry. It is a pure function with unit tests, since the branch is one line of JSX away from
being wrong and E2E cannot economically hold a listing open.

### First run teaches the first task, not the feature set

The empty state is three numbered steps — choose a directory, say what you want done,
watch the work — plus a pointer to browsing existing agent history without creating
anything. Numbered because the order is real: a directory must exist before a turn can
run.

Onboarding is **the empty state itself** rather than a dismissible layer. There is no
"seen" flag, no banner, and no preference: the guide is what having no sessions looks
like, so it disappears permanently after the first create and can never be stale.

### Recents are five *directories*, not five sessions

The unit is the directory, because resuming work means returning to a place. A row is
a directory, labelled by its path, and it opens the newest session there — which is
what "resume" means. The rest are a count (`3 sessions`); reaching a specific older
one is the sidebar's job, and it lists them all under that same directory.

Per-session rows would spend all five on whichever directory was worked in most
recently, since `title` is set only by native import or the API and untitled rows
differ by nothing but a timestamp. `recentWorkspaces` normalizes trailing slashes for
the same reason: the backend guarantees no spelling, and `/w/app` beside `/w/app/` is
one destination twice.

The path carries the row's identity rather than `sessionLabel`, whose `agent · leaf`
fallback reads as the directory named twice once the path is beside it. `fidelity` is
marked when it is not `stream`, since that decides whether a duration is recoverable;
nothing else on the row is badged.

A longer list adds little: the sidebar lists everything grouped this same way, and
`Mod+K` searches all of it.

### The pending slot reserves height; the loaded one does not

The reservation lives on the pending branch, not on the container. A container tall
enough for five rows leaves visible dead space above the key list for every user who
works in three directories — on screen every visit, which is strictly worse than the
one-time settle it was preventing. Three rows' worth is reserved, so a cold load
moves by a small amount in either direction rather than always dropping.

## Component Design

### Timeline rows

`components/execution-timeline.tsx` renders the bounded execution projection. The
disclosure contract lives in § Execution group disclosure. `lib/tool-shape.ts` interprets
the arguments of a loaded tool for its target, diff and detailed view. It never computes
subtree state. All body requests remain explicit.

### Shared primitives

`components/ui/` owns every recurring control, so padding, disabled state, and hover
are decided once rather than per call site.

| Module | Exports | Notes |
| --- | --- | --- |
| `ui/button.tsx` | `Button`, `IconButton`, `Kbd` | Variants are semantic (`primary`, `secondary`, `ghost`, `danger`, `chip`), so a caller picks by meaning. `IconButton` requires `label` — an icon-only control with no accessible name is the most repeated shape in the app, so the failure is a type error rather than an audit finding |
| `ui/menu.tsx` | `ContextMenu`, `Menu`, `MenuButton`, `MenuItem` | One behaviour, three positionings: anchored to a point (right-click), to a trigger (dropdown), or as a bottom sheet on mobile |
| `ui/sheet.tsx` | `Sheet`, `Drawer` | The mobile surfaces |
| `ui/select-list.tsx` | `useSelectList`, `SelectFilter`, `SelectRows`, `SelectScroll` | The one choice-list engine. Every menu, picker, and overlay list in the app; `lib/select-list.ts` holds the rules it renders. See below |
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

The mirror-image failure, which cost the three shortcut overlays their focus
restoration: **a descendant that focuses itself synchronously is captured as the
opener.** React's `autoFocus` fires during the commit, before this hook's mount
effect records `document.activeElement` — so a field declared that way *is* what the
effect sees, and closing the surface "restores" focus to a node that is unmounting,
dropping it on `<body>` (WCAG 2.4.3). `SelectFilter` defers its focus by a microtask,
which lands after every ancestor's effects without ever showing an unfocused frame.
No spec caught this for three overlays because the two whose restoration *was*
covered — the settings panel and the config picker — have no auto-focusing control at
all; `e2e/accessibility.spec.ts` now asserts it for each field that does.

### One choice list, and why there is no second one

Every list a user picks from — menus, the model catalogue, the workspace and folder
pickers, the command palette, search, and the history browser — is
`useSelectList` over rows from `lib/select-list.ts`. The rules are in the pure
module because that is what makes them testable without a DOM: what is shown, what
is elided, and what number each row carries.

**The invariant is one cursor, assigned where the rows are built.** `buildRows` returns rows
in render order, each already carrying its own index — or `null` when it cannot be chosen. No
renderer computes one, so `activeIndex` cannot be interpreted against a second list. A cursor
counted over *enabled items* against a renderer numbering *all rows* would put the highlight,
`aria-activedescendant`, and the committed value on three different rows.

`useListNavigation` in `overlay.tsx` was a second engine and is **deleted** rather than
deprecated, since a hook that still compiles is a pattern the next picker copies. It took a
row count, so each caller numbered its own rows, and it could not draw a group heading or
host a filter field — which is why the palette hand-rolled grouping and four call sites each
re-derived the combobox ARIA.

Three fields let the engine absorb the last three callers with their capabilities intact:

| Field | For | Why it is shaped this way |
| --- | --- | --- |
| `keywords` | The palette's aliases — `cancel` reaching "Stop the running turn" | Matched, never rendered, and ranked *below* the visible hint: a row that matched on hidden text shows nothing explaining why it is there, so it must not displace one whose visible text matched |
| `excerpt` | A search hit's match snippet | Rendered, never matched. It is the *product* of FTS5 ranking over a transcript the browser never loaded, so re-ranking by it would launder that ranking through a weaker one — and without it a hit is a title that does not contain the query |
| `external` | The search overlay's query | Ranking stays with the caller. Hits are ranked server-side by FTS5; `buildRows` could only re-score them by title, which is precisely the text that did not match. The query has to leave the hook anyway — it drives a debounced request |

`external` should stay rare, and the reason is worth stating: a caller that ranks
its own rows also owns keeping `detail` searchable, which is the guarantee the row
model exists to make. Everything else — numbering, heading elision, one checked row
per group — still runs.

**A heading is a `disabled` row**, which is the same flag that gives it no cursor
position and makes a query elide it. One fact, not three agreeing ones.

**The ARIA pattern is chosen by whether there is a text field**, because a
`role="menu"` may only contain menu items — an `<input>` in one is invalid ARIA that
axe reports and that leaves a screen reader with a control its container says cannot
exist. So a filterable list is the combobox pattern instead: input as `combobox`,
rows as a `listbox` of `option`s, `aria-selected` for the current value,
`aria-activedescendant` for the cursor. Two consequences found by auditing with a
list actually open, which a static read does not catch:

- **A `separator` is a legal child of a `menu` and not of a `listbox`**, which admits
  only options and groups. The same separated row therefore draws an announced rule
  in one pattern and a decorative one in the other.
- **The role is claimed only while there are options to contain.** The container also
  holds the states that are not rows — a spinner, a retry button, "no matches" — and
  a `listbox` around one of those is the same `aria-required-children` failure. For
  the same reason `aria-expanded` tracks whether the list is rendered rather than
  being a constant.

### Menus replaced every `<select>`

Not a styling preference. **A `<select>` is sized by its longest option, not by
its current value.** The composer's agent, model, and effort controls were
native selects, so an agent declaring `claude-opus-5-20260501` made that control
~180px wide regardless of what was selected; three of them plus Send exceeded a
390px viewport. Because the width came from content the user could not see, no
amount of `min-w-0` or `truncate` could recover it — the intrinsic minimum was
already too large. This is the direct cause of the "composer runs off
the screen".

A button that renders the *current* value can truncate. A `<select>` cannot. The
secondary benefit is that a native select cannot be styled: on iOS it renders as
a system wheel with its own typography, which is exactly the
assembled-from-parts quality this pass removed.

Menus use `menu`/`menuitem` with `aria-activedescendant` for the virtual cursor,
and `menuitemradio` + `aria-checked` where a group is a single choice. Two ordering
rules the implementation depends on:

- **Focus moves in a second effect, once `position` commits.** The panel carries
  `invisible` while it measures, and a `visibility: hidden` element is not focusable,
  so focus taken during the measuring effect goes nowhere and Escape never reaches the
  handler.
- **Dismiss-on-outside-pointerdown is inert on the sheet path.** The ref it tests is
  the popover's, which the sheet never attaches, so every `contains()` reads false and
  the pointerdown beginning a tap closes the sheet before the click lands.

## Icons

`components/icons.tsx` — 48 hand-authored icons on a 24-unit grid, drawn
at 16px, stroked in `currentColor` at 1.75. No dependency.

Paths rather than Unicode glyphs, and rather than a package. A dingbat is **font
data**: the platform decides what it looks like, so emoji presentation, `.notdef`
boxes in a CJK fallback chain, and shapes with no relation to their meaning are all
outside this project's control. Thirty-odd hand-authored paths cost about 2 KB and
tree-shake per route, which is well inside the budget an icon package exceeded.

Conventions:

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
`frontend/scripts/icon-sheet.mjs` renders every icon at 28px and 16px in both themes to
`/tmp/icon-sheet-*.png`. Run it after touching an icon.

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

## The composer

One bordered surface holding attachments, the message, and the session
configuration that turn will use. The layout rule is that **the message owns its
own row** and the controls sit beneath it, which is what makes it usable on a
phone.

While a turn runs, Steer and Queue are explicit modes above the message. Their availability
comes from the live capability surface, and an unavailable mode states its reason. The Send
control remains visible on touch screens. Text stays editable while a submission is pending;
only sending again is disabled. Attachments belong to ordinary turns, and cannot silently
drop from a mid-turn text submission.

The shared pending queue sits above the composer as a compact list. Each item names its
submitter, text, delivery mode, state, and failure when present. Accepted or queued items
offer edit, cancel, and immediate steer when the session can steer. A dispatched item is
immutable, because its consumption may be uncertain. Recent finished requests are available
behind a disclosure; pending rows appear in server sequence order with bounded rendering.
The queue is read from the server after navigation, refresh, and reconnect.

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
how many flags an agent declares, so the transcript would reflow when a model
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

## Two kinds of command

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

## PWA

Installable, so a phone user gets an icon and a standalone window rather than a
tab they have to find again.

| Piece | Where | Why |
| --- | --- | --- |
| Manifest | `app/manifest.ts` | A route handler, not a static file, so `theme_color` stays in step with the `--canvas` token from one place |
| Icons | `public/icons/*.svg` + generated PNGs | 192 and 512 for the manifest, 180 for `apple-touch-icon`, plus a separate maskable 512 |
| Generator | `frontend/scripts/generate-icons.mjs` | Rasterises the SVGs through Playwright's Chromium |
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

## Design tokens

Every colour resolves through a semantic token declared in `app/globals.css`. No
component names a palette step: `bg-gray-50` and `text-gray-500` do not appear
outside that file, and a build that reintroduces one is a regression.

One declaration site is what keeps a visual change a one-file edit. Hardcoded steps put it
across every component that names one, at which point consistency is a matter of remembering.

| Group | Tokens | Meaning |
| --- | --- | --- |
| Surface | `canvas`, `sunken`, `raised`, `hover`, `active` | Back to front: the page, a recessed well (sidebars, code blocks), anything sitting on the page (menus, the composer), then two interaction states |
| Border | `line`, `line-strong` | Hairline, and the stronger edge for a control |
| Ink | `ink`, `ink-soft`, `ink-muted`, `ink-faint` | By emphasis. `faint` is **decoration only** — it does not clear 4.5:1 on any surface |
| Accent | `accent`, `accent-hover`, `accent-ink`, `accent-ink-muted` | The primary action and text placed on it. Inverts in dark mode, which is why `accent-ink` exists rather than a hardcoded `text-white` |
| Status | `danger`, `success`, `warning`, `info`, each with `-bg`, `-line`, `-solid` | One ink per hue; the tint and edge derive from it |
| Focus | `focus` | Blue, not the accent grey — see below |

`*-bg` and `*-line` are `color-mix()` against `--canvas` rather than hand-picked
steps, so the dark theme gets correct tints for free: overriding the base hue
and the canvas re-derives both.

**The light palette is Tailwind's own `gray`/`red`/`emerald`/`amber`/`blue`/`violet` steps,
copied verbatim in oklch.** Several components carry measured WCAG ratios in their comments,
and a token that merely *looked* similar would invalidate every one of them. Only dark mode
introduces new values.

Two tokens changed meaning rather than spelling:

- **`ink-muted` is gray-600, not gray-500.** Both were in use for the same job.
  gray-500 measures 4.83:1 on white and fails on `sunken`; gray-600 is 7.4:1 and
  passes on both.
- **`stopping` and `interrupted` share the warning hue.** Two adjacent hues are
  indistinguishable at badge size, so the distinction that matters — whether this is still
  moving — is carried by the pulsing dot `stopping` shares with `running`.

### Why the palette lives in plain `:root`

Tailwind 4 can tree-shake unused `@theme` variables out of the emitted CSS. A
token referenced *only* by another token's `color-mix()` — every `*-bg` and
`*-line` — is exactly what that would drop, and the failure mode is an invisibly
wrong colour rather than a build error. So the values live in a plain `:root`
block Tailwind never rewrites, and `@theme inline` maps them onto utility names.

### Dark mode

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

## Type scale

Five steps: `text-3xs` (10px), `text-2xs` (11px), then Tailwind's `xs`, `sm`, `base`. The two
smallest exist for dense chrome, and nothing smaller than `3xs` has a token because it is not
readable. Timeline density maps onto the same scale — compact `xs`, normal `sm`, comfortable
`base`.

## Focus

One utility, `focus-ring`, and it is an **outline** rather than a ring. A ring is a
`box-shadow`, so it is clipped by every `overflow-hidden` ancestor, which covers file-tree
rows and the code-block header; an outline is never clipped. The colour is blue rather than
the accent grey, since a grey ring is invisible against the accent surfaces it appears on.

## Typography

A system stack, no webfont: `system-ui`, then `-apple-system`, `Segoe UI
Variable Text`, and CJK faces (`PingFang SC`, `Hiragino Sans GB`, `Microsoft
YaHei`) before the generic fallback.

A local-first app renders text without the network, an installed PWA works offline, and
`next build` runs in E2E on whatever machine is to hand — a build-time font fetch is a
build-time network dependency. The CJK entries are what keep a mixed Chinese-English string in
one face rather than falling back per glyph.

### Execution group disclosure

Execution groups default to collapsed at the query boundary. User input and outward
assistant replies stay visible in narrative order. A light gray summary describes the
work, followed on expansion by individual execution summaries and then details. Active
summaries name one stable current activity and the number of other running items.
Failures and waiting requests use explicit text and an attention color even when closed.

The entire summary row is a keyboard-operable button with `aria-expanded`. Reserve
chevron space: hide the collapsed chevron until hover or keyboard focus, including on
touch screens; keep the down chevron visible while open. Active text pulses opacity
without changing layout, honors reduced motion, and stops when settled. Collapse all
only closes expanded content. There is no expand-all action or shortcut.
