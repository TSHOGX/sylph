# ADR 0005: Lossless raw records with an extensible retention policy

- Status: Accepted
- Date: 2026-09-07

## Decision

The server retains vendor raw records and large sidecars by default without automatic deletion,
encryption, or redaction. This is a default retention policy, not an irrevocable promise that an
administrator can never delete data. Raw records are the Sylph-side source for re-projection,
diagnostics, and schema evolution; a vendor native store is not a reliable backup.

The unified trace foundation extends this rule to every admitted observation, including
live-only deltas and clocks. Rebuildable projections may be reclaimed; native-store
watermarks cannot authorize evidence deletion. Large bodies use content-addressed chunks
and immutable manifests. [The storage contract](../unified-trace-storage.md#retention-deletion-and-garbage-collection)
owns retention, explicit deletion epochs and shared-payload garbage collection.

Raw may contain prompts, tool input/output, paths, project content, and vendor metadata. Pairing
and sharing surfaces must disclose that an environment grant exposes this data to every authorized
subject. Management UI, deletion workflows, and compliance policy are deferred.

## Verification

Storage tests must prove raw round-trip, blob reference integrity, and that retention cannot prune
raw classes as deltas. Future deletion must define audit, index rebuild, and re-projection behavior.
