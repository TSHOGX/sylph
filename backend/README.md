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
├── migrations/             # SQL migrations
├── src/
│   ├── main.rs            # Server entry point
│   ├── config.rs          # Configuration
│   ├── error.rs           # Error types
│   ├── api/               # HTTP handlers
│   ├── models/            # Domain models
│   ├── db/                # Database layer
│   ├── runners/           # Runner management
│   ├── tracedb/           # TraceDB client
│   └── middleware/        # Auth middleware
└── tests/                 # Integration tests
```

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
