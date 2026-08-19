# Legacy Code Cleanup Summary

**Date:** 2026-08-19  
**Branch:** main  
**Commit:** b5892a1

## Context

Sylph Web is undergoing a greenfield refactor from a Node.js + vanilla JavaScript prototype to a modern architecture with:
- **Backend:** Rust + Axum + SQLx + Tokio
- **Frontend:** Next.js 16 + React 19 + TypeScript (Phase 2, starting week 4)

Phase 1 (Rust backend core) is currently in progress, implemented by other agents.

## Files Removed

### Node.js Backend (`src/`)
- `config.mjs` — Environment configuration
- `migrations.mjs` — SQLite migration system
- `runners.mjs` — Agent runner discovery
- `server.mjs` — Express/Node HTTP server (27KB)
- `session-policy.mjs` — Session lifecycle policy
- `store.mjs` — Direct node:sqlite access (16KB)
- `tracedb.mjs` — TraceDB client
- `turn-supervisor.mjs` — Process supervision (10KB)
- `api-types.d.ts` — TypeScript declarations

**Total:** ~9 files, ~500 LOC

### Test Suite (`test/`)
- `api-v1.test.mjs`
- `server.test.mjs`
- `session-policy.test.mjs`
- `store.test.mjs`
- `tracedb.test.mjs`

**Total:** 5 test files

### Vanilla JS Frontend (`public/`)
- `app.js` — Client-side JavaScript (22KB)
- `index.html` — HTML template (5KB)
- `styles.css` — CSS styles (12KB)

**Total:** ~500 LOC frontend code

### Build Configuration
- `package.json` — Node.js scripts and dependencies

## Documentation Updated

### README.md
- ✅ Removed references to `npm start` and `node:sqlite`
- ✅ Added Rust backend development instructions
- ✅ Documented Next.js frontend as "Phase 2 (coming week 4-5)"
- ✅ Listed new architecture: Rust + Axum + SQLx

### AGENTS.md
- ✅ Replaced `npm start` with `cargo run`
- ✅ Updated development section for Rust workflow
- ✅ Removed Node.js 22.5 requirement note

### docs/architecture.md
- ✅ Removed reference to `src/api-types.d.ts`
- ✅ Updated to reflect Rust backend type generation

### docs/product.md
- ✅ Restructured release slices from "Slice 1-4" to "Phase 1-5"
- ✅ Added "Legacy implementation (removed)" section
- ✅ Clarified Phase 1 (backend) vs Phase 2 (frontend) timeline
- ✅ Marked old "Implemented" features as part of removed prototype

### .gitignore
- ✅ Added Rust-specific ignores (`/backend/target/`)
- ✅ Added Next.js ignores for future Phase 2 (`/frontend/.next/`, etc.)
- ✅ Kept relevant ignores (SQLite, data/, node_modules)

### backend/.gitignore
- ✅ Created with `/target/` to prevent committing Rust build artifacts

## Current Repository State

```
sylph-web/
├── .gitignore
├── AGENTS.md
├── README.md
├── backend/                  ← Phase 1 (Rust, in progress)
│   ├── Cargo.toml
│   ├── migrations/
│   ├── src/
│   │   ├── main.rs
│   │   ├── api/
│   │   ├── db/
│   │   ├── models/
│   │   └── ...
│   └── tests/
└── docs/
    ├── adr/
    ├── api-v1.openapi.json
    ├── architecture.md
    ├── product.md
    ├── refactor-specs.md      ← 2092-line refactor specification
    └── refactor-summary-zh.md
```

## What Was Preserved

### Documentation
- ✅ `docs/refactor-specs.md` — Complete 2092-line refactor specification
- ✅ `docs/refactor-summary-zh.md` — Chinese refactor summary
- ✅ `docs/architecture.md` — Product architecture and boundaries
- ✅ `docs/product.md` — Release roadmap
- ✅ `docs/api-v1.openapi.json` — API specification
- ✅ `docs/adr/0001-runtime-boundaries.md` — ADR for durable execution model

### Backend (Rust)
- ✅ Complete Phase 1 implementation in `backend/`
- ✅ API endpoints: health, diagnostics, sessions
- ✅ Database layer with SQLx migrations
- ✅ Domain models and error handling

## Impact on Development

### For Backend Team (Phase 1)
- ✅ No more confusion from legacy `.mjs` files
- ✅ Clean Rust-only backend directory
- ✅ Clear documentation in `backend/README.md`

### For Frontend Team (Phase 2, starts week 4)
- ✅ No legacy vanilla JS to work around
- ✅ Clean slate for Next.js implementation
- ✅ Clear specs in `docs/refactor-specs.md`

### For Documentation
- ✅ All references updated to reflect Rust + Next.js architecture
- ✅ No misleading references to removed code
- ✅ Clear phase-based roadmap

## Next Steps

The cleanup is complete. Other agents can now continue Phase 1 implementation without confusion from legacy code:

1. **Phase 1 (continuing):** Turn management, runner discovery, SSE streaming
2. **Phase 2 (week 4-5):** Next.js frontend setup
3. **Phase 3 (week 6-7):** Feature alignment (attachments, commands, search)
4. **Phase 4 (week 8-9):** Testing and accessibility
5. **Phase 5 (week 10):** Deployment and packaging

## Verification

```bash
# No .mjs files remain
find . -name "*.mjs" ! -path "*/node_modules/*" ! -path "*/.git/*"
# (returns nothing)

# No legacy frontend
ls public/
# ls: public/: No such file or directory

# Backend structure intact
tree -L 2 backend/
# Shows clean Rust project structure

# Documentation updated
grep -l "npm start\|node:sqlite\|src/.*\.mjs" *.md docs/*.md
# (returns nothing)
```

All legacy code removed, documentation updated, repository ready for continued Phase 1 work.
