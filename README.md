# Sylph Web

Sylph Web is the local native UI for Sylph and the coding-agent sessions archived by TraceDB. It can create Sylph-managed sessions, stream tool/reasoning events, send local attachments, search stable cross-agent history, and resume sessions originally created in Claude Code, Codex, OpenCode, or Pi without changing their identity policy.

## Architecture

**Frontend**: Next.js 16 + React 19 + TypeScript (in development)  
**Backend**: Rust + Axum + SQLx + Tokio (Phase 1 in progress)  
**Database**: SQLite via SQLx (live sessions) + TraceDB (stable history)

## Development

Requirements: 
- Rust 1.85+
- Node.js 22.5+ (for Next.js frontend, coming in Phase 2)
- TraceDB built from `~/Workspace/trace-db`
- Sylph runners under `~/.agents/skills/sylph/runners`

### Backend

```sh
cd backend
cargo build
cargo run
```

The backend runs on `http://127.0.0.1:4317`.

### Frontend

_Next.js frontend coming in Phase 2 (Week 4-5)._

## Storage

Runtime data is stored under `~/.agents/data/sylph/web/`.

Environment variables:

- `SYLPH_WEB_HOST` and `SYLPH_WEB_PORT`
- `SYLPH_WEB_DATA_DIR`
- `SYLPH_RUNNERS_DIR`
- `TRACEDB_BIN`

## Documentation

- [Architecture](docs/architecture.md) — storage ownership and session invariants
- [Product design](docs/product.md) — release roadmap
- [Refactor specifications](docs/refactor-specs.md) — greenfield refactor details
- [API specification](docs/api-v1.openapi.json) — REST + SSE endpoints

Every commit uses Conventional Commits with an English subject and body.
