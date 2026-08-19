# Phase 3 Handoff: Feature Completion Roadmap

**Current State:** Phase 2 complete (commit `4f815d5`)  
**Backend:** Production-ready, 32 tests passing  
**Frontend:** Session list, create dialog, SSE conversation view working  
**Next:** Attachment upload, TraceDB search, native import, filters

## Phase 2 Achievements

✅ Next.js 16 App Router + React 19 + TanStack Query  
✅ Session list with real-time status  
✅ Create session dialog (agent selection, workdir input)  
✅ Conversation view with SSE streaming  
✅ Auto-reconnection from `after_seq` on disconnect  
✅ Event rendering (thinking/tool_call/assistant/status/error)  
✅ API proxy (`/api/v1/*` → backend)  
✅ Cookie-based auth (no explicit headers needed)

Test the full stack:
```sh
# Terminal 1: Backend
cd backend && cargo run --release

# Terminal 2: Frontend
cd frontend && npm run dev

# Visit http://localhost:3000
# Create a session → Send a message → Watch SSE events stream
```

## Phase 3 Deliverables

### 1. Attachment Upload (2-3 days)

**Backend ready:**
- `POST /api/v1/sessions/{id}/turns` already accepts `attachments` array
- Each attachment: `{name, mimeType, data}` (data is base64)
- Size limit: 10 MiB total per turn
- Validation: `attachments::stage()` checks base64, enforces cap

**Frontend work:**
- Drag-and-drop zone in message composer
- File picker button (`<input type="file" multiple accept="image/*,text/*,.pdf">`)
- Preview thumbnails for images (data: URL inline)
- Base64 encoding before send (`FileReader.readAsDataURL`)
- Progress indicator during upload
- Error handling: oversized files, invalid MIME types

**Reference DSH:**
- `apps/web/src/components/AttachmentUpload.tsx` (if exists)
- Look for `FileReader`, `onChange`, drag events

**Implementation notes:**
- Image preview uses the same data: URL you'll send to the backend
- PDF/text files show filename + size, no inline preview
- 10 MiB cap is across ALL attachments in one turn, not per-file
- Backend returns 413 if total exceeds limit (catch and surface)

**Testing:**
- Upload 1 small image → verify it appears in the attachment array sent
- Upload 5 images totaling >10 MiB → expect 413 error
- Drag-and-drop 3 files → all should appear in preview list
- Remove one attachment → verify it's excluded from the request

---

### 2. Agent Command Dialogs (1-2 days)

**Backend ready:**
- `GET /api/v1/agents` returns agent list with `flags` and `commands`
- `flags.model.choices` = available model options for that agent
- Turn request accepts `model: {flag_name: value}` object

**Frontend work:**
- Model selector in create-turn form (dropdown or radio buttons)
- Effort selector if the agent exposes an `effort` flag
- Command palette (optional: advanced UX, defer to post-Phase-3)
- Store selection in local state, send in `model` field

**Example:**
```tsx
const [model, setModel] = useState<Record<string, string>>({});
// Render <select> for each flag in agent.flags
// On submit: api.turns.create(sessionId, { model, message, attachments })
```

**Agent flags from backend:**
- `claude` agent: `model: {choices: ["fast", "slow"], description: "..."}`
- Your frontend renders a dropdown, user picks "fast"
- Turn request: `{model: {model: "fast"}, message: {...}}`

**Testing:**
- Select "slow" model → backend spawns runner with `SYLPH_MODEL=slow`
- No flags exposed → model selector hidden
- Multiple flags → both selectors appear

---

### 3. TraceDB Search Integration (2-3 days)

**Backend gaps:**
- TraceDB client exists (`src/tracedb/mod.rs`), but no API endpoint exposes it
- Need new route: `GET /api/v1/search?q={query}`
- Calls `tracedb.search(query)`, returns session list or turn list
- If TraceDB unavailable, return 503 or empty results

**Backend work (Rust):**
1. Add `search()` method to `TraceDbClient`:
   ```rust
   pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
       self.request(json!({"method": "search", "query": query})).await
   }
   ```
2. Add route in `src/api/mod.rs`:
   ```rust
   pub mod search; // new module
   .route("/api/v1/search", get(search::search))
   ```
