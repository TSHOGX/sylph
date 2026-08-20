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

## Design Philosophy

**PRIMARY DIRECTIVE: DeepSeek Harness is the Reference Implementation**

When implementing Phase 2, **directly reference DeepSeek Harness source code**:

```
/Users/hsw/Workspace/oss/deepseek-harness/repo/apps/web/
/Users/hsw/Workspace/oss/deepseek-harness/repo/packages/web/
```

**Implementation rules:**
1. **When uncertain about UI/UX details** → Read DSH source code, do not guess
2. **After implementing a feature** → Compare with DSH source to verify approach
3. **Only deviate from DSH** when you are **highly confident** your design is superior
4. **Default to DSH's patterns** — component structure, state management, event handling, styling

**Rationale:** DSH is a proven, production-ready coding-agent interface. Reinventing patterns wastes time and introduces risk. Copy what works, optimize only what Sylph requires differently (dual-origin sessions, TraceDB integration, lifecycle indicators).

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

**Core principles:**
1. **Origin-aware UI** — Visual distinction between Sylph-managed and native sessions
2. **Real-time first** — SSE-driven updates with optimistic rendering
3. **Local-first** — No external dependencies, works offline after initial load
4. **Accessibility** — WCAG 2.1 AA compliance, keyboard navigation, screen reader support
5. **Performance** — Virtual scrolling for large trajectories, code splitting, <200KB bundle

**What to copy from DeepSeek Harness:**
- Overall layout structure (left rail + canvas)
- Conversation canvas component hierarchy
- Message bubble rendering (user/assistant/thinking/tool_call)
- Streaming text animation patterns
- Input area with attachment handling
- Thinking block collapse/expand behavior
- Tool call result formatting
- Code block syntax highlighting setup
- Error state UI patterns
- Loading skeleton designs

