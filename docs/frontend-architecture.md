# Frontend Architecture (Phase 2)

Sylph Web frontend is a Next.js 16 application providing a local-first UI for multi-agent session management. The design draws inspiration from modern coding-agent interfaces while optimizing for Sylph's unique multi-origin session model and real-time event streaming.

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
- TanStack Query v5 (server state, caching, optimistic updates)
- Zustand (client state, UI preferences)
- EventSource API (SSE streaming)

**UI components:**
- Radix UI (accessible primitives: Dialog, Dropdown, Tabs, Toast)
- Tailwind CSS 4.0 (utility-first styling)
- Framer Motion (animations, transitions)
- Lucide React (icons)

**Code rendering:**
- Shiki (syntax highlighting, theme-aware)
- React Markdown (message formatting)

**Testing:**
- Vitest (unit tests)
- Playwright (E2E tests)
- React Testing Library (component tests)

**Development:**
- Turbopack (dev server)
- ESLint + Prettier (linting, formatting)
- TypeScript strict mode

## Application Structure

```
frontend/
├── src/
│   ├── app/                      # Next.js App Router
│   │   ├── layout.tsx           # Root layout (theme provider, auth)
│   │   ├── page.tsx             # Home (session list)
│   │   ├── sessions/
│   │   │   ├── [id]/
│   │   │   │   └── page.tsx    # Conversation view
│   │   │   └── new/
│   │   │       └── page.tsx    # Create session dialog
│   │   ├── api/                 # Proxy routes to Rust backend
│   │   │   └── [...proxy]/
│   │   │       └── route.ts
│   │   └── globals.css
│   │
│   ├── components/              # UI components
│   │   ├── ui/                 # Radix primitives (Button, Dialog, etc.)
│   │   ├── session/            # Session-specific components
│   │   │   ├── SessionCard.tsx
│   │   │   ├── SessionList.tsx
│   │   │   ├── CreateSessionDialog.tsx
│   │   │   └── OriginBadge.tsx
│   │   ├── conversation/       # Message canvas components
│   │   │   ├── ConversationCanvas.tsx
│   │   │   ├── MessageGroup.tsx
│   │   │   ├── ThinkingBlock.tsx
│   │   │   ├── AssistantMessage.tsx
│   │   │   ├── ToolCallBlock.tsx
│   │   │   ├── UserMessage.tsx
│   │   │   └── EventStream.tsx
│   │   ├── input/              # Input area
│   │   │   ├── MessageInput.tsx
│   │   │   ├── AttachmentPicker.tsx
│   │   │   └── AttachmentPreview.tsx
│   │   └── layout/             # Layout components
│   │       ├── SessionRail.tsx
│   │       ├── ConversationHeader.tsx
│   │       └── StatusIndicator.tsx
│   │
│   ├── lib/                     # Business logic
│   │   ├── api/                # API client
│   │   │   ├── client.ts       # Fetch wrapper
│   │   │   ├── sessions.ts     # Session endpoints
│   │   │   ├── turns.ts        # Turn endpoints
│   │   │   └── agents.ts       # Agent endpoints
│   │   ├── sse/                # SSE streaming
│   │   │   ├── connection.ts   # EventSource wrapper
│   │   │   ├── parser.ts       # Event normalization
│   │   │   └── reconnect.ts    # Reconnection logic
│   │   ├── hooks/              # React hooks
│   │   │   ├── useSession.ts
│   │   │   ├── useTurnStream.ts
│   │   │   ├── useAgents.ts
│   │   │   └── useSessionList.ts
│   │   └── utils/              # Utilities
│   │       ├── date.ts
│   │       ├── format.ts
│   │       └── validation.ts
│   │
│   ├── stores/                  # Client state (Zustand)
│   │   ├── ui.ts               # UI preferences (theme, collapsed state)
│   │   └── stream.ts           # Live event stream state
│   │
│   └── types/                   # TypeScript definitions
│       ├── api.ts              # Generated from OpenAPI
│       ├── events.ts           # Event stream types
│       └── ui.ts               # UI-specific types
│
├── public/                      # Static assets
│   ├── favicon.ico
│   └── fonts/
│
├── tests/                       # Tests
│   ├── unit/
│   ├── integration/
│   └── e2e/
│
├── next.config.js
├── tailwind.config.ts
├── tsconfig.json
└── package.json
```

## UI Layout & Design

### Overall Layout

Inspired by DeepSeek Harness's clean interface with Sylph-specific enhancements:

