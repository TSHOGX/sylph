# Sylph Web

Local-first native UI for Sylph and coding-agent sessions archived by TraceDB.

Sylph Web can create Sylph-managed sessions, stream live tool/reasoning events, send local attachments, search stable cross-agent history, and resume sessions originally created in Claude Code, Codex, OpenCode, or Pi without changing their identity policy.

## Architecture

- **Backend:** Rust + Axum + SQLx + Tokio (Phase 1 in progress)
- **Frontend:** Next.js 16 + React 19 + TypeScript (Phase 2 starts week 4)
- **Database:** SQLite (live sessions) + TraceDB (stable history)
- **API:** REST + SSE streaming on `http://127.0.0.1:4317`

## Quick Start

### Prerequisites

- Rust 1.85+
- Node.js 22.5+ (for frontend, Phase 2)
- TraceDB built from `~/Workspace/trace-db`
- Sylph runners under `~/.agents/skills/sylph/runners`

### Development

**Backend:**

```sh
cd backend
cargo run
```

The backend runs on `http://127.0.0.1:4317`. Check health:

```sh
curl http://127.0.0.1:4317/api/v1/health
```

**Frontend (coming Phase 2):**

```sh
cd frontend
npm install
npm run dev
```

### Environment Variables

- `SYLPH_WEB_HOST` — bind address (default: `127.0.0.1`)
- `SYLPH_WEB_PORT` — port (default: `4317`)
- `SYLPH_WEB_DATA_DIR` — data directory (default: `~/.agents/data/sylph/web`)
- `SYLPH_RUNNERS_DIR` — runners directory (default: `~/.agents/skills/sylph/runners`)
- `TRACEDB_BIN` — TraceDB binary path (default: `~/Workspace/trace-db/target/release/trace-db`)

## Documentation

- [**AGENTS.md**](AGENTS.md) — Development guide and project rules
- [**Architecture**](docs/architecture.md) — System design and data flow
- [**Roadmap**](docs/roadmap.md) — Release phases and milestones
- [**API Design**](docs/api-design.md) — REST endpoints and SSE streaming
- [**Database Schema**](docs/database-schema.md) — SQLite schema and migrations
- [**Frontend Architecture**](docs/frontend-architecture.md) — UI design and implementation (Phase 2)

## Project Status

**Phase 1 (in progress):** Rust backend core
- ✅ Database layer with migrations
- ✅ Session management API
- ✅ Health and diagnostics
- 🚧 Turn management and SSE streaming
- 🚧 Runner discovery and process supervision
- 🚧 Authentication middleware

**Phase 2 (week 4-5):** Next.js frontend foundation

**Phase 3 (week 6-7):** Feature completion (attachments, search, commands)

**Phase 4 (week 8-9):** Testing and accessibility

**Phase 5 (week 10):** Deployment and packaging

See [roadmap](docs/roadmap.md) for details.

## Contributing

Read [AGENTS.md](AGENTS.md) for development guidelines.

**Key rules:**
- Code, docs, commits: English only
- Conventional Commits with detailed messages
- Never `git add -A` — stage files explicitly
- Update specs before changing design
- Test before committing

## License

Private project
