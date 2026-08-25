# Keyboard Shortcuts, Default Sessions & Settings Design

> **Status: the argument, written before the work.** This is not a specification and
> is not maintained against the code. It records the workflow reasoning behind
> shortcuts, Quick Start, and Settings, which is worth keeping and is not recoverable
> from the implementation. Authoritative sources: `docs/frontend-architecture.md` for
> the shortcut map and UI structure, `docs/configuration.md` for paths and settings.
> Where they differ from this, they win. Two corrections are applied inline rather
> than deferred to the end — the reserved-combo bindings in §1 and the Quick Start
> path note in §2 — because a correction 500 lines after the claim is read too late.
> "What Shipped, and What Did Not" lists the rest.

This document specifies the enhanced UX features for Sylph Web based on user workflow analysis
and comparison with similar agent tools.

---

## 1. Keyboard Shortcuts Redesign

### Problem with Original Design

**Original:**
- `Cmd/Ctrl+K`: Global search (sessions/history)
- `Cmd/Ctrl+P`: Duplicate function (also global search)

**Issue:** Both shortcuts do the same thing, and `Cmd+P` is conventionally a "command palette"
in modern apps (VS Code, Linear, Raycast), not just search.

### New Design

#### `Cmd/Ctrl+K` — Global Search

**Purpose:** Search across all sessions (live + archive) to quickly jump to a conversation.

**Behavior:**
- Opens centered modal dialog
- Search input auto-focused
- Fuzzy search against:
  - Session workdir
  - Agent name
  - Session content (via TraceDB full-text search)
- Results show:
  - Session workspace/agent
  - Last updated time
  - Snippet of last message
- Up/Down to navigate, Enter to open, Esc to close

**UI:**
```
┌─────────────────────────────────────────────────────┐
│  🔍  Search sessions...                             │
├─────────────────────────────────────────────────────┤
│  > sylph-web / claude / 2h ago                      │
│    "Add logging to auth module..."                  │
│                                                     │
│    flashcards / codex / 3d ago                      │
│    "Implement spaced repetition algorithm..."       │
│                                                     │
│    trace-db / claude / 1w ago                       │
│    "Design ingestion watermark tracking..."         │
└─────────────────────────────────────────────────────┘
```

#### `Cmd/Ctrl+P` — Command Palette

**Purpose:** Quick access to common actions and settings without mouse navigation.

**Behavior:**
- Opens centered modal dialog
- Command input auto-focused
- Fuzzy search against available commands
- Commands grouped by category
- Recently used commands appear first
- Up/Down to navigate, Enter to execute, Esc to close

**Available Commands:**