**Sylph-specific additions (not in DSH):**
- Dual-origin session indicators (🟢 Sylph / 🟣 Native)
- Preservation policy warnings for native sessions
- Lifecycle indicators (24h idle, 04:00 reset timing)
- Per-session agent/model/effort flags in header
- TraceDB search with deduplication
- Session incarnation history (backend lifecycle tracking)

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
- **No component library.** The four interactive primitives needed — modal,
  context menu, disclosure, listbox — are implemented directly in
  `components/overlay.tsx`, `components/context-menu.tsx`, native `<details>`,
  and the two picker overlays. Radix UI would have supplied them with better
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
│   ├── page.tsx                # All-sessions list
│   ├── search/page.tsx         # TraceDB archive search and import
│   ├── sessions/
│   │   ├── [id]/page.tsx       # Conversation canvas
│   │   └── new/page.tsx        # Create session
│   └── globals.css
│
├── components/
│   ├── app-shell.tsx           # Three-pane frame, top bar, Quick Start
│   ├── workspace-sidebar.tsx   # Left pane: sessions grouped by workdir
│   ├── artifacts-pane.tsx      # Right pane: file tree
│   ├── file-preview.tsx        # Right pane: one file
│   ├── timeline.tsx            # The conversation canvas itself
│   ├── markdown.tsx            # Markdown parser and renderer
│   ├── code-block.tsx          # Code and command output
│   ├── diff-view.tsx           # Unified inline diff
│   ├── overlay.tsx             # Modal shell, focus trap, list navigation
│   ├── search-overlay.tsx      # Cmd+K
│   ├── command-palette.tsx     # Cmd+P
│   ├── settings-panel.tsx      # Cmd+,
│   ├── context-menu.tsx        # Right-click menus
│   ├── attachment-picker.tsx
│   ├── command-dialog.tsx      # Agent-declared commands
│   └── badges.tsx              # Origin and status badges
│
├── lib/
│   ├── api.ts                  # Typed client; mirrors the wire shape exactly
│   ├── providers.tsx           # QueryClient + AuthGate
│   ├── settings.tsx            # Preferences over localStorage
│   ├── workspace-ui.tsx        # Pane visibility, overlays, artifact selection
│   ├── shortcuts.ts            # Combo grammar and platform resolution
│   ├── timeline-rows.test.ts   # (tests live beside the code they cover)
│   ├── diff.ts                 # LCS line diff
│   ├── tool-shape.ts           # Interprets tool_call payloads per agent
│   ├── fuzzy.ts                # Subsequence ranking
│   ├── use-sse.ts              # EventSource with after_seq resume
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
│ ▾ sylph-web 8│ ▤ Read  backend/src/auth.rs    ✓   │     ▫ auth.rs    │
│   ● session A│                                    │ ▫ Cargo.toml     │
│   ◆ session B│ I'll add logging at three points.  │                  │
│ ▾ trace-db  3│                                    │ ─────────────────│
│   ● session C│ ✎ Edit  backend/src/auth.rs    ✓   │ auth.rs          │
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
| `Mod+P` | Command palette |
| `Mod+N` | New session |
| `Mod+Shift+N` | Quick Start in `~/.agents` |
| `Mod+B` / `Mod+\` | Toggle left / right pane |
| `Mod+,` | Settings |
| `Mod+.` | Stop the running turn |
| `Mod+F` | Filter turns in this session |
| `Shift+E` / `Shift+C` | Expand / collapse all disclosures |
| `Escape` | Close the top overlay (not rebindable) |

`Mod` is Cmd on macOS and Ctrl elsewhere, resolved at match time. Bindings are
stored platform-neutral, so a stored `Meta+k` cannot silently stop working for the
same user on another machine.

Bare-key combos (`Shift+E`) are suppressed while a text field has focus, so the
composer keeps its keystrokes; `Mod`-prefixed combos are global on purpose.

Session-scoped actions — stop turn, expand all, find — cannot be handled by the
shell, which has no access to the open session's state. The session page registers
a handler with `registerSessionActions` and the shell forwards to it; with no
session open, those actions are simply ignored.

### Component Design

#### Session Card

```tsx
<SessionCard>
  <OriginBadge origin={session.origin} />
  <Title>{session.title}</Title>
  <Meta>
    {session.agent} · {session.flags.model}
  </Meta>
  <Timestamp>{relativeTime(session.updatedAtMs)}</Timestamp>
  <StatusDot status={session.status} />
</SessionCard>
```

**Variants:**
- Active: blue border, bold title
- Idle: normal
- Running: pulsing status dot
- Native: violet badge, warning icon if user hovers

#### Message Blocks

**User Message:**
```tsx
<UserMessage>
  <Avatar>👤</Avatar>
  <Content>
    <Text>{message.text}</Text>
    {message.attachments.map(att => (
      <Attachment type={att.type} name={att.name} />
    ))}
  </Content>
</UserMessage>
```

**Thinking Block (collapsible):**
```tsx
<ThinkingBlock expanded={uiState.expanded}>
  <Header onClick={toggleExpand}>
    💭 Thinking
    <CollapseIcon />
  </Header>
  {expanded && (
    <Content fade>
      {thinkingText}
    </Content>
  )}
</ThinkingBlock>
```

**Assistant Message:**
```tsx
<AssistantMessage>
  <Avatar>🤖</Avatar>
  <Content>
    <Markdown>{message.text}</Markdown>
  </Content>
  <Actions>
    <CopyButton />
    <RetryButton />
  </Actions>
</AssistantMessage>
```

**Tool Call Block:**
```tsx
<ToolCallBlock>
  <Header>
    <ToolIcon name={toolCall.name} />
    {toolCall.name}
    <StatusBadge phase={toolCall.phase} />
  </Header>
  <CodeBlock language="typescript">
    {toolCall.text}
  </CodeBlock>
