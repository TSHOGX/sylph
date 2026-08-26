# Sylph Web Backend

Rust backend for Sylph Web, providing REST API and SSE streaming for coding agent sessions.

## Architecture

- **Framework**: Axum 0.8 + Tokio 1.44
- **Database**: SQLite with SQLx (runtime queries; see the note in `AGENTS.md`)
- **Port**: 4317 (127.0.0.1 only)
- **Data**: `~/.sylph/data/journal.db`

## Build

```bash
cargo build --release
```

## Run

```bash
cargo run --release
```

Subcommands:

```bash
cargo run -- doctor                    # what configuration resolved to
cargo run -- config init               # create a missing config.toml or SOUL.md
cargo run -- config path               # print the config file's path
cargo run -- migrate --from DIR        # bring an older data directory in
```

## Configuration

Everything lives under `~/.sylph`, configured by `~/.sylph/config.toml`, which is
created fully commented on first start. `docs/configuration.md` is authoritative.

For a throwaway instance, one variable is enough:

```bash
SYLPH_HOME=/tmp/sylph-scratch SYLPH_PORT=8080 cargo run --release
```

## Development

```bash
# Watch for changes and rebuild
cargo watch -x run

# Run tests
cargo test

# Check code
cargo check
cargo clippy
cargo fmt
```

## Environment Variables

An environment variable overrides `config.toml`, which overrides a default derived
from the root. The full table is in `docs/configuration.md`; the ones used most:

| Variable | Default | Description |
|----------|---------|-------------|
| `SYLPH_HOME` | `~/.sylph` | The root holding config, identity, data, and scratch |
| `SYLPH_PORT` | `4317` | Port number |
| `SYLPH_HOST` | `127.0.0.1` | Bind address. Non-loopback needs an auth change first |
| `SYLPH_LOG`, then `RUST_LOG` | `info` | Log filter |

## API Endpoints

### Health
- `GET /api/v1/health` - Health check
- `GET /api/v1/diagnostics` - System diagnostics

### Sessions
- `GET /api/v1/sessions` - List sessions (query: `limit=50`)
- `POST /api/v1/sessions` - Create Sylph session
- `GET /api/v1/sessions/{id}` - Get session detail

### Turns (TODO)
- `GET /api/v1/sessions/{id}/turns` - List turns
- `POST /api/v1/sessions/{id}/turns` - Create turn
- `GET /api/v1/turns/{id}` - Get turn detail
- `GET /api/v1/turns/{id}/events` - Stream events (SSE)
- `POST /api/v1/turns/{id}/stop` - Stop turn

### Agents (TODO)
- `GET /api/v1/agents` - List discovered agents
- `POST /api/v1/agents/refresh` - Refresh agent discovery

## Database Schema

SQLite database with tables:
- `app_meta` - Metadata
- `sessions` - Session records
- `backend_incarnations` - Session lifecycle tracking
- `turns` - Turn records
- `events` - Event stream storage

Migrations are in `migrations/` and run automatically on startup.

## Project Structure

```
backend/
├── Cargo.toml              # Dependencies
├── migrations/             # SQL migrations, plus migrations/history for history.db
├── src/
│   ├── main.rs            # Server entry point and subcommands
│   ├── lib.rs             # Router, AppState, module tree
│   ├── config/            # Resolution, the value object, and what to report
│   ├── error.rs           # Error types and their published codes
│   ├── paths.rs           # Path containment, shared by every fs endpoint
│   ├── attachments.rs     # Staging uploaded files for adapters to read
│   ├── api/               # HTTP handlers
│   ├── models/            # Domain models
│   ├── db/                # The journal: unsettled turns and their sessions
│   ├── storage/           # Durable history: folds, blobs, retention
│   ├── adapter/           # The sylph.agent.v3 wire protocol
│   ├── agents/            # Adapter discovery and handshake
│   ├── links/             # The pool of live adapter processes
│   ├── supervisor/        # Turn supervision
│   ├── events/            # Event normalization and the SSE hub
│   ├── env/               # Where a session's work runs
│   ├── documents/         # Spreadsheet, document, and slide parsing
│   ├── tracedb/           # TraceDB client
│   └── middleware/        # Auth middleware
└── tests/                 # Integration tests
```

There is no `runners/`. What the v1 runner protocol did in one module is now four,
because a long-lived adapter process has a lifecycle a one-shot subprocess did not:
`adapter/` speaks the wire format, `agents/` finds installed adapters and learns what
each one can do by handshaking with it, `links/` owns the live processes, and
`supervisor/` drives a turn across one. `docs/session-protocol.md` is authoritative
for the protocol itself.

## Testing

```bash
# Unit tests
cargo test

# Integration tests (TODO)
cargo test --test '*'

# With output
cargo test -- --nocapture
```

## Implementation Status

### ✅ Completed
- Project structure and dependencies
- Database layer with SQLx and migrations
- Session API (list, create, get)
- Health and diagnostics endpoints
- Structured error handling
- Configuration loading

### 🚧 In Progress (Phase 1)
- Turn management API
- Runner discovery and execution
- SSE streaming
- Authentication middleware

### 📋 TODO (Later Phases)
- Native session import
- Attachment handling
- TraceDB integration
- Comprehensive tests
- Performance optimization

## License

Private project