| Command | Action | Shortcut Hint |
|---------|--------|---------------|
| **Session Management** | | |
| New Session | Opens new session dialog | `Mod+Shift+O` |
| New Session in Current Space | Creates session in active workspace | `Mod+J` |
| Close Session | Closes current session (keeps in history) | — (`Cmd+W` closes the tab) |
| Duplicate Session | Forks current session (future) | - |
| **Navigation** | | |
| Search Sessions | Opens global search | `Cmd+K` |
| Go to Settings | Opens settings panel | `Cmd+,` |
| Toggle Left Sidebar | Show/hide workspaces | `Cmd+B` |
| Toggle Right Sidebar | Show/hide artifacts | `Cmd+\` |
| **Workspace (Space) Management** | | |
| New Workspace | Creates new workspace folder | - |
| Rename Workspace | Renames current workspace | - |
| Delete Workspace | Removes workspace (keeps sessions) | - |
| **Agent Controls** | | |
| Stop Current Turn | Cancels running turn | `Cmd+.` |
| Retry Last Turn | Re-runs last failed turn | - |
| Change Model | Opens model selector | - |
| **View Options** | | |
| Expand All Details | Expands all thinking/tool blocks | `Shift+E` |
| Collapse All Details | Collapses all blocks | `Shift+C` |
| Toggle Thinking Visibility | Show/hide all thinking blocks | - |
| **Settings** | | |
| Set Default Agent | Configure default agent | - |
| Set Default Model | Configure default model | - |
| Manage Agents | View/refresh available agents | - |
| Preferences | General settings | `Cmd+,` |

**UI:**
```
┌─────────────────────────────────────────────────────┐
│  ⌘  Command Palette                                 │
├─────────────────────────────────────────────────────┤
│  > new session                                      │
│                                                     │
│  🎯 Session Management                              │
│    New Session                      Shift+Cmd+O     │
│    New Session in Current Space           Cmd+J     │
│                                                     │
│  🧭 Navigation                                      │
│    Search Sessions                          Cmd+K   │
│    Go to Settings                           Cmd+,   │
│    Toggle Left Sidebar                      Cmd+B   │
│                                                     │
│  📊 Recently Used                                   │
│    Stop Current Turn                        Cmd+.   │
│    Set Default Model                                │
└─────────────────────────────────────────────────────┘
```

#### Full Keyboard Shortcut Map

| Shortcut | Action | Scope |
|----------|--------|-------|
| `Cmd/Ctrl+K` | Global search | Global |
| `Cmd/Ctrl+P` | Command palette | Global |
| `Shift+Cmd/Ctrl+O` | New session | Global |
| `Cmd/Ctrl+J` | New session in current space (Quick Start) | Global |
| `Shift+Cmd/Ctrl+K` | Browse workspace history | Global |
| `Cmd/Ctrl+B` | Toggle left sidebar | Global |
| `Cmd/Ctrl+\` | Toggle right sidebar | Global |
| `Cmd/Ctrl+,` | Open settings | Global |
| `Cmd/Ctrl+.` | Stop current turn | In session view, while turn running |
| `Shift+E` | Expand all collapsible blocks | In session view |
| `Shift+C` | Collapse all blocks | In session view |
| `Cmd/Ctrl+F` | Find in current session | In session view |
| `Esc` | Close modal/dialog | Global |
| `Enter` | Submit message / Confirm action | Context-dependent |
| `Shift+Enter` | New line in input | In message input |

**Why not `Cmd+N`, `Cmd+Shift+N`, or `Cmd+W`.** This document originally proposed
all three, and none of them is reachable: Chrome has reserved `Ctrl/Cmd+N` (new
window), `+T` (new tab), `+W` (close tab) and their shifted forms since version 4,
above the page's own handlers and beyond the reach of `preventDefault`. A binding
the browser answers first is not a binding that can be fixed later — so New Session
is `Mod+Shift+O`, Quick Start is `Mod+J`, and closing a session has no accelerator
because the one users would reach for closes their tab instead. The recorder in
§3.5 refuses any reserved combo and names what the browser does with it, rather
than accepting a binding that would silently never fire. `lib/combo.ts` holds the
reserved set; `docs/frontend-architecture.md` holds the current map.

Every binding here is rebindable, so treat the table as defaults rather than as
the key that will be in effect. Surfaces that teach a shortcut read the live
binding through `useShortcutHint`.

---

## 2. Default Session & the Quick Start Workspace

> The path named throughout this section, `~/.agents`, was superseded in Phase 12
> by `paths.quick_start_dir` (`~/.sylph/scratch` by default). See "What Shipped,
> and What Did Not".

### Problem

**Current design:** Every session requires selecting a workspace directory upfront.

**Issue:** High barrier to entry. Users who just want to "ask the agent something" don't want
to think about directories, spaces, or file organization. They want to start chatting immediately.

### Solution: Default Session

**Concept:** Sessions without an explicit workspace are created in a **default workspace** at
`~/.agents` (the global agent data directory).

#### Behavior

1. **"Quick Start" button** in top nav: clicking creates a new session instantly in `~/.agents`
2. **No workspace selection dialog** for default sessions
3. **Default sessions don't appear in the Spaces list** (left sidebar)
   - They're accessible via global search (`Cmd+K`)
   - They appear in a special **"Quick Sessions"** section (separate from workspaces)
4. **User can "promote" a default session** to a proper workspace later:
   - Right-click session → "Move to Workspace..."
   - Copies/moves session to selected workspace
   - Session now appears in that Space

#### UI Changes

**Top Navigation Bar:**
```
┌─────────────────────────────────────────────────────┐
│  Sylph Web             [⚡ Quick Start]  [+ New...] │
└─────────────────────────────────────────────────────┘
```

- **⚡ Quick Start**: Creates default session in `~/.agents` (no dialog)
- **+ New...**: Opens full session creation dialog (select workspace, agent, model)

**Left Sidebar:**
```
┌───────────────────┐
│ ⚡ Quick Sessions  │  ← New section
│   Session A        │
│   Session B        │
│                   │
│ 🗂️ Workspaces      │
│   sylph-web       │
│     Session C     │
│     Session D     │
│   flashcards      │
│     Session E     │
└───────────────────┘
```

- **Quick Sessions**: Collapsible section for default `~/.agents` sessions
- Sessions here don't have an explicit workspace folder
- When user navigates to one, right panel shows `~/.agents` contents (agent config, skills, data)

#### Backend Changes

**Session Model:**
```rust
struct Session {
    id: Uuid,
    workdir: String,  // For default sessions: "~/.agents"
    is_default: bool,  // New field: true for ~/.agents sessions
    // ... rest unchanged
}
```

**API Changes:**
- `POST /api/v1/sessions` accepts `is_default: true` to skip workdir selection
- `PATCH /api/v1/sessions/{id}` accepts `workdir` to move session to new workspace

**File System:**
- Default sessions' attachments go to `~/.agents/data/sylph/web/attachments/{session_id}/`
- No special handling needed; workdir is just `~/.agents`

---

## 3. Settings Panel Design

### Access

- **Keyboard:** `Cmd/Ctrl+,`
- **Command Palette:** "Preferences" or "Go to Settings"
- **Top Nav:** Gear icon (⚙️) in top-right corner

### Layout

**Full-page modal** (like VS Code settings):
```
┌─────────────────────────────────────────────────────────────┐
│  Settings                                           [✕ Close] │
├──────────────┬──────────────────────────────────────────────┤
│              │                                              │
│  General     │  🎨 Appearance                               │
│  Agents      │  ○ Light                                     │
│  Models      │  ● Dark                                      │
│  Workspaces  │  ○ System                                    │
│  Shortcuts   │                                              │
│  Advanced    │  🌐 Language                                 │
│              │  [English ▼]                                 │
│              │                                              │
│              │  📏 Density                                  │
│              │  ○ Compact  ● Normal  ○ Comfortable         │
│              │                                              │
│              │  ──────────────────────────────────────────  │
│              │                                              │
│              │  💾 Data Directory                           │
│              │  ~/.agents/data/sylph/web                    │
│              │  [Change...]                                 │
└──────────────┴──────────────────────────────────────────────┘
```

### Settings Sections

#### 1. General

**Appearance:**
- Theme: Light / Dark / System
- Density: Compact / Normal / Comfortable (affects spacing, font size)
- Font size: 12px / 14px / 16px
- Code font: SF Mono / Cascadia Code / Fira Code / JetBrains Mono

**Language:**
- UI language: English / 中文 (future)

**Data:**
- Data directory path (read-only, show current location)
- Cache size: Show current size, "Clear cache" button

**Privacy:**
- Send anonymous usage data: Yes / No (future, not impl yet)

#### 2. Agents

**Default Agent:**
```
Default agent for new sessions:
[claude ▼]

