# Agent UI Design Analysis

This document analyzes agent interaction patterns from production coding agents to inform
Sylph Web's conversation canvas redesign. Unlike chat-oriented LLM UIs (ChatGPT, Gemini),
coding agents require a **workspace-oriented** interface that emphasizes tool execution,
code changes, and iterative problem-solving over conversational flow.

## Core Insight: Workspace vs Chat

**Chat UIs** (ChatGPT, Gemini, 豆包):
- Message bubbles with backgrounds/borders
- User messages right-aligned, assistant left-aligned
- Emphasis on conversational flow
- Tool calls often hidden or minimized
- Linear timeline

**Agent UIs** (Claude Code, Cursor, Zed, VS Code Copilot):
- Flat, timeline-based layout (no bubbles)
- Tool execution is primary, text is secondary
- Code diffs and file changes prominent
- Inline, collapsible details
- Non-linear: can jump to file changes, re-run tools

**Sylph Web should follow the Agent pattern.**

---

## Reference Analysis

### 1. Claude Code (CLI/Desktop)

**Observation from usage:**

```
┌─────────────────────────────────────────────────┐
│ > User: Add logging to the auth module          │
├─────────────────────────────────────────────────┤
│ 🤔 Thinking...                                   │
│   (collapsed by default, click to expand)       │
├─────────────────────────────────────────────────┤
│ I'll add structured logging to track auth flow  │
│                                                  │
│ 🔧 Read backend/src/auth.rs                     │
│    (file content preview, collapsible)          │
│                                                  │
│ 🔧 Edit backend/src/auth.rs                     │
│    + use tracing::{info, warn};                 │
│    + info!("Auth attempt: {}", username);       │
│    (diff view, expandable)                      │
│                                                  │
│ ✓ Added logging at 3 key points                 │
└─────────────────────────────────────────────────┘
```

**Key patterns:**
- No message bubbles or backgrounds
- **Tool calls are first-class citizens**: displayed inline with icons
- **Thinking is collapsed** by default (not prominent)
- **Diffs are inline**: not a separate "code block"
- **Status indicators**: 🤔 (thinking), 🔧 (tool use), ✓ (done), ❌ (error)
- **Plain text assistant messages** between tool calls
- File paths are **clickable links** to artifacts

### 2. Cursor (Editor)

**Observation:**
- Agent runs in a **side panel** (not the main editor)
- Changes appear as **inline diffs** in the editor itself
- Side panel shows agent's reasoning + tool execution log
- **"Apply" button** per tool call - user must approve changes
- Multi-file changes shown as a **tree of pending edits**

**Key patterns:**
- Clear separation: **agent output (side panel)** vs **code changes (main editor)**
- Tool calls show **before/after preview**
- User has **explicit control** over what gets applied
- No chat bubbles, just flat timeline

### 3. Zed (Editor)

**Observation:**
- Similar to Cursor: agent panel + main editor
- Agent responses appear as **inline annotations** in code
- **"Assist" mode**: agent runs in background, suggests changes as you type
- Tool execution shown as **log entries** with timestamps
- Minimal chrome: no borders, no bubbles

**Key patterns:**
- **Contextual assistance**: agent tied to current file/selection
- Tool calls are **log-style**, not message-style
- Emphasis on **seamless integration** with editor workflow
- Can run **multiple agent tasks** simultaneously (shown as tabs)

### 4. VS Code GitHub Copilot Chat

**Observation:**
- Chat panel in sidebar (similar to Cursor/Zed)
- Responses are **Markdown-rendered** but without bubble backgrounds
- Code blocks have **"Insert at cursor"** / **"Copy"** buttons
- Tool calls (when using extensions) shown as **collapsible sections**
- **References** section: shows which files agent read

**Key patterns:**
- Flat layout, no visual separation between user/assistant
- **Actions** are prominent: insert, copy, apply
- File references are **interactive**: click to open
- Can **pin important messages** for context

---

## Anti-Patterns to Avoid

Based on the analysis, Sylph Web should **NOT**:

