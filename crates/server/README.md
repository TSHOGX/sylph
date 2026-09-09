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

The complete, machine-checked contract is [`docs/api-v1.openapi.json`](../../docs/api-v1.openapi.json).
The most important surfaces are:

- `GET /api/v1/health`, `/diagnostics`, `/config`, `/environments`, and `/soul`
- `GET /api/v1/sessions` and `GET /api/v1/sessions/{id}` for the one durable session
  collection
- `POST /api/v1/sessions` to start an ephemeral draft; the draft is promoted by
  `POST /api/v1/sessions/draft/{id}/turns` after the first real turn
- `GET /api/v1/sessions/{id}/turns` and `POST /api/v1/sessions/{id}/turns` for durable
  history and subsequent turns
- `GET /api/v1/sessions/{id}/trace/events` and turn-scoped SSE for live replay
- `GET/POST /api/v1/sessions/{id}/capabilities`, `/link`, `/requests`, and `/config`
  for the adapter-owned control surface
- `POST /api/v1/sessions/adopt` and `GET /api/v1/sessions/enumerate` for explicit
  adoption from an environment's native store
- Session-scoped file/document routes and machine-scoped `/api/v1/fs/*` browsing
- `GET /api/v1/search` and `GET /api/v1/workspaces` over the server-owned store

## Database Schema

SQLite database with tables:
- `app_meta` - Metadata
- `sessions` - Durable records keyed by `(environment, agent, native_id)`
- `turns` - Turn records, including the user/agent origin
- `events` - The journal tail for live and recently settled turns

Consolidated blocks, content-addressed payloads, and the full-text index live in the
separate `history.db` and `blobs/` paths under the same data directory. There is no
incarnation table, surrogate session id, or second archive database.

Migrations are in `migrations/` and run automatically on startup.

## Project Structure

```
crates/server/
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
│   ├── links/             # The pool of live adapter sessions
│   ├── supervisor/        # Turn supervision
│   ├── events/            # Event normalization and the SSE hub
│   ├── env/               # Where a session's work runs
│   ├── documents/         # Spreadsheet, document, and slide parsing
│   └── middleware/        # Auth middleware
└── tests/                 # Integration tests
```

Vendor-specific process and native-history handling lives in `crates/adapters`; the
backend only consumes the protocol types from `crates/protocol`. `links/` owns one live
adapter session, `supervisor/` settles turns, and `storage/` is the durable authority.
`docs/session-protocol.md` is authoritative for the protocol itself.

## Testing

```bash
# Unit tests
cargo test

# Integration tests
cargo test --test integration

# With output
cargo test -- --nocapture
```

The implementation roadmap and remaining work are maintained in
[`docs/roadmap.md`](../../docs/roadmap.md), rather than duplicated here.

## License

Private project