Available agents:
┌─────────┬─────────┬────────────────────────────────┐
│ Name    │ Version │ Status                         │
├─────────┼─────────┼────────────────────────────────┤
│ claude  │ 3.1.0   │ ✓ Ready (4 models available)   │
│ codex   │ 2.5.1   │ ✓ Ready (2 models available)   │
│ pi      │ 1.0.0   │ ⚠️ Not found                    │
└─────────┴─────────┴────────────────────────────────┘

[Refresh Agents]  [Add Agent...]
```

**Behavior:**
- **Refresh Agents**: Re-runs runner discovery (`--describe`)
- **Add Agent**: Opens dialog to manually add a runner script path
- **Default agent**: Used when creating quick sessions or when not specified

#### 3. Models

**Default Model:**
```
Default model for new sessions:
[claude-opus-5 ▼]

Model configuration per agent:
┌─────────┬────────────────┬─────────────────────────────┐
│ Agent   │ Available Models                           │
├─────────┼───────────────────────────────────────────┤
│ claude  │ ● opus-5                                  │
│         │ ○ sonnet-5                                │
│         │ ○ haiku-5                                 │
├─────────┼───────────────────────────────────────────┤
│ codex   │ ● deepseek-v3                             │
│         │ ○ deepseek-v2.5                           │
└─────────┴───────────────────────────────────────────┘