```
┌──────────────────────────────────────────────────────────────┐
│  Sylph Web                                      [Settings] ⚙  │
├────────────┬─────────────────────────────────────────────────┤
│            │  Feature Implementation · opus-5 · high    [🟢]  │
│  Sessions  │  /Users/hsw/Workspace/project               [···]│
│            ├─────────────────────────────────────────────────┤
│ ┌────────┐ │                                                  │
│ │Sylph 🟢│ │  ┌─────────────────────────────────────────┐   │
│ │Feature │ │  │ 👤 User                                  │   │
│ │Impl... │ │  │ Implement feature X with proper tests   │   │
│ └────────┘ │  └─────────────────────────────────────────┘   │
│            │                                                  │
│ ┌────────┐ │  ┌─────────────────────────────────────────┐   │
│ │Native🟣│ │  │ 💭 Thinking                         [−]  │   │
│ │Refactor│ │  │ Analyzing the codebase structure...     │   │
│ └────────┘ │  └─────────────────────────────────────────┘   │
│            │                                                  │
│ [+ New]    │  ┌─────────────────────────────────────────┐   │
│            │  │ 🤖 Assistant                             │   │
│ [Search]   │  │ I'll implement feature X by...          │   │
│            │  │                                          │   │
│            │  │ ┌─ Write ──────────────────────────┐    │   │
│            │  │ │ file_path: src/feature.rs        │    │   │
│            │  │ │ content: ...                     │    │   │
│            │  │ └──────────────────────────────────┘    │   │
│            │  └─────────────────────────────────────────┘   │
│            │                                                  │
│            │  ┌─────────────────────────────────────────┐   │
│            │  │ Type a message...               [📎] [⏎]│   │
│            │  └─────────────────────────────────────────┘   │
└────────────┴─────────────────────────────────────────────────┘
```

**Left Rail (240px fixed width):**
- Session list with virtual scrolling
- Origin badge (🟢 Sylph / 🟣 Native)
- Title truncation with tooltip
- Active session highlight
- "New Session" button
- Search input

**Main Canvas (flex-grow):**
- **Header bar:**
  - Session title (editable on hover)
  - Agent + model + effort flags
  - Status indicator (idle/running/failed)
  - Actions menu (rename, archive, stop)
  
- **Message area (scrollable):**
  - Auto-scroll during streaming
  - Scroll lock when user scrolls up
  - Virtual scrolling for 1000+ events
  
- **Input area (fixed bottom):**
  - Auto-expanding textarea (max 10 lines)
  - Attachment picker (drag/drop, file picker)
  - Attachment previews with remove button
  - Send button (Enter to send, Shift+Enter for newline)

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

### Client State (Zustand)

```ts
// UI preferences store
interface UIStore {
  theme: 'light' | 'dark' | 'system'
  sidebarCollapsed: boolean
  expandedThinkingBlocks: Set<string> // by turn ID
  toggleThinking: (turnId: string) => void
}

// Stream state store
interface StreamStore {
  activeStreams: Map<string, EventSource> // turnId → connection
  liveEvents: Map<string, Event[]> // turnId → ephemeral events
  addEvent: (turnId: string, event: Event) => void
  finalizeEvent: (turnId: string, seq: number) => void
}
```

**Persistence:**
- UI preferences → localStorage
- Stream state → in-memory only (ephemeral)

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

### Virtual Scrolling

For sessions with 1000+ events, use virtual scrolling:

```tsx
import { useVirtualizer } from '@tanstack/react-virtual'

function ConversationCanvas({ turns }: Props) {
  const parentRef = useRef<HTMLDivElement>(null)
  
  const virtualizer = useVirtualizer({
    count: turns.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 200, // avg turn height
    overscan: 5
  })
  
  return (
    <div ref={parentRef} className="h-full overflow-auto">
      <div style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map(item => (
          <div
            key={item.key}
            style={{
              position: 'absolute',
              top: 0,
              left: 0,
              width: '100%',
              transform: `translateY(${item.start}px)`
            }}
          >
            <TurnView turn={turns[item.index]} />
          </div>
        ))}
      </div>
    </div>
  )
}
```

### Code Splitting

**Route-based splitting:**
- `/` (session list) — lazy load conversation components
- `/sessions/[id]` — lazy load input/stream components
- `/sessions/new` — lazy load dialog

**Component-based splitting:**
```tsx
const ThinkingBlock = lazy(() => import('./ThinkingBlock'))
const CodeBlock = lazy(() => import('./CodeBlock'))
const AttachmentPreview = lazy(() => import('./AttachmentPreview'))
```

