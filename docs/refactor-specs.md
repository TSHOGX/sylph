# Sylph Web Refactoring Specifications

## Executive Summary

This document specifies a comprehensive greenfield refactor of Sylph Web from a plain Node.js/JavaScript application to a modern, production-ready architecture using Next.js 15+ (TypeScript, React 19+) for the frontend and Rust for the backend.

**Current State**: Early-stage prototype with suboptimal tech choices
- Plain Node.js (ES modules) with no framework
- Vanilla JavaScript frontend (no TypeScript, no React)
- SQLite accessed directly via `node:sqlite`
- NDJSON streaming over HTTP
- ~500 LOC server, ~500 LOC client

**Target State**: Modern, scalable, maintainable architecture
- **Frontend**: Next.js 16 App Router, TypeScript strict mode, React 19, Server Components
- **Backend**: Rust (Axum/Tokio), type-safe, high-performance
- **Data**: SQLite with rusqlite/sqlx, proper migrations
- **Communication**: REST + SSE (Server-Sent Events) for streaming
- **Developer Experience**: Hot reload, comprehensive testing, end-to-end type safety

**Core Product Requirements Preserved**:
1. Local-first native UI for coding agent sessions
2. Two session types: Sylph-managed (with SOUL.md) and native (preserve identity)
3. Discover agents via `--describe`, stream live events (NDJSON protocol)
4. Two-tier storage: live journal (active turns) + TraceDB (stable history)
5. Loopback-only security model with cookie-based authentication