1. ❌ **Message bubbles with backgrounds**: Creates visual clutter, emphasizes chat over work
2. ❌ **Right-align user messages**: Breaks scanability, agent work is primary
3. ❌ **Hide tool calls by default**: Tool execution is the core value of agents
4. ❌ **Separate "tool result" from "assistant message"**: They're one continuous thought
5. ❌ **Heavy borders/shadows**: Clean, flat design is more professional
6. ❌ **Thinking process always visible**: It's debugging info, not primary content
7. ❌ **Code blocks as "messages"**: Code changes are workspace artifacts, not messages

---

## Sylph Web Conversation Canvas: Design Spec

### Layout Principles

1. **Flat timeline**: All events (user input, thinking, tool calls, assistant output) in one continuous, left-aligned stream
2. **Tool execution is visual anchor**: Use icons, colors, collapsible details
3. **Minimal chrome**: No backgrounds, subtle borders only for separation
4. **Interactive artifacts**: File paths → click to open in right panel
5. **Collapsible everything**: User controls density (expand for debugging, collapse for clarity)

### Event Rendering

#### User Turn (Input)

```tsx
<div className="turn-separator">
  <div className="user-prompt">
    <span className="prompt-icon">›</span>
    <span className="prompt-text">Add logging to the auth module</span>
    <span className="prompt-meta">2024-08-20 14:23:45</span>
  </div>
</div>
```

**Visual:**
```
› Add logging to the auth module                    14:23:45
```

- No background, no bubble
- Single line if short, multiline if long
- **Icon (›)** marks user input
- Timestamp on the right
- Attachments shown as **pills** below (e.g., `📎 config.toml`)

#### Thinking Process

```tsx
<details className="thinking-block" open={false}>
  <summary className="thinking-summary">
    <span className="thinking-icon">🤔</span>
    <span>Thinking...</span>
    <span className="thinking-duration">2.3s</span>
  </summary>
  <div className="thinking-content">
    <MarkdownContent>{thinkingText}</MarkdownContent>
  </div>
</details>
```

**Visual (collapsed):**
```
🤔 Thinking... (2.3s)                              [click to expand]
```

**Visual (expanded):**
```
🤔 Thinking... (2.3s)                              [click to collapse]
┌─────────────────────────────────────────────────────────────┐
│ I need to:                                                  │
│ 1. Read the existing auth module to understand structure   │
│ 2. Identify key authentication checkpoints                 │
│ 3. Add tracing calls with appropriate log levels           │
│ 4. Verify no sensitive data (passwords) are logged         │
└─────────────────────────────────────────────────────────────┘
```