</ToolCallBlock>
```

### Color System & Theming

**Origin indicators:**
- Sylph: `green-500` (#10b981) — trustworthy, managed
- Native: `violet-500` (#8b5cf6) — preserved, external

**Status colors:**
- Idle: `gray-400`
- Running: `blue-500` (pulsing)
- Completed: `green-500`
- Failed: `red-500`
- Stopping: `amber-500`

**Theme support:**
- Light mode (default)
- Dark mode (auto via `prefers-color-scheme`)
- Explicit toggle in settings
- CSS variables for all semantic colors

**Typography:**
- Headings: Inter (system font stack fallback)
- Body: Inter
- Code: JetBrains Mono (fallback to monospace)

## State Management

### Server State (TanStack Query)

Query keys follow a hierarchical structure:

```ts
// Session list
['sessions', filters] → useQuery

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

## Real-Time Event Streaming

### SSE Connection Management

```ts
class TurnStreamManager {
  private connections = new Map<string, EventSource>()
  
  connect(turnId: string, afterSeq = -1): EventSource {
    const url = `/api/v1/turns/${turnId}/events?after_seq=${afterSeq}`
    const source = new EventSource(url)
    
    source.addEventListener('thinking', (e) => this.handleEvent('thinking', e))
    source.addEventListener('assistant', (e) => this.handleEvent('assistant', e))
    source.addEventListener('tool_call', (e) => this.handleEvent('tool_call', e))
    source.addEventListener('tool_result', (e) => this.handleEvent('tool_result', e))
    source.addEventListener('usage', (e) => this.handleEvent('usage', e))
    source.addEventListener('error', (e) => this.handleReconnect(turnId))
    
    this.connections.set(turnId, source)
    return source
  }
  
  disconnect(turnId: string) {
    const source = this.connections.get(turnId)
    if (source) {
      source.close()
      this.connections.delete(turnId)
    }
  }
  
  private handleReconnect(turnId: string) {
    const lastSeq = this.getLastSeq(turnId)
    setTimeout(() => this.connect(turnId, lastSeq), 1000)
  }
}
```

**Event normalization:**
- Parse JSON from `data` field
- Track sequence numbers for replay
- Distinguish `started` / `delta` / `completed` phases
- Merge deltas into live projection
- Persist only `completed` events