Effort level:
○ Low  ○ Medium  ● High  ○ X-High  ○ Max
```

**Behavior:**
- Shows all models available per agent (from `--describe`)
- Default model selection per agent
- Default effort level (applies to all new turns unless overridden)

#### 4. Workspaces

**Default Workspace:**
```
Quick Start sessions created in:
~/.sylph/scratch  [Read-only here; set per install]

Recent workspaces:
┌────────────────────────────┬──────────┬─────────────┐
│ Path                       │ Sessions │ Last Used   │
├────────────────────────────┼──────────┼─────────────┤
│ ~/Workspace/sylph-web      │ 8        │ 2h ago      │
│ ~/Projects/flashcards      │ 3        │ 1d ago      │
│ ~/Work/huawei-docs         │ 12       │ 3d ago      │
└────────────────────────────┴──────────┴─────────────┘

[Add Workspace...]  [Clean Up...]
```

**Behavior:**
- **Add Workspace**: Manually register a frequently-used workspace (appears in left sidebar)
- **Clean Up**: Remove workspaces with no sessions

**Workspace Management:**
- Workspaces are auto-discovered from sessions
- User can pin frequently-used workspaces to keep them visible even with 0 sessions
- User can set custom names for workspaces (overrides directory name)

#### 5. Shortcuts

**Keyboard Shortcuts:**
```
Customize keyboard shortcuts:

┌──────────────────────────────┬──────────────┬──────────┐
│ Action                       │ Shortcut     │          │
├──────────────────────────────┼──────────────┼──────────┤
│ Global Search                │ Cmd+K        │ [Edit]   │
│ Command Palette              │ Cmd+P        │ [Edit]   │
│ New Session                  │ Shift+Cmd+O  │ [Edit]   │
│ Toggle Left Sidebar          │ Cmd+B        │ [Edit]   │
│ Toggle Right Sidebar         │ Cmd+\        │ [Edit]   │
│ Stop Current Turn            │ Cmd+.        │ [Edit]   │
│ ...                          │              │          │
└──────────────────────────────┴──────────────┴──────────┘

[Reset to Defaults]
```

**Behavior:**
- Click **[Edit]** to record new shortcut (press keys, auto-saves)
- Conflict detection: warns if shortcut already used
- **Reset to Defaults** restores all shortcuts

#### 6. Advanced

**Performance:**
```
Performance & Caching:

☑ Enable virtual scrolling for large conversations
☑ Lazy-load code syntax highlighting
☑ Cache file previews (reduces disk I/O)
☐ Disable animations (improves performance on slow machines)

Max events per session before virtualization:
[1000]

Max file size for inline preview:
[1] MB
```

**Developer:**
```
Developer Options:

☑ Show debug logs in browser console
☐ Enable React DevTools integration
☐ Preserve agent process on stop (for debugging)

Backend API endpoint:
http://localhost:4317  [Default]

☑ Auto-reconnect on backend restart
```

**Danger Zone:**
```
🔴 Danger Zone

[Delete All Quick Sessions]
Permanently removes all sessions in ~/.agents

[Reset All Settings]
Restores all settings to defaults