---

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Technology Stack](#technology-stack)
3. [System Architecture](#system-architecture)
4. [File Structure](#file-structure)
5. [Database Schema](#database-schema)
6. [API Design](#api-design)
7. [Frontend Architecture](#frontend-architecture)
8. [Backend Architecture](#backend-architecture)
9. [Type Safety & Shared Types](#type-safety--shared-types)
10. [Security & Authentication](#security--authentication)
11. [Testing Strategy](#testing-strategy)
12. [Migration Strategy](#migration-strategy)
13. [Implementation Phases](#implementation-phases)
14. [Risk Analysis](#risk-analysis)
15. [Performance Considerations](#performance-considerations)
16. [Developer Experience](#developer-experience)

---

## Architecture Overview

### High-Level Design

```
┌─────────────────────────────────────────────────────────────┐
│                        Browser                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Next.js Frontend (Port 3000)                 │   │
│  │  - React 19 Server/Client Components                │   │
│  │  - TypeScript strict mode                           │   │
│  │  - Streaming UI with Suspense                       │   │
│  │  - EventSource for SSE consumption                  │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                           │
                           │ HTTP/SSE (127.0.0.1)
                           ▼
┌─────────────────────────────────────────────────────────────┐
│              Rust Backend (Port 4317)                       │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  Axum HTTP Server + Tokio Runtime                   │   │
│  │  - REST API endpoints                               │   │
│  │  - SSE streaming for live events                    │   │
│  │  - Process management (runner supervision)          │   │
│  │  - SQLite via rusqlite/sqlx                         │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                           │
                           ├──────────────────┐
                           ▼                  ▼
                    ┌─────────────┐    ┌─────────────┐
                    │  live.db    │    │  TraceDB    │
                    │  (SQLite)   │    │  (External) │
                    └─────────────┘    └─────────────┘
                           │
                           ▼
                    ┌─────────────┐
                    │   Runners   │
                    │  (Subprocs) │
                    └─────────────┘
```

### Design Principles

1. **Separation of Concerns**: Clear boundaries between presentation (Next.js), business logic (Rust backend), and data (SQLite/TraceDB)
2. **Type Safety End-to-End**: TypeScript on frontend, Rust on backend, shared type definitions via code generation
3. **Local-First Architecture**: All data stored locally, no cloud dependencies
4. **Progressive Enhancement**: Server Components for initial render, Client Components for interactivity
5. **Streaming by Default**: SSE for real-time updates, React Suspense for async UI
6. **Security Layered**: Cookie authentication, host validation, CSRF protection, input sanitization
7. **Testability**: Unit tests for logic, integration tests for API, E2E tests for critical flows

---

## Technology Stack

### Frontend

| Technology | Version | Purpose |
|------------|---------|---------|
| **Next.js** | 16+ | Full-stack React framework with App Router |
| **React** | 19+ | UI library with Server Components |
| **TypeScript** | 5.8+ | Type-safe development (strict mode) |
| **Tailwind CSS** | 4.0+ | Utility-first styling |
| **Radix UI** | Latest | Unstyled, accessible component primitives |
| **TanStack Query** | 6.0+ | Server state management, caching, mutations |
| **Zod** | 3.24+ | Runtime validation, schema generation |
| **date-fns** | 4.1+ | Date formatting (replaces moment.js) |

### Backend

| Technology | Version | Purpose |
|------------|---------|---------|
| **Rust** | 1.85+ (2021 edition) | Systems programming language |
| **Axum** | 0.8+ | Ergonomic web framework on Tokio |
| **Tokio** | 1.44+ | Async runtime |
| **SQLx** | 0.8+ | Async SQL with compile-time query checking |
| **Serde** | 1.0+ | Serialization/deserialization |
| **Tower** | 0.5+ | Middleware (rate limiting, logging) |
| **tokio-stream** | 0.1+ | Async stream utilities for SSE |
| **refinery** | 0.8+ | SQL migration framework |

### Development Tools

| Tool | Purpose |
|------|---------|
| **pnpm** | Fast, disk-efficient package manager |
| **Turbopack** | Next.js bundler (replaces Webpack) |
| **cargo-watch** | Auto-reload Rust on file changes |
| **prettier** | Code formatting (TS/JS/CSS) |
| **rustfmt** | Rust code formatting |
| **eslint** | Linting (with typescript-eslint) |
| **clippy** | Rust linting |
| **vitest** | Frontend unit/integration tests |
| **playwright** | E2E tests |

---

## System Architecture

### Communication Flow

**Session Creation (Sylph-managed)**:
```
Browser → Next.js API Route → Rust Backend → SQLite (sessions table)
                                            → Create session record
                                            ← Return session resource
                            ← JSON response
        ← Server Component renders session view
```

**Turn Execution with Streaming**:
```
Browser → POST /api/v1/sessions/{id}/turns (with prompt + attachments)
        → Rust Backend validates, creates turn record
        → Spawns runner subprocess (detached process group)
        → Returns 202 Accepted immediately
        
Browser → EventSource to GET /api/v1/turns/{turnId}/events
        → Rust Backend opens SSE stream
        → Replays stable events from SQLite
        → Streams live projection from runner stdout (NDJSON)
        → Browser updates UI incrementally
        → On completion: final assistant event, turn status update
```

### Process Supervision

The Rust backend maintains a `TurnSupervisor` that:
- Spawns runner subprocesses in detached process groups
- Parses NDJSON events from stdout (non-blocking)
- Manages bounded event projections (max 256 events in memory)
- Handles graceful shutdown (SIGTERM → SIGKILL after 2s)
- Persists stable events (thinking, tool_call, tool_result, usage) to SQLite
- Marks interrupted turns on service restart

---

## File Structure

### Monorepo Layout (Workspace Root)

```
sylph-web/
├── Cargo.toml                # Rust workspace manifest
├── package.json              # Node.js workspace (pnpm)
├── pnpm-workspace.yaml       # pnpm workspace config
├── .gitignore
├── README.md
├── docs/
│   ├── architecture.md       # (existing, updated)
│   ├── product.md            # (existing)
│   ├── refactor-specs.md     # (this document)
│   └── adr/
│       └── 0001-runtime-boundaries.md
│
├── backend/                  # Rust backend workspace
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs           # Entry point, Axum server setup
│   │   ├── lib.rs            # Library exports for testing
│   │   ├── config.rs         # Configuration loading
│   │   ├── api/              # HTTP handlers
│   │   │   ├── mod.rs
│   │   │   ├── health.rs
│   │   │   ├── sessions.rs
│   │   │   ├── turns.rs
│   │   │   ├── agents.rs
│   │   │   └── events.rs     # SSE streaming
│   │   ├── models/           # Domain models
│   │   │   ├── mod.rs
│   │   │   ├── session.rs
│   │   │   ├── turn.rs
│   │   │   ├── event.rs
│   │   │   └── agent.rs
│   │   ├── db/               # Database layer
│   │   │   ├── mod.rs
│   │   │   ├── store.rs      # LiveStore abstraction
│   │   │   └── migrations/   # SQL migrations (refinery)
│   │   │       ├── V1__initial_schema.sql
│   │   │       ├── V2__turns_and_incarnations.sql
│   │   │       └── V3__attachments_commands.sql
│   │   ├── runners/          # Runner discovery & execution
│   │   │   ├── mod.rs
│   │   │   ├── registry.rs
│   │   │   ├── supervisor.rs
│   │   │   └── protocol.rs   # NDJSON parsing
│   │   ├── tracedb/          # TraceDB client
│   │   │   ├── mod.rs
│   │   │   └── client.rs
│   │   ├── middleware/       # Axum middleware
│   │   │   ├── mod.rs
│   │   │   ├── auth.rs       # Cookie validation
│   │   │   └── security.rs   # CSRF, host validation
│   │   └── error.rs          # Error types
│   ├── tests/
│   │   ├── integration/      # Integration tests
│   │   └── fixtures/         # Test fixtures
│   └── .env.example
│
├── frontend/                 # Next.js frontend
│   ├── package.json
│   ├── next.config.ts
│   ├── tsconfig.json
│   ├── tailwind.config.ts
│   ├── postcss.config.mjs
│   ├── src/
│   │   ├── app/              # Next.js App Router
│   │   │   ├── layout.tsx    # Root layout
│   │   │   ├── page.tsx      # Home page
│   │   │   ├── sessions/
│   │   │   │   └── [id]/
│   │   │   │       └── page.tsx  # Session detail
│   │   │   └── api/          # API routes (proxy to Rust)
│   │   │       └── [...path]/
│   │   │           └── route.ts
│   │   ├── components/       # React components
│   │   │   ├── ui/           # Radix UI wrappers
│   │   │   ├── session-list.tsx
│   │   │   ├── conversation.tsx
│   │   │   ├── event-node.tsx
│   │   │   ├── composer.tsx
│   │   │   └── new-session-dialog.tsx
│   │   ├── lib/              # Utilities
│   │   │   ├── api-client.ts
│   │   │   ├── sse.ts        # EventSource wrapper
│   │   │   └── utils.ts
│   │   ├── hooks/            # Custom React hooks
│   │   │   ├── use-sessions.ts
│   │   │   ├── use-turn-events.ts
│   │   │   └── use-agents.ts
│   │   ├── types/            # TypeScript types
│   │   │   ├── api.ts        # API response types
│   │   │   └── models.ts     # Domain models
│   │   └── styles/
│   │       └── globals.css
│   ├── public/
│   │   └── favicon.svg
│   └── .env.local.example
│
├── shared/                   # Shared types (optional)
│   ├── package.json
│   └── src/
│       └── api-schema.ts     # Shared API types (Zod schemas)
│
└── scripts/                  # Development scripts
    ├── dev.sh                # Start both frontend and backend
    ├── build.sh              # Production build
    └── setup.sh              # Initial setup

```

### Key Design Decisions

1. **Monorepo**: Keeps frontend and backend in sync, simplifies type sharing
2. **Backend as library**: `src/lib.rs` exports types for integration tests
3. **API routes in Next.js**: Proxy `/api/*` to Rust backend (avoids CORS)
4. **Migrations in SQL**: SQL-first migrations (not ORM-generated) for transparency
5. **Shared types**: Optional `shared/` package for Zod schemas used by both sides

---

## Database Schema

### SQLite Schema (live.db)

The schema preserves the existing structure with minor refinements for Rust compatibility.

```sql
-- V1__initial_schema.sql
CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  origin TEXT NOT NULL CHECK (origin IN ('sylph', 'native')),
  identity_mode TEXT NOT NULL CHECK (identity_mode IN ('sylph', 'preserve')),
  lifecycle_mode TEXT NOT NULL CHECK (lifecycle_mode IN ('sylph', 'preserve')),
  agent TEXT NOT NULL,
  backend_session_id TEXT,
  trace_session_id TEXT,
  workdir TEXT NOT NULL,
  title TEXT,
  flags_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'idle' CHECK (status IN ('idle', 'running')),
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);

CREATE INDEX sessions_updated_idx ON sessions(updated_at_ms DESC);
CREATE INDEX sessions_trace_idx ON sessions(trace_session_id) WHERE trace_session_id IS NOT NULL;

CREATE TABLE backend_incarnations (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  agent TEXT NOT NULL,
  backend_session_id TEXT,
  started_at_ms INTEGER NOT NULL,
  ended_at_ms INTEGER,
  reset_reason TEXT,
  source_trace_session_id TEXT,
  resulting_trace_session_id TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'ended'))
);

CREATE INDEX incarnations_session_idx ON backend_incarnations(session_id, started_at_ms DESC);

-- V2__turns_and_events.sql
CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  incarnation_id TEXT REFERENCES backend_incarnations(id),
  status TEXT NOT NULL CHECK (status IN (
    'queued', 'starting', 'running', 'stopping',
    'completed', 'failed', 'cancelled', 'interrupted'
  )),
  request_text TEXT NOT NULL DEFAULT '',
  command_invocation_id TEXT,
  idempotency_key TEXT,
  error_code TEXT,
  error_message TEXT,
  runner_exit_code INTEGER,
  created_at_ms INTEGER NOT NULL,
  started_at_ms INTEGER,
  completed_at_ms INTEGER,
  stopped_at_ms INTEGER,
  UNIQUE(session_id, idempotency_key)
);

CREATE INDEX turns_session_idx ON turns(session_id, created_at_ms DESC);
CREATE INDEX turns_status_idx ON turns(status) WHERE status IN ('queued', 'starting', 'running', 'stopping');

CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN (
    'status', 'system', 'user', 'thinking', 'assistant',
    'tool_call', 'tool_result', 'usage', 'error', 'approval'
  )),
  phase TEXT,  -- 'started', 'delta', 'completed'
  stream_id TEXT,
  name TEXT,
  text TEXT NOT NULL DEFAULT '',
  data_json TEXT NOT NULL DEFAULT '{}',
  is_error INTEGER NOT NULL DEFAULT 0,
  stability TEXT NOT NULL DEFAULT 'stable' CHECK (stability IN ('ephemeral', 'stable')),
  source TEXT NOT NULL DEFAULT 'server' CHECK (source IN ('runner', 'server', 'tracedb')),
  created_at_ms INTEGER NOT NULL,
  reconciled_at_ms INTEGER,
  UNIQUE(session_id, seq)
);

CREATE INDEX events_session_idx ON events(session_id, seq);
CREATE INDEX events_turn_idx ON events(turn_id, seq) WHERE turn_id IS NOT NULL;

-- V3__attachments_commands.sql
CREATE TABLE command_invocations (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT NOT NULL UNIQUE REFERENCES turns(id) ON DELETE CASCADE,
  command TEXT NOT NULL,
  arguments_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'queued',
  result_text TEXT,
  error_code TEXT,
  error_message TEXT,
  created_at_ms INTEGER NOT NULL,
  completed_at_ms INTEGER
);

CREATE INDEX command_invocations_session_idx ON command_invocations(session_id, created_at_ms DESC);

CREATE TABLE attachments (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  turn_id TEXT REFERENCES turns(id) ON DELETE SET NULL,
  original_name TEXT NOT NULL,
  staged_name TEXT NOT NULL,
  absolute_path TEXT NOT NULL,
  mime_type TEXT NOT NULL DEFAULT 'application/octet-stream',
  byte_size INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'staged' CHECK (status IN ('staged', 'active', 'expired', 'deleted')),
  created_at_ms INTEGER NOT NULL,
  expires_at_ms INTEGER
);

CREATE INDEX attachments_cleanup_idx ON attachments(status, expires_at_ms) WHERE expires_at_ms IS NOT NULL;

-- V4__trace_links.sql (future reconciliation)
CREATE TABLE trace_links (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
  incarnation_id TEXT REFERENCES backend_incarnations(id),
  turn_id TEXT REFERENCES turns(id),
  source_trace_session_id TEXT,
  resulting_trace_session_id TEXT,
  reconciliation_status TEXT NOT NULL DEFAULT 'pending' CHECK (
    reconciliation_status IN ('pending', 'verified', 'failed', 'skipped')
  ),
  attempts INTEGER NOT NULL DEFAULT 0,
  last_error TEXT,
  verified_at_ms INTEGER,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL,
  UNIQUE(session_id, incarnation_id, turn_id)
);

CREATE INDEX trace_links_status_idx ON trace_links(reconciliation_status, updated_at_ms);
```

### Rust Type Mapping

```rust
// src/models/session.rs
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: String,
    pub origin: SessionOrigin,
    pub identity_mode: IdentityMode,
    pub lifecycle_mode: LifecycleMode,
    pub agent: String,
    pub backend_session_id: Option<String>,
    pub trace_session_id: Option<String>,
    pub workdir: String,
    pub title: Option<String>,
    pub flags_json: String,  // JSON string, parsed to serde_json::Value when needed
    pub status: SessionStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum SessionOrigin {
    Sylph,
    Native,
}

// Similar enums for IdentityMode, LifecycleMode, SessionStatus, etc.
```

### Migration Strategy for Existing Databases

The Rust backend will detect existing prototype databases (schema version 0) and apply migrations 1-4 transactionally.

---

## API Design

### RESTful Endpoints (Rust Backend)

All endpoints follow the OpenAPI 3.1 spec in `docs/api-v1.openapi.json` with minor refinements for Rust idioms.

#### Core Endpoints

| Method | Path | Description | Request | Response |
|--------|------|-------------|---------|----------|
| GET | `/api/v1/health` | Service health check | - | `{ ok: true, apiVersion, schemaVersion, tracedb }` |
| GET | `/api/v1/diagnostics` | Detailed diagnostics | - | System state snapshot |
| GET | `/api/v1/agents` | List discovered agents | - | `{ agents: AgentSpec[], broken: Record<string, string> }` |
| POST | `/api/v1/agents/refresh` | Refresh agent discovery | - | Updated agent list |
| GET | `/api/v1/sessions` | List sessions | `?limit=50` | `{ sessions: Session[] }` |
| POST | `/api/v1/sessions` | Create Sylph session | `CreateSessionRequest` | `201 { session: Session }` |
| POST | `/api/v1/sessions/import` | Import native session | `{ traceSessionId }` | `201 { session: Session }` |
| GET | `/api/v1/sessions/{id}` | Get session detail | - | `{ session, incarnations, turns }` |
| GET | `/api/v1/sessions/{id}/turns` | List turns | `?limit=50&before=<ms>` | `{ turns: Turn[] }` |
| POST | `/api/v1/sessions/{id}/turns` | Create turn | `CreateTurnRequest` | `202 { turn: Turn }` |
| GET | `/api/v1/turns/{id}` | Get turn detail | - | `{ turn: Turn }` |
| GET | `/api/v1/turns/{id}/events` | Stream turn events (SSE) | `?after_seq=-1` | `text/event-stream` |
| POST | `/api/v1/turns/{id}/stop` | Stop running turn | - | `202 { turn: Turn }` |

#### Request/Response Types (TypeScript)

```typescript
// frontend/src/types/api.ts
export interface CreateSessionRequest {
  agent: string;
  workdir: string;
  title?: string;
  flags?: Record<string, string>;
}

export interface CreateTurnRequest {
  text?: string;
  attachments?: Attachment[];
}

export interface Attachment {
  name: string;
  type: string;
  data: string;  // base64
}

export interface Session {
  id: string;
  origin: 'sylph' | 'native';
  identityMode: 'sylph' | 'preserve';
  lifecycleMode: 'sylph' | 'preserve';
  agent: string;
  workdir: string;
  title?: string;
  flags: Record<string, string>;
  status: 'idle' | 'running';
  createdAtMs: number;
  updatedAtMs: number;
  // backendSessionId and traceSessionId are never exposed
}

export interface Turn {
  id: string;
  sessionId: string;
  incarnationId?: string;
  status: 'queued' | 'starting' | 'running' | 'stopping' | 'completed' | 'failed' | 'cancelled' | 'interrupted';
  requestText: string;
  errorCode?: string;
  errorMessage?: string;
  runnerExitCode?: number;
  createdAtMs: number;
  startedAtMs?: number;
  completedAtMs?: number;
  stoppedAtMs?: number;
}

export interface Event {
  id?: number;
  sessionId: string;
  turnId?: string;
  seq: number;
  kind: 'status' | 'system' | 'user' | 'thinking' | 'assistant' | 'tool_call' | 'tool_result' | 'usage' | 'error' | 'approval';
  phase?: 'started' | 'delta' | 'completed';
  streamId?: string;
  name?: string;
  text: string;
  data: Record<string, any>;
  isError: boolean;
  stability: 'ephemeral' | 'stable';
  source: 'runner' | 'server' | 'tracedb';
  createdAtMs: number;
}

export interface AgentSpec {
  name: string;
  version: string;
  description?: string;
  flags: Record<string, FlagSpec>;
  commands: Record<string, CommandSpec>;
}

export interface FlagSpec {
  description?: string;
  choices: string[];
  default?: string;
}

export interface CommandSpec {
  description?: string;
  args: Record<string, ArgSpec>;
}

export interface ArgSpec {
  type: 'string' | 'int' | 'bool';
  description?: string;
  required?: boolean;
}
```

### SSE Stream Format

SSE messages follow the format:

```
id: <event.seq>
data: <JSON-serialized message>

```

Message types:

```typescript
type SSEMessage =
  | { type: 'event'; event: Event }
  | { type: 'runner.event'; event: RunnerEvent }
  | { type: 'turn.started'; turnId: string; sessionId: string; agent: string }
  | { type: 'turn.completed'; turnId: string; sessionId: string; assistant: Event }
  | { type: 'turn.failed'; turnId: string; error: { code: string; message: string } }
  | { type: 'turn.stopping'; turnId: string }
  | { type: 'session.reset'; reason: string }
  | { type: 'command.completed'; turnId: string; sessionId: string; command: string; text: string }
  | { type: 'command.failed'; command: string; error: string };
```

### Error Handling

Rust backend returns structured error envelopes:

```json
{
  "error": {
    "code": "SESSION_NOT_FOUND",
    "message": "Session not found",
    "details": {}
  }
}
```

Error codes match the OpenAPI spec: `INVALID_REQUEST`, `SESSION_NOT_FOUND`, `RUNNER_NOT_FOUND`, `TURN_FAILED`, `ATTACHMENT_INVALID`, etc.

---

## Frontend Architecture

### Next.js App Router Structure

```
frontend/src/app/
├── layout.tsx                # Root layout (RootProvider, theme, fonts)
├── page.tsx                  # Home page (session list + empty state)
├── sessions/
│   └── [id]/
│       ├── layout.tsx        # Session layout (sidebar + conversation)
│       └── page.tsx          # Session detail (Server Component)
├── api/
│   └── [...path]/
│       └── route.ts          # Proxy to Rust backend
└── not-found.tsx
```

### Component Hierarchy

```
RootLayout (Server Component)
  ├── ThemeProvider (Client)
  ├── QueryClientProvider (Client)
  └── {children}

HomePage (Server Component)
  ├── SessionSidebar (Client)
  │   ├── NewSessionButton → NewSessionDialog (Client)
  │   ├── SessionSearch (Client)
  │   ├── AgentFilters (Client)
  │   └── SessionList (Client)
  │       └── SessionRow[] (Client)
  └── EmptyState (Server)

SessionPage (Server Component)
  ├── SessionHeader (Client)
  ├── PolicyBanner (Server)
  ├── Conversation (Client)
  │   ├── Timeline (Client)
  │   │   └── EventNode[] (Client)
  │   └── Composer (Client)
  │       ├── PromptTextarea (Client)
  │       ├── AttachmentList (Client)
  │       └── AttachButton (Client)
  └── CommandDialog (Client)
```

### State Management

**TanStack Query** for server state:

```typescript
// frontend/src/hooks/use-sessions.ts
export function useSessions() {
  return useQuery({
    queryKey: ['sessions'],
    queryFn: async () => {
      const res = await fetch('/api/v1/sessions');
      if (!res.ok) throw new Error('Failed to fetch sessions');
      return res.json();
    },
    refetchInterval: 30_000,  // Poll every 30s
  });
}

export function useCreateSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (data: CreateSessionRequest) => {
      const res = await fetch('/api/v1/sessions', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(data),
      });
      if (!res.ok) throw new Error('Failed to create session');
      return res.json();
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] });
    },
  });
}
```

**Local component state** for UI-only state (dialog open/close, form inputs).

**Zustand** (optional) for global UI state like filters, search query:

```typescript
// frontend/src/lib/store.ts
import { create } from 'zustand';

interface AppState {
  agentFilter: string;
  setAgentFilter: (filter: string) => void;
  searchQuery: string;
  setSearchQuery: (query: string) => void;
}

export const useAppStore = create<AppState>((set) => ({
  agentFilter: 'all',
  setAgentFilter: (filter) => set({ agentFilter: filter }),
  searchQuery: '',
  setSearchQuery: (query) => set({ searchQuery: query }),
}));
```

### SSE Integration

```typescript
// frontend/src/hooks/use-turn-events.ts
import { useEffect, useState } from 'react';
import { SSEMessage, Event } from '@/types/api';

export function useTurnEvents(turnId: string) {
  const [events, setEvents] = useState<Event[]>([]);
  const [liveEvents, setLiveEvents] = useState<SSEMessage[]>([]);
  const [status, setStatus] = useState<'connecting' | 'open' | 'closed'>('connecting');

  useEffect(() => {
    const eventSource = new EventSource(`/api/v1/turns/${turnId}/events`);

    eventSource.onopen = () => setStatus('open');
    eventSource.onerror = () => setStatus('closed');

    eventSource.onmessage = (e) => {
      const message: SSEMessage = JSON.parse(e.data);
      
      if (message.type === 'event') {
        setEvents((prev) => [...prev, message.event]);
      } else {
        setLiveEvents((prev) => [...prev, message]);
      }
    };

    return () => eventSource.close();
  }, [turnId]);

  return { events, liveEvents, status };
}
```

### Styling with Tailwind CSS

Utility-first with custom design tokens:

```typescript
// frontend/tailwind.config.ts
import type { Config } from 'tailwindcss';

const config: Config = {
  content: ['./src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        sylph: {
          50: '#f0fdf4',
          500: '#10b981',
          900: '#064e3b',
        },
        native: {
          50: '#f5f3ff',
          500: '#8b5cf6',
          900: '#4c1d95',
        },
      },
    },
  },
  plugins: [require('@tailwindcss/typography')],
};

export default config;
```

### Accessibility

- Semantic HTML (nav, main, article, aside)
- ARIA labels on interactive elements
- Keyboard navigation (Tab, Enter, Escape)
- Focus management (modal traps, skip links)
- Screen reader announcements for live regions (`aria-live="polite"` for event stream)

---

## Backend Architecture

### Axum Server Setup

```rust
// backend/src/main.rs
use axum::{
    Router,
    routing::{get, post},
};
use tower::ServiceBuilder;
use tower_http::trace::TraceLayer;
use std::net::SocketAddr;

mod api;
mod config;
mod db;
mod middleware;
mod models;
mod runners;
mod tracedb;
mod error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = config::load()?;
    let store = db::LiveStore::new(&config.database_path).await?;
    let tracedb = tracedb::Client::new(&config.tracedb_bin);
    let runners = runners::Registry::new(&config.runners_dir).await?;
    let supervisor = runners::Supervisor::new(store.clone(), runners.clone());

    let app = Router::new()
        .route("/api/v1/health", get(api::health::get))
        .route("/api/v1/diagnostics", get(api::health::diagnostics))
        .route("/api/v1/agents", get(api::agents::list))
        .route("/api/v1/agents/refresh", post(api::agents::refresh))
        .route("/api/v1/sessions", get(api::sessions::list).post(api::sessions::create))
        .route("/api/v1/sessions/import", post(api::sessions::import))
        .route("/api/v1/sessions/:id", get(api::sessions::get))
        .route("/api/v1/sessions/:id/turns", get(api::turns::list).post(api::turns::create))
        .route("/api/v1/turns/:id", get(api::turns::get))
        .route("/api/v1/turns/:id/events", get(api::events::stream))
        .route("/api/v1/turns/:id/stop", post(api::turns::stop))
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(middleware::auth::layer())
                .layer(middleware::security::layer())
        )
        .with_state(AppState {
            store,
            tracedb,
            runners,
            supervisor,
            cookie_secret: config.cookie_secret,
        });

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    println!("Sylph Web backend listening on http://{}", addr);
    
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

#[derive(Clone)]
struct AppState {
    store: db::LiveStore,
    tracedb: tracedb::Client,
    runners: runners::Registry,
    supervisor: Arc<runners::Supervisor>,
    cookie_secret: String,
}
```

### Database Layer (SQLx)

```rust
// backend/src/db/store.rs
use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};
use crate::models::{Session, Turn, Event};

#[derive(Clone)]
pub struct LiveStore {
    pool: SqlitePool,
}

impl LiveStore {
    pub async fn new(path: &str) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&format!("sqlite:{}?mode=rwc", path))
            .await?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await?;

        Ok(Self { pool })
    }

    pub async fn create_session(&self, session: &Session) -> Result<Session, sqlx::Error> {
        sqlx::query_as::<_, Session>(
            "INSERT INTO sessions (id, origin, identity_mode, lifecycle_mode, agent, workdir, title, flags_json, status, created_at_ms, updated_at_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING *"
        )
        .bind(&session.id)
        .bind(&session.origin)
        .bind(&session.identity_mode)
        .bind(&session.lifecycle_mode)
        .bind(&session.agent)
        .bind(&session.workdir)
        .bind(&session.title)
        .bind(&session.flags_json)
        .bind(&session.status)
        .bind(session.created_at_ms)
        .bind(session.updated_at_ms)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn get_session(&self, id: &str) -> Result<Option<Session>, sqlx::Error> {
        sqlx::query_as::<_, Session>("SELECT * FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_sessions(&self, limit: i64) -> Result<Vec<Session>, sqlx::Error> {
        sqlx::query_as::<_, Session>(
            "SELECT * FROM sessions ORDER BY updated_at_ms DESC LIMIT ?"
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    // Similar methods for turns, events, etc.
}
```

### Process Supervision

```rust
// backend/src/runners/supervisor.rs
use tokio::process::Command;
use tokio::sync::{mpsc, RwLock};
use std::collections::HashMap;
use std::sync::Arc;

pub struct Supervisor {
    store: LiveStore,
    runners: Registry,
    processes: Arc<RwLock<HashMap<String, ProcessHandle>>>,
    subscribers: Arc<RwLock<HashMap<String, Vec<EventSender>>>>,
}

impl Supervisor {
    pub async fn start_turn(
        &self,
        turn_id: String,
        session: Session,
        prompt: String,
        attachments: Vec<String>,
    ) -> Result<(), Error> {
        let runner = self.runners.get(&session.agent)
            .ok_or(Error::RunnerNotFound)?;

        // Spawn runner subprocess
        let mut cmd = Command::new(&runner.path);
        cmd.arg("--workdir").arg(&session.workdir);
        
        if let Some(backend_id) = &session.backend_session_id {
            cmd.arg("--session-id").arg(backend_id);
        }

        for (key, value) in &session.flags {
            cmd.arg("--flag").arg(format!("{}={}", key, value));
        }

        for path in &attachments {
            cmd.arg("--attachment").arg(path);
        }

        cmd.env("SYLPH_STREAM_FORMAT", "ndjson-v1");
        if session.identity_mode == IdentityMode::Preserve {
            cmd.env("SYLPH_SOUL", "");
        }

        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        // Write prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        // Track process
        let handle = ProcessHandle {
            child,
            turn_id: turn_id.clone(),
        };
        self.processes.write().await.insert(turn_id.clone(), handle);

        // Spawn event consumer task
        tokio::spawn(self.consume_events(turn_id, session));

        Ok(())
    }

    async fn consume_events(&self, turn_id: String, session: Session) {
        // Read stdout line by line, parse NDJSON events, broadcast to subscribers
        // Persist stable events to database
        // Handle completion/failure
    }

    pub async fn subscribe(&self, turn_id: &str) -> mpsc::Receiver<SSEMessage> {
        let (tx, rx) = mpsc::channel(256);
        self.subscribers.write().await
            .entry(turn_id.to_string())
            .or_default()
            .push(tx);
        rx
    }
}
```

### SSE Streaming

```rust
// backend/src/api/events.rs
use axum::{
    extract::{Path, State, Query},
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::{self, Stream};
use std::convert::Infallible;

pub async fn stream(
    Path(turn_id): Path<String>,
    Query(params): Query<EventsQuery>,
    State(state): State<AppState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, Error> {
    let turn = state.store.get_turn(&turn_id).await?
        .ok_or(Error::TurnNotFound)?;

    // Replay stable events from database
    let stable_events = state.store.get_events(&turn.session_id, &turn_id, params.after_seq).await?;
    
    // Subscribe to live events
    let mut live_rx = state.supervisor.subscribe(&turn_id).await;

    let stable_stream = stream::iter(stable_events)
        .map(|event| {
            let data = serde_json::to_string(&SSEMessage::Event { event }).unwrap();
            Ok(Event::default()
                .id(event.seq.to_string())
                .data(data))
        });

    let live_stream = async_stream::stream! {
        while let Some(message) = live_rx.recv().await {
            let data = serde_json::to_string(&message).unwrap();
            let event_id = match &message {
                SSEMessage::Event { event } => Some(event.seq.to_string()),
                _ => None,
            };
            let mut evt = Event::default().data(data);
            if let Some(id) = event_id {
                evt = evt.id(id);
            }
            yield Ok(evt);
        }
    };

    let combined = stable_stream.chain(live_stream);

    Ok(Sse::new(combined).keep_alive(KeepAlive::default()))
}
```

---

## Type Safety & Shared Types

### Strategy

1. **Backend Types**: Rust structs with `#[derive(Serialize, Deserialize)]`
2. **Frontend Types**: TypeScript interfaces mirroring Rust structs
3. **Code Generation**: Use `typeshare` to generate TypeScript from Rust (optional)
4. **Runtime Validation**: Zod schemas for request validation on frontend

### Example: Shared Type Definition

```rust
// backend/src/models/session.rs
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub origin: SessionOrigin,
    pub identity_mode: IdentityMode,
    pub lifecycle_mode: LifecycleMode,
    pub agent: String,
    pub workdir: String,
    pub title: Option<String>,
    pub flags: serde_json::Value,
    pub status: SessionStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
```

```typescript
// frontend/src/types/api.ts (hand-written or generated)
export interface Session {
  id: string;
  origin: 'sylph' | 'native';
  identityMode: 'sylph' | 'preserve';
  lifecycleMode: 'sylph' | 'preserve';
  agent: string;
  workdir: string;
  title?: string;
  flags: Record<string, string>;
  status: 'idle' | 'running';
  createdAtMs: number;
  updatedAtMs: number;
}
```

### Zod Validation on Frontend

```typescript
// frontend/src/lib/validation.ts
import { z } from 'zod';

export const createSessionSchema = z.object({
  agent: z.string().min(1),
  workdir: z.string().min(1),
  title: z.string().optional(),
  flags: z.record(z.string()).optional(),
});

export type CreateSessionInput = z.infer<typeof createSessionSchema>;
```

---

## Security & Authentication

### Loopback-Only Model

1. **Bind to 127.0.0.1**: Backend listens only on localhost
2. **Cookie Authentication**:
   - Root page sets `HttpOnly`, `SameSite=Strict`, `Secure=false` cookie
   - Cookie value is a random 24-byte secret generated at startup
   - All API requests validate cookie presence
3. **Host Validation**: Reject requests with `Host` header outside `[127.0.0.1:<port>, localhost:<port>, [::1]:<port>]`
4. **CSRF Protection**: Mutations (POST/PUT/DELETE) reject mismatched `Origin` header
5. **Input Validation**:
   - Workdir must exist and be absolute path
   - Agent name must be from discovered runners
   - Attachments: max 5 files, 8 MiB per file, 10 MiB total
   - Base64 validation for attachments
   - Command/flag validation against agent spec

### Rust Middleware

```rust
// backend/src/middleware/auth.rs
use axum::{
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

pub async fn validate_cookie<B>(
    req: Request<B>,
    next: Next<B>,
) -> Result<Response, StatusCode> {
    let cookies = req.headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let secret = req.extensions().get::<String>().unwrap();
    let expected = format!("sylph_session={}", secret);

    if !cookies.split(';').any(|c| c.trim() == expected) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
}
```

### Next.js Proxy (CORS Avoidance)

Next.js API routes proxy `/api/*` to Rust backend, avoiding CORS setup:

```typescript
// frontend/src/app/api/[...path]/route.ts
import { NextRequest } from 'next/server';

const BACKEND_URL = process.env.BACKEND_URL || 'http://127.0.0.1:4317';

export async function GET(req: NextRequest, { params }: { params: { path: string[] } }) {
  const path = params.path.join('/');
  const url = new URL(path, BACKEND_URL);
  url.search = req.nextUrl.search;

  const res = await fetch(url, {
    headers: { cookie: req.headers.get('cookie') || '' },
  });

  return new Response(res.body, {
    status: res.status,
    headers: res.headers,
  });
}

export async function POST(req: NextRequest, { params }: { params: { path: string[] } }) {
  const path = params.path.join('/');
  const url = new URL(path, BACKEND_URL);

  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'content-type': req.headers.get('content-type') || 'application/json',
      cookie: req.headers.get('cookie') || '',
      origin: req.headers.get('origin') || '',
    },
    body: req.body,
  });

  return new Response(res.body, {
    status: res.status,
    headers: res.headers,
  });
}

// Similar for PUT, DELETE
```

---

## Testing Strategy

### Backend (Rust)

**Unit Tests**: Test individual functions in isolation

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_origin_serialization() {
        let origin = SessionOrigin::Sylph;
        let json = serde_json::to_string(&origin).unwrap();
        assert_eq!(json, "\"sylph\"");
    }

    #[tokio::test]
    async fn test_create_session() {
        let store = LiveStore::new(":memory:").await.unwrap();
        let session = Session {
            id: "test_123".to_string(),
            origin: SessionOrigin::Sylph,
            // ...
        };
        let created = store.create_session(&session).await.unwrap();
        assert_eq!(created.id, "test_123");
    }
}
```

**Integration Tests**: Test full API flows

```rust
// backend/tests/integration/sessions_test.rs
use axum::http::StatusCode;
use tower::ServiceExt;

#[tokio::test]
async fn test_create_session_flow() {
    let app = create_test_app().await;
    
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"agent":"claude","workdir":"/tmp"}"#))
                .unwrap()
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
}
```

### Frontend (Vitest + React Testing Library)

**Component Tests**:

```typescript
// frontend/src/components/__tests__/session-list.test.tsx
import { render, screen } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { SessionList } from '../session-list';

describe('SessionList', () => {
  it('renders sessions', () => {
    const queryClient = new QueryClient();
    render(
      <QueryClientProvider client={queryClient}>
        <SessionList />
      </QueryClientProvider>
    );
    expect(screen.getByText(/sessions/i)).toBeInTheDocument();
  });
});
```

### E2E Tests (Playwright)

```typescript
// frontend/e2e/session-flow.spec.ts
import { test, expect } from '@playwright/test';

test('create and view session', async ({ page }) => {
  await page.goto('http://localhost:3000');
  
  // Click "New Session"
  await page.click('text=New Session');
  
  // Fill form
  await page.selectOption('select[name="agent"]', 'claude');
  await page.fill('input[name="workdir"]', '/tmp');
  
  // Submit
  await page.click('button[type="submit"]');
  
  // Verify session appears
  await expect(page.locator('.session-row')).toBeVisible();
});
```

### Coverage Targets

- Backend: 80% line coverage (unit + integration)
- Frontend: 70% component coverage
- E2E: Critical user journeys (session creation, turn execution, native import)

---

## Migration Strategy

### Phase 0: Preparation (No Code Changes)

1. **Backup existing data**: Copy `~/.agents/data/sylph/web/live.sqlite3`
2. **Document current behavior**: Record existing session/turn/event flows
3. **Set up new repo structure**: Create `backend/` and `frontend/` directories
4. **Install toolchains**: Rust 1.83+, Node.js 22+, pnpm

### Phase 1: Backend Foundation

**Goal**: Rust backend with feature parity to current Node.js server

**Steps**:
1. Create `backend/Cargo.toml` with dependencies (axum, sqlx, tokio, serde)
2. Port database schema to `backend/src/db/migrations/`
3. Implement `LiveStore` with SQLx
4. Port `RunnerRegistry` and runner discovery
5. Implement core API endpoints (health, agents, sessions, turns)
6. Port `TurnSupervisor` with async process management
7. Implement SSE streaming
8. Add authentication middleware (cookie validation, host checking)
9. Write integration tests

**Validation**:
- All existing API endpoints work identically
- Existing `live.sqlite3` can be opened and migrated
- Curl/HTTPie tests pass against Rust backend

**Duration**: 2-3 weeks (1 developer)

### Phase 2: Frontend Skeleton

**Goal**: Next.js app with basic routing and API integration

**Steps**:
1. Create `frontend/` with Next.js 15 + TypeScript
2. Set up Tailwind CSS and base styles
3. Create API proxy routes (`/api/[...path]/route.ts`)
4. Implement basic layouts (root, session)
5. Port session list component (no styling yet)
6. Port conversation component (timeline, composer)
7. Set up TanStack Query with API client
8. Implement SSE hook (`use-turn-events.ts`)

**Validation**:
- Can list sessions
- Can create Sylph session
- Can view session detail
- Can send turn and see events stream

**Duration**: 2 weeks

### Phase 3: Feature Completion

**Goal**: Full feature parity with current app

**Steps**:
1. Port all UI components (new session dialog, command dialog, filters)
2. Implement attachments (upload, preview, validation)
3. Add native session import flow
4. Implement search with TraceDB integration
5. Add agent-specific flag controls
6. Port session policy banners
7. Implement stop turn functionality
8. Add error handling and loading states

**Validation**:
- All current features work
- UI matches or improves on current design
- No regressions in functionality

**Duration**: 2 weeks

### Phase 4: Polish & Testing

**Goal**: Production-ready quality

**Steps**:
1. Write comprehensive unit tests (backend + frontend)
2. Write E2E tests for critical flows
3. Add accessibility improvements (keyboard nav, ARIA labels)
4. Performance optimization (bundle size, SSE reconnection, caching)
5. Documentation (README, architecture docs, API docs)
6. Developer experience (dev scripts, hot reload, debugging)

**Validation**:
- 80%+ backend coverage
- 70%+ frontend coverage
- E2E tests pass
- Lighthouse score 90+
- No console errors/warnings

**Duration**: 1-2 weeks

### Phase 5: Deployment & Rollout

**Goal**: Replace current implementation

**Steps**:
1. Package backend as standalone binary
2. Build optimized frontend bundle
3. Create launch script (starts both backend and frontend)
4. Migrate existing user data (if schema changed)
5. Update documentation
6. Monitor for issues

**Rollback Plan**:
- Keep old Node.js implementation in `legacy/` branch
- Database backups before migration
- Feature flag to switch between old/new backend

**Total Duration**: 7-10 weeks (1 full-time developer)

---

## Implementation Phases

### Phase 1: Backend Core (Weeks 1-3)

**Deliverables**:
- Rust Axum server on port 4317
- SQLite with SQLx + migrations
- All `/api/v1/*` endpoints functional
- Runner discovery and process supervision
- SSE streaming for turn events
- Authentication middleware
- Integration test suite

**Critical Path**:
1. Database layer (migrations, LiveStore)
2. Runner registry and supervisor
3. API handlers (sessions, turns, events)
4. SSE implementation
5. Testing

**Risks**:
- SQLx async learning curve
- SSE streaming edge cases (reconnection, backpressure)
- Process supervision on Windows (if supporting)

**Mitigation**:
- Start with SQLite synchronous API, migrate to SQLx incrementally
- Study existing Node.js SSE implementation closely
- Focus on Unix/macOS first, Windows later

### Phase 2: Frontend Foundation (Weeks 4-5)

**Deliverables**:
- Next.js 15 App Router structure
- API proxy routes
- Basic session list and conversation UI
- SSE integration with EventSource
- TanStack Query setup

**Critical Path**:
1. Next.js setup and routing
2. API client with type safety
3. Session list component
4. Conversation timeline
5. SSE hook and real-time updates

**Risks**:
- Next.js App Router learning curve (Server/Client Components)
- SSE reconnection logic
- State management complexity

**Mitigation**:
- Follow Next.js 15 official patterns
- Reuse existing EventSource logic from app.js
- Keep state simple initially (TanStack Query + local state)

### Phase 3: Feature Parity (Weeks 6-7)

**Deliverables**:
- All UI components ported
- Attachments, commands, search
- Native session import
- Agent flags and filters
- Error handling

**Critical Path**:
1. Dialogs (new session, commands)
2. Attachment upload and validation
3. TraceDB integration (search, import)
4. Filters and flags
5. Error boundaries

**Risks**:
- UI state complexity
- File upload edge cases
- Search performance with large TraceDB

**Mitigation**:
- Use Radix UI for dialog primitives
- Comprehensive validation on both frontend and backend
- Pagination and debouncing for search

### Phase 4: Production Ready (Weeks 8-9)

**Deliverables**:
- Comprehensive test coverage
- Accessibility audit and fixes
- Performance optimization
- Documentation
- Developer tooling

**Critical Path**:
1. Unit tests (backend + frontend)
2. Integration tests (backend API)
3. E2E tests (Playwright)
4. Accessibility (keyboard, screen reader)
5. Performance audit (bundle, SSE, DB queries)

**Risks**:
- Test maintenance burden
- Accessibility gaps
- Performance bottlenecks

**Mitigation**:
- Focus tests on critical paths
- Use automated accessibility checkers (axe-core)
- Profile early, optimize incrementally

### Phase 5: Deployment (Week 10)

**Deliverables**:
- Packaged binaries
- Migration scripts
- Launch documentation
- Rollback plan

**Critical Path**:
1. Binary packaging (cargo build --release)
2. Frontend production build
3. Data migration testing
4. Launch script
5. Monitoring setup

**Risks**:
- Data migration failures
- Unexpected deployment issues
- Performance under real load

**Mitigation**:
- Test migration on copy of production data
- Gradual rollout (canary deployment)
- Robust logging and monitoring

---

## Risk Analysis

### Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Rust Learning Curve** | Medium | High | Start with small modules, extensive documentation, pair programming |
| **SQLx Async Complexity** | Medium | Medium | Use synchronous SQLite initially, migrate incrementally |
| **SSE Reconnection Bugs** | High | High | Reuse existing logic, comprehensive E2E tests, test with network throttling |
| **Process Supervision Edge Cases** | Medium | High | Test process crashes, SIGKILL scenarios, orphan cleanup |
| **Next.js App Router Learning** | Low | Medium | Follow official patterns, use Server Components judiciously |
| **Type Mismatch Frontend/Backend** | Medium | Medium | Code generation (typeshare) or strict manual sync, runtime validation |
| **Database Migration Failures** | Low | Critical | Test on copies, transactional migrations, rollback plan |
| **Performance Regression** | Medium | Medium | Profile early, benchmark against current, optimize hot paths |
| **Accessibility Gaps** | Medium | Medium | Automated checks (axe-core), manual testing, ARIA best practices |
| **Windows Compatibility** | High | Low | Unix-first, Windows later, document platform limitations |

### Product Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Feature Creep** | Medium | High | Strict scope control, defer enhancements to post-refactor |
| **User Workflow Disruption** | Low | High | Maintain UI consistency, gradual rollout, rollback ready |
| **Data Loss** | Low | Critical | Comprehensive backups, migration testing, no destructive operations |
| **Prolonged Timeline** | Medium | Medium | Weekly checkpoints, MVP approach, prioritize core features |

### Operational Risks

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **Deployment Complexity** | Low | Medium | Simple launch script, clear docs, automated checks |
| **Rollback Difficulty** | Low | High | Keep old implementation, feature flags, database compatibility |
| **Insufficient Testing** | Medium | High | Mandatory coverage thresholds, block merge on test failures |
| **Documentation Drift** | High | Low | Doc-as-code, update docs in same PR as code changes |

---

## Performance Considerations

### Backend Performance

**Current Node.js Baseline**:
- Session list: ~10ms (50 sessions)
- Turn creation: ~50ms (includes DB insert + process spawn)
- SSE streaming: ~1ms per event
- Memory: ~50MB baseline, +10MB per active turn

**Rust Performance Targets**:
- Session list: <5ms (10x faster DB queries with SQLx)
- Turn creation: <30ms (faster spawn with tokio)
- SSE streaming: <0.5ms per event
- Memory: ~20MB baseline, +5MB per active turn

**Optimizations**:
1. **Database**: Connection pooling (5 connections), prepared statements, indexed queries
2. **Process Management**: Reuse tokio runtime, bounded event queues (256 events)
3. **SSE**: Backpressure handling, subscriber limits, automatic cleanup
4. **Serialization**: Use `simd-json` for faster JSON parsing (optional)

### Frontend Performance

**Metrics**:
- First Contentful Paint (FCP): <1.5s
- Largest Contentful Paint (LCP): <2.5s
- Time to Interactive (TTI): <3s
- Total Blocking Time (TBT): <300ms

**Optimizations**:
1. **Server Components**: Static session list, policy banners
2. **Code Splitting**: Lazy load dialogs, command UI
3. **Bundle Size**: Tree-shake unused Radix components, minify production build
4. **Caching**: TanStack Query cache (5min TTL for sessions), HTTP cache headers
5. **Streaming**: React Suspense for async data, progressive rendering
6. **Virtualization**: Virtual scrolling for large timelines (>100 events)

### Database Performance

**Current Prototype**:
- WAL mode enabled
- No query optimization
- No indexes on foreign keys

**Optimized Schema**:
- Compound indexes: `(session_id, seq)`, `(turn_id, seq)`
- Covering indexes for common queries
- VACUUM on startup (compact deleted data)
- ANALYZE after migrations (update query planner stats)

**Query Optimization**:
```sql
-- Before: Full table scan
SELECT * FROM events WHERE session_id = ? AND turn_id = ?;

-- After: Index scan with covering index
CREATE INDEX events_turn_covering ON events(turn_id, seq, kind, text, created_at_ms);
SELECT seq, kind, text, created_at_ms FROM events 
WHERE turn_id = ? 
ORDER BY seq;
```

### Memory Management

**Backend**:
- Bounded event queues (256 events per turn)
- Subscriber limits (256 per turn)
- Periodic cleanup of completed turns (1h retention in memory)
- SQLite cache size: 2000 pages (~8MB)

**Frontend**:
- React key optimization (stable keys for list items)
- Unmount cleanup (close EventSource on unmount)
- Query cache limits (max 100 cached queries)
- Virtual scrolling for long timelines

---

## Developer Experience

### Local Development Setup

```bash
# Clone repo
git clone https://github.com/user/sylph-web.git
cd sylph-web

# Backend setup
cd backend
cargo build
cargo test

# Frontend setup
cd ../frontend
pnpm install
pnpm dev

# Run both (from root)
./scripts/dev.sh
```

**`scripts/dev.sh`**:
```bash
#!/bin/bash
set -e

# Start backend in background
cd backend
cargo watch -x run &
BACKEND_PID=$!

# Start frontend
cd ../frontend
pnpm dev

# Cleanup on exit
trap "kill $BACKEND_PID" EXIT
```

### Hot Reload

- **Backend**: `cargo-watch` restarts on `.rs` file changes
- **Frontend**: Next.js Turbopack hot module replacement

### Debugging

**Backend**:
```rust
// Use tracing for structured logging
use tracing::{info, warn, error};

#[tracing::instrument]
async fn create_session(session: Session) -> Result<Session, Error> {
    info!("Creating session: {:?}", session.id);
    // ...
}
```

**Frontend**:
- React DevTools for component inspection
- TanStack Query DevTools for cache inspection
- Chrome DevTools for network (SSE) debugging

### Environment Variables

```bash
# Backend (.env)
DATABASE_PATH=~/.agents/data/sylph/web/live.db
RUNNERS_DIR=~/.agents/skills/sylph/runners
TRACEDB_BIN=~/Workspace/trace-db/target/release/trace-db
PORT=4317
HOST=127.0.0.1
LOG_LEVEL=info

# Frontend (.env.local)
NEXT_PUBLIC_BACKEND_URL=http://127.0.0.1:4317
```

### Code Quality Tools

**Backend**:
```bash
cargo fmt          # Format code
cargo clippy       # Lint
cargo test         # Run tests
cargo bench        # Benchmarks
```

**Frontend**:
```bash
pnpm format        # Prettier
pnpm lint          # ESLint
pnpm test          # Vitest
pnpm test:e2e      # Playwright
pnpm typecheck     # TypeScript
```

### CI/CD Pipeline

```yaml
# .github/workflows/ci.yml
name: CI
on: [push, pull_request]

jobs:
  backend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cd backend && cargo test --all-features
      - run: cd backend && cargo clippy -- -D warnings

  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
      - run: cd frontend && pnpm install
      - run: cd frontend && pnpm typecheck
      - run: cd frontend && pnpm lint
      - run: cd frontend && pnpm test
      - run: cd frontend && pnpm build

  e2e:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Start backend
        run: cd backend && cargo run &
      - name: Run E2E tests
        run: cd frontend && pnpm test:e2e
```

---

## Deployment Considerations

### Binary Packaging

**Backend**:
```bash
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
# Produces: target/release/sylph-web-backend
```

**Frontend**:
```bash
cd frontend
pnpm build
# Produces: .next/ (standalone build)
```

### Launch Script

**`scripts/start.sh`** (macOS/Linux):
```bash
#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"

# Start backend
"$ROOT_DIR/backend/target/release/sylph-web-backend" &
BACKEND_PID=$!

# Wait for backend health
until curl -sf http://127.0.0.1:4317/api/v1/health > /dev/null; do
  sleep 0.5
done

# Start frontend
cd "$ROOT_DIR/frontend"
node .next/standalone/server.js &
FRONTEND_PID=$!

# Open browser
sleep 1
open http://localhost:3000

# Wait and cleanup
wait $BACKEND_PID $FRONTEND_PID
```

### LaunchAgent (macOS)

**`~/Library/LaunchAgents/com.sylph.web.plist`**:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.sylph.web</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/hsw/Workspace/sylph-web/scripts/start.sh</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/tmp/sylph-web.log</string>
  <key>StandardErrorPath</key>
  <string>/tmp/sylph-web.error.log</string>
</dict>
</plist>
```

### Health Checks

```bash
# Backend health
curl http://127.0.0.1:4317/api/v1/health

# Frontend health
curl http://localhost:3000/

# Full diagnostics
curl http://127.0.0.1:4317/api/v1/diagnostics | jq
```

### Monitoring & Logging

**Structured Logging (Backend)**:
```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into())
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();
    
    // ...
}
```

**Logs Location**:
- Backend: stdout (captured by LaunchAgent to `/tmp/sylph-web.log`)
- Frontend: stdout (captured to `/tmp/sylph-web-frontend.log`)
- Database: `~/.agents/data/sylph/web/live.db-wal`

**Crash Recovery**:
- LaunchAgent `KeepAlive` restarts on crash
- Database WAL ensures consistency
- Interrupted turns marked on restart

---

## Appendix: Key Differences from Current Implementation

| Aspect | Current (Node.js + Vanilla JS) | Refactored (Rust + Next.js) |
|--------|-------------------------------|----------------------------|
| **Backend Language** | JavaScript (Node.js 22+) | Rust (1.83+) |
| **Backend Framework** | Raw `node:http` | Axum + Tokio |
| **Database Access** | Synchronous `node:sqlite` | Async SQLx with compile-time checking |
| **Frontend Framework** | Vanilla JS + DOM | Next.js 15 + React 19 |
| **Type System** | None (JavaScript) | TypeScript strict + Rust |
| **State Management** | Global state object | TanStack Query + React state |
| **Styling** | Inline CSS in HTML | Tailwind CSS + CSS modules |
| **Build Tool** | None (served directly) | Turbopack (Next.js) + cargo |
| **Testing** | None | Vitest + Playwright + cargo test |
| **API Structure** | Mixed `/api/v1` and `/api` | Unified `/api/v1` with proxy |
| **SSE Implementation** | Manual EventSource + NDJSON | Axum SSE with backpressure |
| **Process Supervision** | Global Map + spawn | TurnSupervisor with bounded queues |
| **Error Handling** | Generic JSON errors | Structured error enums |
| **Security** | Cookie + host validation | Cookie + host + CSRF + input validation |
| **Performance** | ~50MB memory, ~10ms queries | ~20MB memory, <5ms queries (target) |
| **Developer Experience** | Manual restart | Hot reload (cargo-watch + Turbopack) |
| **Deployment** | Node.js required | Standalone binaries |

---

## Conclusion

This refactoring plan transforms Sylph Web from an early-stage prototype into a production-ready application with:

1. **Type Safety**: End-to-end type checking (TypeScript + Rust)
2. **Performance**: 2-5x faster with Rust backend and optimized database access
3. **Maintainability**: Clear separation of concerns, comprehensive tests
4. **Developer Experience**: Hot reload, structured logging, CI/CD
5. **Scalability**: Async runtime, bounded queues, connection pooling
6. **Security**: Layered defenses, input validation, secure defaults

The 10-week timeline is aggressive but achievable with focused execution. The monorepo structure keeps frontend and backend in sync, while the migration strategy ensures no data loss and provides a clear rollback path.

**Next Steps**:
1. Review and approve this specification
2. Set up development environment (Rust, Node.js, tooling)
3. Begin Phase 1 (Backend Core) implementation
4. Weekly progress reviews and adjustments