3. Handler:
   ```rust
   pub async fn search(
       State(state): State<AppState>,
       Query(params): Query<SearchQuery>,
   ) -> Result<Json<SearchResponse>> {
       let results = state.tracedb.search(&params.q).await?;
       Ok(Json(SearchResponse { results }))
   }
   ```

**Frontend work:**
- Search bar in header (magnifying glass icon + input)
- Results page `/search?q={query}` or modal overlay
- Display: session title, agent, snippet, link to `/sessions/{id}`
- Empty state: "No results for '{query}'"
- Error state: "TraceDB unavailable" (503 or client-side check)

**Testing:**
- Backend without TraceDB → search returns 503 or []
- Backend with TraceDB → search returns actual sessions
- Frontend handles both gracefully

---

### 4. Native Session Import Flow (1-2 days)

**Backend ready:**
- `POST /api/v1/sessions` accepts `origin: "native"` (default is "sylph")
- Native sessions preserve their originating agent's identity
- Lifecycle policy: no SOUL.md injection, no prompt rewriting

**Frontend work:**
- "Import Native Session" button on session list page
- Dialog with TraceDB session ID input (or search picker from #3)
- Call `api.sessions.create({agent, workdir, title, origin: "native"})`
- Visual indicator: native sessions show violet badge (vs green for Sylph)

**Design note:**
- Native sessions are read-only until explicitly converted to Sylph
- Resuming a native session requires a confirmation dialog:
  > "This session was created by {agent}. Resuming will preserve its
  > identity and lifecycle policy. Convert to Sylph session?"
  - Yes → update `origin: "sylph"` via new `PATCH /api/v1/sessions/{id}` endpoint
  - No → proceed as-is

**Testing:**
- Create native session → verify violet badge, no SOUL.md in turns
- Attempt to resume → confirmation dialog appears
- Convert to Sylph → badge changes to green

---

### 5. Session Filters (1 day)

**Frontend only:**
- Filter bar above session list: agent dropdown, workspace input, date range
- Apply filters client-side (sessions are already loaded from `GET /api/v1/sessions`)
- Optional: backend pagination if session count grows (defer to Phase 4)

**Implementation:**
```tsx
const [filters, setFilters] = useState({
  agent: "",
  workdir: "",
  dateFrom: "",
  dateTo: "",
});

const filtered = sessions.filter(s => {
  if (filters.agent && s.agent !== filters.agent) return false;
  if (filters.workdir && !s.workdir.includes(filters.workdir)) return false;
  // date range comparison...
  return true;
});
```

**Testing:**
- Filter by agent → only matching sessions shown
- Filter by workdir substring → partial matches included
- Clear filters → all sessions return

---

### 6. Error Boundaries and Retry Logic (1 day)

**Frontend work:**
- React Error Boundary wrapping `<Providers>{children}</Providers>`
- Catch-all fallback: "Something went wrong. [Reload]"
- Per-component error boundaries for session list, conversation view
- Retry button on failed queries (TanStack Query's `refetch()`)

**TanStack Query retry config:**
```tsx
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 5000,
    },
  },
});
```

**Error Boundary example:**
```tsx
class ErrorBoundary extends React.Component {
  state = { hasError: false };
  static getDerivedStateFromError() { return { hasError: true }; }
  render() {
    if (this.state.hasError) {
      return <div>Error. <button onClick={() => location.reload()}>Reload</button></div>;
    }
    return this.props.children;
  }
}
```

**Testing:**
- Kill backend mid-request → error boundary catches
- SSE disconnect → "Reconnecting..." shown, auto-reconnects
- 500 error from backend → error message surfaces

---

## Phase 3 Exit Criteria (from roadmap)

✅ Feature parity with specs  
✅ Native session import preserves identity  
✅ Search works across live + TraceDB sessions  

Estimated effort: **2 weeks** (10-12 full-time days)

---

## Phase 4 Preview: Production Readiness (Weeks 8-9)

After Phase 3 features are complete:

1. **Test Coverage**
   - Unit tests: Rust backend already >80% (32 tests passing)
   - Frontend tests: Vitest + React Testing Library
   - E2E tests: Playwright covering session create → message → SSE flow

2. **Accessibility Audit**
   - WCAG 2.1 AA compliance
   - Keyboard navigation (Tab, Enter, Esc)
   - Screen reader announcements for SSE events
   - Color contrast checks (all status badges)

3. **Performance Optimization**
   - Bundle size <200 KB (check with `npm run build && ls -lh .next/static`)
   - LCP <2.5s (Lighthouse audit)
   - Virtual scrolling for large turn lists (react-window)

4. **Documentation**
   - User guide: installation, first session, troubleshooting
   - API docs: update OpenAPI spec if new endpoints added
   - Developer guide: running tests, adding features

---

## Phase 5 Preview: Deployment (Week 10)

1. **Binary Packaging**
   - `cargo build --release` produces `sylph-web-backend`
   - `npm run build` produces `.next/` static export (optional)
   - Single archive: `sylph-web-v1.0.0.tar.gz`

2. **Installation Script**
   - Copy binary to `/usr/local/bin/`
   - Create data directory (`~/.sylph-web/`)
   - Generate cookie secret
   - LaunchAgent plist for macOS (auto-start on boot)

3. **Health Checks**
   - Systemd unit (Linux) or LaunchAgent (macOS)
   - Restart on crash
   - Log rotation

4. **Migration**
   - Schema version in `app_meta` table
   - Migration runner for database changes
   - Backward compatibility for one major version

---

## Immediate Next Steps (Start Phase 3)

1. **Attachment upload** — highest user value, well-scoped
   - Add drag-drop zone to message composer
   - FileReader → base64 → include in turn request
   - Preview thumbnails for images

2. **Agent command dialogs** — unblocks model selection
   - Fetch agent list on session create page
   - Render flag selectors dynamically
   - Include `model` object in turn request

3. **Error boundaries** — foundational UX polish
   - Wrap root layout in ErrorBoundary
   - Per-page boundaries for session list, conversation
   - Retry buttons on failed queries

4. **Filters** — low-hanging fruit for session management
   - Client-side filtering above session list
   - Agent dropdown, workdir input, date range

5. **TraceDB search** — requires backend endpoint first
   - Add `GET /api/v1/search` route (Rust)
   - Frontend search bar + results page
   - Handle TraceDB unavailable gracefully

6. **Native import** — last, since it depends on search
   - "Import" button on session list
   - Dialog with session ID input or search picker
   - Violet badge for native sessions

---

## Questions for Phase 3

1. **TraceDB schema:** What does `trace-db api` search return? Session list, turn list, or both? Schema affects frontend result rendering.

2. **Attachment previews:** Should video files (`video/*`) render inline, or just show filename? Backend accepts any MIME type.

3. **Model selector placement:** In create-turn form (per-turn override), or in session settings (session-wide default)?

4. **Virtual scrolling priority:** Defer to Phase 4, or add now if turn lists are already >100 events?

5. **Native session conversion:** Should it be a `PATCH /api/v1/sessions/{id}` endpoint, or a new `POST /api/v1/sessions/{id}/convert` action?

---

## Files to Modify (Phase 3)

```
backend/src/api/
├── search.rs         # NEW: TraceDB search endpoint
└── mod.rs            # Add search route

frontend/app/
├── sessions/[id]/
│   └── page.tsx      # Add attachment upload zone, model selector
├── search/
│   └── page.tsx      # NEW: Search results page
└── layout.tsx        # Wrap in ErrorBoundary

frontend/lib/
├── api.ts            # Add search() method
└── use-sse.ts        # (no changes needed)
```

---

**Phase 3 is where Sylph Web becomes feature-complete.** Attachment upload and TraceDB search are the two user-facing gaps; everything else is polish. After Phase 3, Phases 4-5 are production hardening (tests, accessibility, packaging).

Estimated timeline:
- Phase 3: 2 weeks
- Phase 4: 2 weeks
- Phase 5: 1 week
- **Total to production:** 5 weeks from Phase 3 start

---

**Current commit:** `4f815d5 feat(frontend): add SSE streaming and conversation view`  
**Next commit target:** Attachment upload working end-to-end
