# AGENTS.md

Sylph Web is a local-first native interface for Sylph and the coding-agent sessions archived by TraceDB.

## Product invariants

- Treat session origin as durable data. `sylph` sessions use Sylph identity and lifecycle policy; `native` sessions preserve the originating agent's identity and lifecycle.
- Never inject `SOUL.md`, reset, compact, or rewrite prompts for a native session unless the user explicitly converts it to a Sylph session.
- Read durable coding-agent history through TraceDB interfaces. Do not query TraceDB's SQLite schema directly.
- The live journal is authoritative only while a WebApp turn is active or has not yet appeared in TraceDB. TraceDB is authoritative for stable history.
- Bind to loopback by default. Any non-loopback deployment requires a separate authentication and threat-model change.

## Development

**Backend (Rust)** — Phase 1 in progress:

```sh
cd backend
cargo run
```

**Frontend (Next.js)** — Phase 2 starts week 4:

```sh
cd frontend
npm run dev  # Coming soon
```

Rust 1.85+ is required. The backend uses SQLx for compile-time SQL verification and Axum for HTTP/SSE.

Every commit uses Conventional Commits with an English subject and body.