[Clear All Data]
Deletes journal.db, all sessions, all attachments
(TraceDB archive is unaffected)
```

---

## 4. Right-Click Context Menus

### Artifacts Panel: File Context Menu

**When right-clicking a file in the file tree:**

```
┌──────────────────────────────┐
│ Open in Default App          │  ← System file handler
│ Open in Artifacts View       │  ← Show preview in right panel
│ Reveal in Finder/Explorer    │  ← Open enclosing folder
├──────────────────────────────┤
│ Copy Path                    │
│ Copy Relative Path           │
├──────────────────────────────┤
│ Rename...                    │  (Future)
│ Delete...                    │  (Future, with confirmation)
└──────────────────────────────┘
```

**"Open in Default App":**
- **macOS:** `open <file>`
- **Windows:** `start <file>`
- **Linux:** `xdg-open <file>`
- Uses system file associations (e.g., `.md` → Obsidian, `.rs` → VS Code)

**"Reveal in Finder/Explorer":**
- Opens file manager with file selected
- **macOS:** `open -R <file>`
- **Windows:** `explorer /select,<file>`
- **Linux:** `xdg-open $(dirname <file>)`

### Session List: Session Context Menu

**When right-clicking a session in the left sidebar:**

```
┌──────────────────────────────┐
│ Open                         │
│ Open in New Window           │  (Future: multi-window support)
├──────────────────────────────┤
│ Rename...                    │
│ Move to Workspace...         │  ← For quick sessions
│ Duplicate (Fork)             │  (Future)
├──────────────────────────────┤
│ Export as Markdown...        │  (Future)
│ Export as PDF...             │  (Future)
├──────────────────────────────┤
│ Delete...                    │  (With confirmation)
└──────────────────────────────┘
```

### Workspace: Context Menu

**When right-clicking a workspace in the left sidebar:**

```
┌──────────────────────────────┐
│ New Session in This Space    │
│ Collapse All Sessions        │
│ Expand All Sessions          │
├──────────────────────────────┤
│ Rename Workspace...          │
│ Pin to Top                   │
├──────────────────────────────┤
│ Remove Workspace             │  ← Removes from sidebar, keeps sessions
└──────────────────────────────┘
```

---

## 5. Implementation Notes

### Keyboard Shortcut System

The sketch this section originally carried described a `lib/use-keyboard-shortcuts.ts`
dispatching through Redux or Zustand. Neither exists. What shipped:

- **`lib/shortcuts.ts`** — the grammar, and pure enough to unit-test:
  `comboFromEvent` normalizes a `KeyboardEvent` into a `Combo`, `formatCombo` renders
  one for display, `isTypingTarget` suppresses a binding inside an input, and
  `useShortcutHint` returns the binding *currently* in effect for an action.
- **`components/app-shell.tsx`** — one document-level `keydown` listener for the whole
  app, dispatching through `runAction`. Not one listener per component: two components
  both claiming `Cmd+K` is a bug that only appears when both are mounted.
- **`lib/settings.tsx`** — bindings over `localStorage` through
  `useSyncExternalStore`, so a rebind reaches every surface without a state library.

Bindings are stored platform-neutral (`Mod` rather than `Cmd` or `Ctrl`) and resolved
at render, which is what makes the same stored binding correct on macOS and Linux.
`useShortcutHint` exists because a surface that *teaches* a shortcut has to read the
binding in effect — a hardcoded "⌘N" is wrong the moment the user rebinds it, and the
start screen teaches several.

**Settings Storage:**
- Stored in `localStorage` initially (Phase 6.1-6.4)
- Migrated to backend database (Phase 6.5+, for cross-device sync)

### Default Session Detection

**Frontend:**
```typescript
function isDefaultSession(session: Session): boolean {
  return session.workdir === '~/.agents' || session.is_default;
}

function groupSessions(sessions: Session[]) {
  const quick = sessions.filter(isDefaultSession);
  const workspaces = sessions.filter(s => !isDefaultSession(s));
  return { quick, workspaces };
}
```

**Backend:**
```rust
impl Session {
    pub fn is_default(&self) -> bool {
        self.workdir == "~/.agents" || self.is_default
    }
}
```

### Context Menu Implementation

**Use Radix UI's ContextMenu:**
```tsx
import * as ContextMenu from '@radix-ui/react-context-menu';

<ContextMenu.Root>
  <ContextMenu.Trigger>
    <FileTreeItem file={file} />
  </ContextMenu.Trigger>
  <ContextMenu.Portal>
    <ContextMenu.Content>
      <ContextMenu.Item onSelect={() => openInDefaultApp(file.path)}>
        Open in Default App
      </ContextMenu.Item>
      <ContextMenu.Item onSelect={() => revealInFinder(file.path)}>
        Reveal in Finder
      </ContextMenu.Item>
      {/* ... */}
    </ContextMenu.Content>
  </ContextMenu.Portal>
</ContextMenu.Root>
```

**System Commands:**
```typescript
// lib/system-commands.ts
export async function openInDefaultApp(path: string) {
  await api.system.openFile({ path, mode: 'default' });
}