### Bundle Size Target

- Initial load: <150KB (gzip)
- Route chunks: <50KB each
- Total budget: <200KB
- Use `next/dynamic` for heavy components
- Tree-shake unused Radix primitives

### Image Optimization

**Attachment previews:**
- Use Next.js `<Image>` component
- Lazy load below fold
- Blur placeholder for images
- WebP format with fallback
- Max display width 800px

## Accessibility

### WCAG 2.1 AA Compliance

**Keyboard navigation:**
- Tab through sessions, messages, actions
- Arrow keys in session list
- Enter to open session
- Escape to close dialogs
- Focus trapping in modals

**Screen reader support:**
- Semantic HTML (`<main>`, `<nav>`, `<article>`)
- ARIA labels for icons
- Live regions for streaming updates
- Alt text for images
- Status announcements

**Color contrast:**
- All text meets 4.5:1 ratio
- Interactive elements meet 3:1
- Focus indicators 3:1 against background
- Error states use color + icon

**Focus management:**
- Visible focus rings (outline, not box-shadow)
- Skip to content link
- Focus returns to trigger after dialog close
- Focus moves to first error on validation

### Testing Checklist

- [ ] VoiceOver (macOS)
- [ ] NVDA (Windows)
- [ ] Keyboard-only navigation
- [ ] Axe DevTools audit
- [ ] Color contrast checker
- [ ] High contrast mode

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

### Unit Tests (Vitest)

**Test utilities:**
```tsx
// tests/utils/test-utils.tsx
export function renderWithProviders(ui: ReactElement) {
  return render(
    <QueryClientProvider client={testQueryClient}>
      <ThemeProvider>
        {ui}
      </ThemeProvider>
    </QueryClientProvider>
  )
}

export function createMockSession(overrides?: Partial<Session>): Session {
  return {
    id: 's_test',
    origin: 'sylph',
    agent: 'claude',
    workdir: '/test',
    title: 'Test Session',
    status: 'idle',
    ...overrides
  }
}
```

**Component tests:**
```tsx
describe('SessionCard', () => {
  it('shows Sylph badge for Sylph sessions', () => {
    const session = createMockSession({ origin: 'sylph' })
    const { getByText } = renderWithProviders(<SessionCard session={session} />)
    expect(getByText('🟢 Sylph')).toBeInTheDocument()
  })
  
  it('shows native badge for native sessions', () => {
    const session = createMockSession({ origin: 'native' })
    const { getByText } = renderWithProviders(<SessionCard session={session} />)
    expect(getByText('🟣 Native')).toBeInTheDocument()
  })
})
```

**Hook tests:**
```tsx
describe('useTurnStream', () => {
  it('accumulates delta events', async () => {
    const { result } = renderHook(() => useTurnStream('t_123'))
    
    act(() => {
      result.current.handleEvent({
        kind: 'thinking',
        phase: 'delta',
        seq: 1,
        text: 'Hello'
      })
      result.current.handleEvent({
        kind: 'thinking',
        phase: 'delta',
        seq: 2,
        text: ' world'
      })
    })
    
    expect(result.current.liveText).toBe('Hello world')
  })
})
```

### Integration Tests

**API mock server:**
```tsx
// tests/mocks/server.ts
import { setupServer } from 'msw/node'
import { rest } from 'msw'

export const server = setupServer(
  rest.get('/api/v1/sessions', (req, res, ctx) => {
    return res(ctx.json({ sessions: [createMockSession()] }))
  }),
  rest.post('/api/v1/sessions/:id/turns', (req, res, ctx) => {
    return res(ctx.status(202), ctx.json({ turn: createMockTurn() }))
  })
)
```

**Flow tests:**
```tsx
describe('Create session flow', () => {
  it('creates session and navigates to conversation', async () => {
    const { user } = renderWithProviders(<App />)
    
    // Open dialog
    await user.click(screen.getByText('New Session'))
    
    // Fill form
    await user.selectOptions(screen.getByLabelText('Agent'), 'claude')
    await user.type(screen.getByLabelText('Workdir'), '/test')
    await user.type(screen.getByLabelText('Title'), 'Test')
    
    // Submit
    await user.click(screen.getByText('Create'))
    
    // Verify navigation
    await waitFor(() => {
      expect(window.location.pathname).toMatch(/\/sessions\/s_/)
    })
  })
})
```