- **Collapsed by default** (most users don't need reasoning details)
- Click to expand for debugging
- Duration shown (helps identify slow reasoning)
- Uses native `<details>` for accessibility

#### Tool Call: Read

```tsx
<div className="tool-call tool-read">
  <div className="tool-header">
    <span className="tool-icon">📖</span>
    <span className="tool-name">Read</span>
    <span className="tool-target">backend/src/auth.rs</span>
    <button className="tool-toggle">⌄</button>
  </div>
  <details className="tool-content" open={false}>
    <summary className="tool-summary">532 lines</summary>
    <CodeBlock language="rust" lines={fileContent} />
  </details>
</div>
```

**Visual (collapsed):**
```
📖 Read backend/src/auth.rs (532 lines)            [click to expand]
```

**Visual (expanded):**
```
📖 Read backend/src/auth.rs (532 lines)            [click to collapse]
┌─────────────────────────────────────────────────────────────┐
│ 1  use actix_web::{HttpRequest, HttpResponse};             │
│ 2  use sqlx::PgPool;                                        │
│ 3  ...                                                      │
│ 532 }                                                       │
└─────────────────────────────────────────────────────────────┘
```

- File path is **clickable** → opens in right panel Artifacts view
- **Collapsed by default** (reading is context-gathering, not output)
- Line count gives sense of scale
- Syntax highlighting when expanded

#### Tool Call: Edit/Write

```tsx
<div className="tool-call tool-edit">
  <div className="tool-header">
    <span className="tool-icon">✏️</span>
    <span className="tool-name">Edit</span>
    <span className="tool-target">backend/src/auth.rs:45-47</span>
    <span className="tool-status status-completed">✓</span>
  </div>
  <div className="tool-diff">
    <DiffView before={oldLines} after={newLines} />
  </div>
</div>
```

**Visual:**
```
✏️ Edit backend/src/auth.rs:45-47                   ✓
┌─────────────────────────────────────────────────────────────┐
│  45 │ pub async fn authenticate(                           │
│  46 │+    info!("Auth attempt: {}", req.username);         │
│  47 │     let user = db.find_user(&req.username).await?;   │
└─────────────────────────────────────────────────────────────┘
```

- **Always expanded** (edits are the primary output)
- Unified diff view (+ green, - red)
- Line numbers match source file
- Status indicator: ✓ (success), ⏳ (pending), ❌ (failed)
- File path **clickable** → opens in right panel with diff highlighted

#### Tool Call: Bash

```tsx
<div className="tool-call tool-bash">
  <div className="tool-header">
    <span className="tool-icon">$</span>
    <span className="tool-name">Bash</span>
    <span className="tool-command">cargo test auth</span>
    <span className="tool-status status-running">⏳</span>
  </div>
  <details className="tool-output" open={true}>
    <summary>Exit code: 0 (12.3s)</summary>
    <pre className="bash-output">{stdout}</pre>
  </details>
</div>
```

**Visual (running):**
```
$ Bash: cargo test auth                             ⏳
┌─────────────────────────────────────────────────────────────┐
│ running 8 tests                                             │
│ test auth::test_valid_token ... ok                         │
│ test auth::test_expired_token ... ok                       │
│ [streaming output...]                                       │
└─────────────────────────────────────────────────────────────┘
```

**Visual (completed):**
```
$ Bash: cargo test auth                             ✓ (12.3s)
Exit code: 0                                        [click to expand]
```

- **Auto-expanded while running** (user sees progress)
- **Auto-collapsed on success** (output is usually boilerplate)
- **Auto-expanded on error** (exit code ≠ 0)
- Command shown in header (helps understand what agent did)
- Streaming output updates in real-time (via SSE)

#### Assistant Text (Between Tools)

```tsx
<div className="assistant-message">
  <MarkdownContent>{messageText}</MarkdownContent>
</div>
```

**Visual:**
```
I've added logging at 3 key authentication checkpoints. The logs use the
`info!` level for successful auth and `warn!` for failures. No sensitive
data is logged - usernames are included but passwords are never touched.
```

- Plain text, **left-aligned**
- No icon, no background
- **Markdown rendered** (links, bold, inline code)
- Sits between tool calls naturally
- **No visual separation** from tool calls (they're one continuous stream)

#### Turn Separator (Between Turns)

```tsx
<div className="turn-boundary">
  <div className="turn-summary">
    <span className="turn-status">✓ Completed</span>
    <span className="turn-duration">23.4s</span>
    <span className="turn-usage">12.3K in / 4.5K out</span>
  </div>
</div>
```

**Visual:**
```
──────────────────────────────────────────────────────────────
✓ Completed (23.4s)  12.3K in / 4.5K out
──────────────────────────────────────────────────────────────
```

- Subtle horizontal rule
- Status summary (completed, failed, interrupted)
- Duration and token usage (for cost tracking)
- Separates turns without heavy visual weight

---

## Implementation: Component Architecture

### Component Hierarchy

```
<TurnTimeline>
  <UserPrompt />
  <ThinkingBlock />
  <ToolCallRead />
  <AssistantMessage />
  <ToolCallEdit />
  <AssistantMessage />
  <ToolCallBash />
  <TurnSummary />
</TurnTimeline>
```

### Key Components

#### `<TurnTimeline>`
- Container for all events in a turn
- Handles scroll anchoring (pin to bottom during streaming)
- Manages expand/collapse state (global "expand all" / "collapse all")

#### `<ThinkingBlock>`
- Native `<details>` element for accessibility
- Collapsed by default
- Markdown rendering for structured reasoning
- Duration indicator

#### `<ToolCall>`
- Polymorphic: renders different layouts based on tool type
- Status indicator (pending → running → completed/failed)
- Collapsible content (except for Edit/Write, always visible)
- Clickable file paths (opens in Artifacts panel)

#### `<AssistantMessage>`
- Simple Markdown renderer
- No visual chrome
- Inline with tool calls

#### `<TurnSummary>`
- Turn-level metadata
- Token usage for cost tracking
- Duration for performance monitoring

---

## Interaction Patterns

### 1. Streaming Updates

**During a turn:**
- New events append to timeline
- Scroll anchored to bottom (unless user scrolled up)
- Tool output streams in real-time (Bash, long file reads)
- Status indicators animate (⏳ → ✓)

**After turn completes:**
- Final turn summary appears
- Input box re-enabled
- Scroll anchors release (user can freely navigate)

### 2. Expand/Collapse

**Individual controls:**
- Each thinking block has its own toggle
- Each tool call has its own toggle
- State persists across re-renders (React state)

**Global controls:**
- Toolbar button: "Expand all" / "Collapse all"
- Keyboard: `Shift+E` (expand) / `Shift+C` (collapse)

### 3. Copy Actions

**Per-block copy:**
- Thinking block: copy reasoning text
- Tool call: copy command/file path/diff
- Assistant message: copy text
- All use native Clipboard API

**Full turn copy:**
- Button in turn summary: "Copy turn"
- Flattens all events into Markdown
- Includes tool calls but excludes thinking (too verbose)

### 4. Jump to Artifacts

**File path clicks:**
- Any file path in tool calls is a link
- Click → opens file in right panel Artifacts view
- If file is already open, highlights the relevant section (e.g., edit lines)
- If file doesn't exist (e.g., agent plans to write it), shows "not yet created"

---

## Color & Typography

### Color Palette

**Status colors:**
- 🟢 Success: `#10b981` (green-500)
- 🔴 Error: `#ef4444` (red-500)
- 🟡 Warning: `#f59e0b` (amber-500)
- 🔵 Running: `#3b82f6` (blue-500)
- ⚪ Neutral: `#6b7280` (gray-500)

**Tool icons:**
- Read: 📖 (book emoji or file-text icon)
- Edit/Write: ✏️ (pencil emoji or edit icon)
- Bash: $ (terminal icon)
- Delete: 🗑️ (trash icon)
- Other: 🔧 (wrench, generic tool)

**No backgrounds, only subtle accents:**
- Tool call header: light gray left border (`border-l-2`)
- Diff view: green/red backgrounds for add/remove lines
- Error state: red left border + light red background
- Everything else: white/transparent

### Typography

**Hierarchy:**
- User prompt: `text-base font-medium` (14px, slightly bold)
- Assistant message: `text-base font-normal` (14px, regular)
- Tool header: `text-sm font-medium` (13px, slightly bold)
- Tool content: `text-sm font-mono` (13px, monospace for code/output)
- Metadata (time, duration): `text-xs text-gray-500` (11px, muted)

**Font stacks:**
- Body text: system font stack (San Francisco on macOS, Segoe UI on Windows)
- Code: `'SF Mono', 'Cascadia Code', 'Fira Code', monospace`

---

## Accessibility

### Keyboard Navigation

**Focus order:**
- Tab through interactive elements: user prompt → thinking toggles → tool toggles → copy buttons → turn actions
- Enter/Space on `<details>` toggles expand/collapse
- Escape closes any open modal (e.g., full-screen diff view)

### Screen Reader

**Semantic HTML:**
- `<details>` / `<summary>` for collapsible sections
- `role="region"` on timeline for landmark navigation
- `aria-label` on icon-only buttons
- `aria-live="polite"` on streaming output (announces updates)

**Status announcements:**
- "Tool call started: Read file"
- "Tool call completed: Edit succeeded"
- "Turn completed in 23 seconds"

---

## Performance Considerations

### Large Turns

**Problem:** A turn with 100+ tool calls (e.g., mass file edits) renders slowly.

**Solution:**
- **Virtualize timeline**: Only render visible events (use `react-window`)
- **Lazy expand**: Don't render collapsed content until user expands
- **Memoize components**: `React.memo` on `<ToolCall>`, `<AssistantMessage>`
- **Debounce streaming**: Batch SSE events every 100ms (avoid re-render on every byte)

### Long File Diffs

**Problem:** A 10,000-line diff freezes the browser.

**Solution:**
- **Truncate by default**: Show first 100 lines, "Show more" button
- **Syntax highlighting on demand**: Only when expanded
- **Unified diff format**: More compact than side-by-side

---

## Comparison: Before & After

### Before (Chat-style, Phase 1-5)

```
┌────────────────────────────────────────┐
│  You                          14:23:45 │
│  ┌──────────────────────────────────┐ │
│  │ Add logging to auth module       │ │
│  └──────────────────────────────────┘ │
└────────────────────────────────────────┘

┌────────────────────────────────────────┐
│  Claude                       14:23:48 │
│  ┌──────────────────────────────────┐ │
│  │ I'll add logging...              │ │
│  │                                  │ │
│  │ [Tool: Read auth.rs]             │ │
│  │ [Tool: Edit auth.rs]             │ │
│  │                                  │ │
│  │ Done! Added 3 log statements.    │ │
│  └──────────────────────────────────┘ │
└────────────────────────────────────────┘
```

**Problems:**
- ❌ Bubbles waste space
- ❌ Tool calls hidden/minimized
- ❌ Hard to scan (alternating alignment)
- ❌ Looks like chat, not work

### After (Agent-style, Phase 6)

```
› Add logging to auth module                     14:23:45

🤔 Thinking... (2.1s)                   [click to expand]

📖 Read backend/src/auth.rs (532 lines) [click to expand]

I'll add logging at key authentication checkpoints.

✏️ Edit backend/src/auth.rs:45-47                ✓
┌──────────────────────────────────────────────────┐
│  45 │ pub async fn authenticate(                │
│  46 │+    info!("Auth attempt: {}", username);  │
│  47 │     let user = db.find_user(...).await?;  │
└──────────────────────────────────────────────────┘

$ Bash: cargo test auth                           ✓ (12.3s)
Exit code: 0                            [click to expand]

Added 3 log statements at authentication checkpoints.

────────────────────────────────────────────────────────
✓ Completed (23.4s)  12.3K in / 4.5K out
────────────────────────────────────────────────────────
```

**Improvements:**
- ✅ Flat, scannable layout
- ✅ Tool calls are visual anchors
- ✅ Diffs inline, immediately visible
- ✅ Collapsible details (thinking, tool output)
- ✅ Looks like a work log, not a chat

---

## What Shipped

This document is the argument, written before the work. The design landed in Phase 6
and `docs/frontend-architecture.md` describes what the code actually does; where the
two differ, that file wins.

The six steps this section originally listed are all done. Three of their targets
turned out to be worth keeping as measurements:

- **1000-event turn renders in under 2s** — holds, through row memoization on event
  identity rather than virtualization. Confirmed on a cold reload of durable history
  in `e2e/timeline.spec.ts`.
- **Streaming does not jank** — but the first implementation did not stream at all:
  it batched the whole turn to the end, and every test passed anyway because each
  asserted final state against a runner faster than one render. Two causes, both in
  `AGENTS.md` Pitfalls. Asserting only the end state cannot distinguish streaming
  from batching.
- **Accessibility** — axe-core reports zero WCAG 2.1 AA violations across the canvas
  and all overlays, and caught two real defects during the work. Screen-reader passes
  (VoiceOver, NVDA) and forced-colours mode remain untested, so that step is done
  only in its automated half.

One thing this document argued for that the code deliberately does not have: icons
per tool. `components/timeline.tsx` uses text labels and a status mark, because an
icon set is a dependency and the tool name is already the clearest label available.