**Reconnection strategy:**
- Exponential backoff: 1s, 2s, 4s, 8s, max 30s
- Resume from `Last-Event-ID` or `after_seq`
- Replay stable events from backend journal
- Show reconnection UI after 3 seconds

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
  
  // Merge and deduplicate by trace_session_id
  const merged = useMemo(() => {
    const live = liveResults.sessions
    const trace = traceResults?.sessions || []
    
    const traceIds = new Set(live.map(s => s.traceSessionId).filter(Boolean))
    const uniqueTrace = trace.filter(s => !traceIds.has(s.traceSessionId))
    
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

## Performance Optimization

### Bundle budget

**Measured: 196.2 KB gzipped first-load JS, against a 200 KB budget.** Phase 4
established the budget at 200 KB and measured 187.8 KB; Phase 6 added the shell,
three overlays, the file browser, the Markdown renderer, the diff view, and the
fuzzy ranker for **+8.4 KB**.

That number is why this document lists so many "no dependency" decisions. The
naive versions of these features — `react-markdown` + `remark-gfm` (~45 KB), Shiki
or Prism (~35 KB), jsdiff (~30 KB), Radix (~30 KB for the primitives used), fuse.js
(~12 KB), an icon package — total well over 150 KB against 12 KB of headroom.
Each replacement is a bounded, well-understood algorithm carrying unit tests; that
is the trade, stated plainly, and it is a trade that stops being right the moment
the budget rises or the feature set outgrows the subset implemented.

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

### File browsing

The Artifacts tree fetches **one directory per expansion**. Walking the tree
server-side would spend seconds on a 100k-file repository building a payload whose
first screen is all anyone reads. Each level owns its own query, so an expanded
subtree is cached independently and collapsing does not discard it. Listings cap at
2000 entries per directory and report `truncated`.

Previews cap at 1 MiB, truncating text to a 256 KiB prefix. Images are
all-or-nothing, because a truncated image decodes to nothing.

## Accessibility

Target: WCAG 2.1 AA. What that means concretely here, and how each part is
checked.

### Automated: axe-core, in CI

`e2e/accessibility.spec.ts` runs axe-core against four surfaces with real content —
the session list, the create form, a conversation canvas holding tool calls, a
diff, and a failed command with both sidebars open, and each of the three overlays
while open. Tags: `wcag2a`, `wcag2aa`, `wcag21a`, `wcag21aa`. **Currently zero
violations.**

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
- Session list (isolated failure)
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

**Empty states:**
```tsx
function SessionList({ sessions }: Props) {
  if (sessions.length === 0) {
    return (
      <EmptyState>
        <Icon name="inbox" size={48} />
        <Title>No sessions yet</Title>
        <Description>
          Create your first Sylph session to get started
        </Description>
        <Button onClick={openCreateDialog}>
          <Icon name="plus" /> New Session
        </Button>
      </EmptyState>
    )
  }
  // ...
}
```

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

**81 tests over the pure logic that replaced a dependency.** That is the
selection rule: hand-rolling a diff, a Markdown parser, and a fuzzy ranker means
taking on the test burden those libraries carried, and this is where that debt is
paid.

| File | Covers |
| --- | --- |
| `lib/diff.test.ts` | LCS alignment, line numbering against the original files, `-` before `+`, no phantom trailing line, prefix/suffix stripping speed, degradation above the size cap |
| `lib/markdown-parse.test.ts` | Block and inline parsing, code not re-scanned, `javascript:`/`data:` links refused, termination on pathological input |
| `lib/tool-shape.test.ts` | Each shipped runner's payload shape, name classification, hunk extraction, never throwing on malformed `data` |
| `lib/fuzzy.test.ts` | Subsequence matching and *relative ranking* — boundary and adjacency bonuses, field weighting |
| `lib/shortcuts.test.ts` | Canonical combo spelling, Cmd/Ctrl unification, recorder and dispatcher agreeing |
| `lib/timeline-rows.test.ts` | Delta accumulation, tool-result pairing, interleaved streams, referential stability of settled rows |

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

**18 specs.** Both servers start automatically on offset ports with a throwaway
data directory and a mock runner, so a run never touches the real journal,
archive, or agents.

| File | Covers |
| --- | --- |
| `session-lifecycle.spec.ts` | Create, send, stream, reload-replay, sidebar grouping |
| `timeline.spec.ts` | Tool calls as regions, inline diff, failure auto-expansion, Markdown, 1000-event render budget |
| `workspace-shell.spec.ts` | Pane toggles and persistence, palette, search, focus restoration, settings round-trip, Artifacts tree |
| `accessibility.spec.ts` | Skip link, focus trapping and restoration, axe-core audit of four surfaces |

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

## Future Enhancements (Phase 3+)

### Advanced Features

- **Session management:**
  - Rename sessions inline
  - Archive/unarchive
  - Export to JSON
  - Fork session at specific turn

- **Search:**
  - Full-text search across all turns
  - Filter by agent, status, date
  - Highlight matches in context

- **Attachments:**
  - Image previews in conversation
  - PDF viewer
  - Code file syntax highlighting
  - Drag-and-drop upload

- **Lifecycle:**
  - Manual reset for Sylph sessions
  - Convert native → Sylph (with warning)
  - Incarnation history viewer

### Performance

- **Virtual scrolling:** for 10K+ events
- **Lazy loading:** below-fold content
- **Code splitting:** per-route bundles
- **Service worker:** offline support

### Collaboration (Post-MVP)

- **Sharing:** export session as shareable HTML
- **Comments:** annotate specific turns
- **Diff view:** compare sessions side-by-side

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

What was built instead: `components/overlay.tsx` (focus trap, Escape, focus
restoration, scroll lock), `components/context-menu.tsx` (arrow navigation,
viewport clamping, outside-dismiss), native `<details>`, and the combobox pattern in
the two pickers. The axe audit and the focus tests exist to hold that decision
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