### E2E Tests (Playwright)

**Critical paths:**
```ts
test('send message and receive streaming response', async ({ page }) => {
  await page.goto('/')
  await page.click('text=New Session')
  await page.fill('[name=agent]', 'claude')
  await page.fill('[name=workdir]', '/test')
  await page.click('text=Create')
  
  // Wait for conversation
  await page.waitForURL(/\/sessions\/s_/)
  
  // Send message
  await page.fill('textarea', 'Hello')
  await page.press('textarea', 'Enter')
  
  // Verify streaming
  await page.waitForSelector('.thinking-block')
  await page.waitForSelector('.assistant-message')
  
  // Verify completed state
  await expect(page.locator('[data-turn-status="completed"]')).toBeVisible()
})
```

**Performance tests:**
```ts
test('renders 1000 turns without lag', async ({ page }) => {
  // Mock session with 1000 turns
  await page.route('/api/v1/sessions/*', route => {
    route.fulfill({
      json: { session: createSessionWith1000Turns() }
    })
  })
  
  await page.goto('/sessions/s_perf')
  
  // Measure render time
  const startTime = Date.now()
  await page.waitForLoadState('domcontentloaded')
  const duration = Date.now() - startTime
  
  expect(duration).toBeLessThan(2000) // <2s render
  
  // Verify virtual scrolling
  const visibleTurns = await page.locator('[data-turn]').count()
  expect(visibleTurns).toBeLessThan(50) // only ~20 rendered
})
```

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

## Phase 2 Deliverables

### Week 4: Foundation

- [ ] **Before starting: Read DSH `/apps/web/` structure**
- [ ] Next.js project setup with TypeScript
- [ ] Tailwind + Radix UI configuration (match DSH color tokens)
- [ ] API client with type safety
- [ ] Session list page (`/`) — **verify against DSH conversation list**
- [ ] Conversation page (`/sessions/[id]`) — **verify against DSH chat page**
- [ ] Basic layout (left rail + main canvas) — **match DSH layout proportions**
- [ ] Session card components — **verify card structure with DSH**
- [ ] TanStack Query setup
- [ ] **Week 4 checkpoint: Compare all components with DSH source**

### Week 5: Streaming & Interaction

- [ ] SSE connection manager — **check DSH SSE/WebSocket handling**
- [ ] Event stream rendering — **verify delta accumulation matches DSH**
- [ ] Message input with attachments — **match DSH input area behavior**
- [ ] Create session dialog — **verify form validation with DSH**
- [ ] Thinking block (collapsible) — **MUST match DSH animation timing**
- [ ] Tool call rendering with syntax highlighting — **verify Shiki config with DSH**
- [ ] Origin badges and warnings (Sylph-specific)
- [ ] Error boundaries — **match DSH error fallback UI**
- [ ] Loading states — **verify skeleton design with DSH**
- [ ] **Week 5 checkpoint: Side-by-side comparison with running DSH instance**

### Final Verification (Before Phase 2 Exit)

- [ ] **Open DSH web app locally and Sylph Web side-by-side**
- [ ] **Verify UI patterns match:**
  - [ ] Layout proportions (rail width, header height, input area)
  - [ ] Message bubble styling and spacing
  - [ ] Thinking block collapse/expand animation
  - [ ] Tool call result formatting
  - [ ] Code syntax highlighting theme
  - [ ] Loading skeleton structure
  - [ ] Error state illustrations
  - [ ] Input area behavior (focus, resize, attachment preview)
- [ ] **Document any deviations with justification**
- [ ] **If uncertain whether deviation is improvement, revert to DSH pattern**

- [ ] Origin badges and warnings
- [ ] Error boundaries
- [ ] Loading states

### Testing

- [ ] Unit tests for components
- [ ] Integration tests for flows
- [ ] E2E tests for critical paths
- [ ] Accessibility audit

### Documentation

- [ ] Component usage examples
- [ ] State management patterns
- [ ] SSE reconnection guide
- [ ] Performance optimization notes

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

### Why Radix over Headless UI?

- More primitives (Toast, Toolbar, etc.)
- Better accessibility defaults
- Active maintenance
- Composable API

### Why Zustand over Context API?

- Less boilerplate
- Better performance (no re-renders)
- DevTools support
- Simpler mental model

## References

- Backend API: `docs/api-design.md`
- Event protocol: `docs/architecture.md` (Event Streaming section)
- Database schema: `docs/database-schema.md`
- Product roadmap: `docs/roadmap.md`


