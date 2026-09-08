# ADR 0002: Environment, Workspace, and Subject boundaries

- Status: Accepted
- Date: 2026-09-07

## Context

Sylph serves records produced by machines with different homes, credentials, native stores,
and filesystem permissions. A browser user is an access subject, not the owner of a session.
Using a user id or a directory as session identity would make imports, sharing, and reconnects
ambiguous.

## Decisions

- A **Subject** is the authenticated actor. The first implementation supports registered users;
  API tokens and service identities may be added without changing resource identity.
- An **Environment** is the durable identity of one envagent and its OS-user filesystem, home,
  credentials, native stores, and adapters. Different OS users are different environments.
- A **Workspace** is `(environment_id, canonical_absolute_path)`. Directory canonicalization,
  symlink resolution, and browse-root checks are performed by the envagent that owns the path.
- A **Session** keeps the identity `(environment, agent, native_id)`. User, workspace, and
  workdir are access or execution context and never replace that identity. Unknown native paths
  remain unknown; the server does not guess them.
- Initial authorization is environment-level: `subject → environment` through an explicit
  access grant. A grant covers all sessions, workspaces, native history, and available control
  in that environment. This is a deliberate trust boundary and must be disclosed at pairing.
- Pairing creates an environment and may grant the pairing subject access. It does not create an
  implicit owner or a second session namespace.

[ADR-0006](0006-environment-authority-and-capability-grants.md) refines the last two decisions
without changing session identity: it settles who may issue a grant, splits the grant into
`observe` and `control`, and reserves `administer` to the environment's authority root. The scope
of an environment grant stays as stated here; what varies is which capability it conveys.

## Consequences

Environment filtering becomes the common authorization boundary for sessions, search, workspaces,
health, files, and raw records. Directory-level ACLs and session owners are deferred until a
concrete requirement exists. The schema should retain `subject`, `capability`, and grant
provenance so that extension does not require changing session identity.

## Verification and rollback

The decision is verified by schema/API consistency checks and multi-environment authorization
tests. It can be rolled back before multi-user data exists by removing grant tables and restoring
the single-subject bootstrap middleware; session identity remains unchanged.