export async function revealInFinder(path: string) {
  await api.system.revealFile({ path });
}
```

**Backend API:**
```rust
// POST /api/v1/system/open-file
struct OpenFileRequest {
    path: String,
    mode: OpenMode,  // "default" | "reveal"
}

async fn open_file(req: OpenFileRequest) -> Result<(), ApiError> {
    let validated = validate_path(&req.path)?;
    
    match req.mode {
        OpenMode::Default => {
            #[cfg(target_os = "macos")]
            Command::new("open").arg(validated).spawn()?;
            
            #[cfg(target_os = "windows")]
            Command::new("cmd").args(["/C", "start", "", validated]).spawn()?;
            
            #[cfg(target_os = "linux")]
            Command::new("xdg-open").arg(validated).spawn()?;
        }
        OpenMode::Reveal => {
            #[cfg(target_os = "macos")]
            Command::new("open").args(["-R", validated]).spawn()?;
            
            // Windows/Linux similar
        }
    }
    Ok(())
}
```

---

## Summary

This design addresses:

1. **Keyboard shortcuts rethink:**
   - `Cmd+K` = Global search (sessions/archive)
   - `Cmd+P` = Command palette (actions, settings, navigation)
   - Full shortcut map for power users

2. **Default sessions:**
   - Quick Start button creates session in `~/.agents` instantly
   - No upfront workspace selection (lowers barrier to entry)
   - Default sessions appear in separate "Quick Sessions" section
   - Can be promoted to proper workspaces later

3. **Settings panel:**
   - Comprehensive UI for agent/model configuration
   - Default agent and model selection
   - Workspace management
   - Customizable keyboard shortcuts
   - Performance tuning

4. **Right-click menus:**
   - "Open in Default App" (not hardcoded VS Code)
   - System-aware file operations
   - Session and workspace management actions

These features transform Sylph Web from a basic session viewer into a **full-featured agent workspace**.

## What Shipped, and What Did Not

All four areas landed in Phase 6. Four differences are worth knowing before treating
this document as a description of the code:

- **The Quick Start button is gone.** Phase 7 removed it from the top bar along with
  "New session", because creating a session in a directory the user already clicked
  should not route through a button that re-asks. The behaviour survives in
  `Mod+Shift+O` / `Mod+J`, the command palette, and every sidebar group's context menu — all
  through one shared creator (`lib/create-session.ts`). The "Quick Sessions" grouping
  is unchanged in shape.
- **The Quick Start directory is no longer `~/.agents`, and is no longer a fixed
  path.** Section 2 and the summary both name `~/.agents`, because that is where the
  global agent data lived when this was written. Phase 12 gave Sylph its own root, so the
  directory is now `paths.quick_start_dir` — `~/.sylph/scratch` by default, and
  configurable per install. Read every `~/.agents` in this document as "the
  configured scratch directory" — including the one in the summary above. Nothing in the UI derives or displays the path: it
  comes back from `GET /config` as `quickStartDir`, because a client that derived it
  would disagree with a configured one. `docs/configuration.md` is authoritative.
- **"Open in Default App" and "Reveal in Finder" are not implemented.** Both need the
  backend to spawn a process against a client-supplied path, which is a local
  code-execution surface and wants a threat-model decision rather than a menu entry.
  The rest of section 4's menus exist.
- **`Cmd+F` filters turns rather than highlighting matches inside them.** Highlighting
  would mean reaching into the Markdown, diff, and code renderers to rewrite their
  text nodes.
- **Settings ships without the performance-tuning controls.** Density, expansion
  defaults, default agent and per-agent model, the shortcut recorder, and workspace
  management are all there. Bounded rendering is an architecture property, not a
  preference to expose — see `docs/frontend-architecture.md`.

- **The reserved-combo bindings are corrected in place.** This document originally
  proposed `Cmd+N`, `Shift+Cmd+N`, and `Cmd+W`, which a browser answers before the
  page sees them. The tables in §1 now carry the shipped defaults and the reasoning
  sits beside the map rather than here, because a reader who reaches §1 first would
  otherwise try the wrong keys for 500 lines before finding the correction.

The rest of the shortcut map is current.
