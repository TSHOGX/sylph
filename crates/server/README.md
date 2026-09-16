# Sylph server

The server owns operational policy in `control.db`, immutable admitted trace evidence in
`evidence.db`, and rebuildable generation/search databases. Every vendor process and native
filesystem read runs through an outward-connected envagent. There is no in-process agent
execution path.

The repository [documentation index](../../docs/README.md) routes design and API questions.
[Architecture](../../docs/architecture.md) owns identity and authority;
[trace storage](../../docs/unified-trace-storage.md) owns admission, publication and reads;
[database schema](../../docs/database-schema.md) owns database layout.

Run from the repository root:

```sh
cargo run -p sylph-server
cargo test --workspace
cargo clippy --workspace --all-targets
cargo doc --workspace --no-deps
```

Start and pair an envagent before requesting execution. See
[development](../../docs/development.md) and [configuration](../../docs/configuration.md).

`src/db/` holds Subject/grant/session metadata, immutable command inputs and the operational
outbox. `src/env/` owns the authenticated bridge and control dispatch. `src/trace/` holds
capture upload admission, payload CAS, projection workers, generation publication, snapshot
leases, bounded reading projections, search and collection. Pure vendor mapping and reduction
are in the separate `sylph-trace` crate. `src/api/` exposes authenticated HTTP/SSE routes.

Initial SQL schemas live beside their owning stores. Schema churn is a deliberate hard
break; no dual reader, compatibility migration or hot/cold semantic tier exists.
