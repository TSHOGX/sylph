#!/usr/bin/env bash
# Credential scan for a candidate fixture before it is copied under fixtures/native/.
#
# Run this on EVERY extraction, not once. The agent session stores on a developer
# machine accumulate whatever their transcripts happened to print, so a store that was
# clean last month is not clean now — measured 2026-08-28, 695 files under
# ~/.codex/sessions/ and 3 under ~/.claude/projects/ carry a live Postgres password
# inside an injected "Approved command prefixes" list, which `world_state` snapshots
# then duplicate. That is a property of this installation, not of one session.
#
# Two rules about fixing a hit:
#
#   - Delete the whole line. Never redact a substring: a partial edit leaves a record
#     whose shape is a lie, and a fixture's whole value is being byte-for-byte what the
#     vendor wrote.
#   - Prefer choosing a different session over editing one, because an edited fixture
#     cannot be re-derived from the store and so nobody can check it later.
#
# Implementation notes, both of which are bugs this script already had:
#
#   - **No `grep -P`.** BSD grep on macOS does not support it and *exits nonzero*, which
#     an `if grep …; then` reads as "no matches" — so the scan reported every file clean
#     while checking nothing. Python does the matching instead, so one interpreter
#     behaves the same on every machine.
#   - **`sk-` needs more than a prefix.** The literal tag `<task-notification>` contains
#     the substring `sk-notification`, which a bare `sk-[A-Za-z0-9-]+` flagged on two
#     real fixtures. A provider key is followed by a random string rather than an English
#     word, so requiring a digit in it separates the two.
#
# Two known false positives, both investigated and both left in place deliberately,
# because the key-name patterns below are worth more than the noise they make:
#
#   - `{"type": "apiKey"}` in a codex capture is an *auth-method enum value* — the
#     account logs in with an API key — not a key. `docs/measurements/codex/captures/`
#     carries it on `s01_initialize.jsonl`.
#   - `"apiKey": {"type": "string"}` in codex's generated JSON Schema is a type
#     declaration with no value at all.
#
# A hit on a key *name* is a prompt to look, not a verdict. Confirm whether a value sits
# beside it before deleting anything.
#
# Exits nonzero and prints matches when something looks like a secret; exits nonzero if
# it cannot scan at all, because a scanner that fails open is worse than none.
set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: credscan.sh <file>" >&2
  exit 2
fi

exec python3 - "$1" <<'PY'
import re
import sys

PATTERNS = [
    # Provider keys: prefix plus a random-looking body. The digit requirement is what
    # keeps `sk-notification` out.
    (r"sk-(?=[A-Za-z0-9_-]*[0-9])[A-Za-z0-9_-]{16,}", "provider key"),
    (r"ghp_[A-Za-z0-9]{20,}", "github token"),
    (r"ghs_[A-Za-z0-9]{20,}", "github token"),
    (r"github_pat_[A-Za-z0-9_]{20,}", "github token"),
    (r"AKIA[A-Z0-9]{12,}", "aws key id"),
    (r"ASIA[A-Z0-9]{12,}", "aws key id"),
    (r"Bearer [A-Za-z0-9._-]{10,}", "bearer token"),
    (r"-----BEGIN [A-Z ]*PRIVATE KEY", "private key"),
    # Database credentials this machine's stores are known to carry.
    (r"PGPASSWORD", "postgres password"),
    (r"POSTGRES_PASSWORD", "postgres password"),
    (r"MYSQL_PWD", "mysql password"),
    (r"postgres\.your-tenant", "postgres connection string"),
    # Key-shaped JSON members.
    (r"authtoken:", "auth token"),
    (r'"authorization"', "authorization header"),
    (r'"api_key"', "api key"),
    (r'"apiKey"', "api key"),
    (r'"access_token"', "access token"),
]

COMPILED = [(re.compile(p), label) for p, label in PATTERNS]
path = sys.argv[1]
hits = 0

with open(path, encoding="utf-8", errors="replace") as handle:
    for lineno, line in enumerate(handle, 1):
        for pattern, label in COMPILED:
            match = pattern.search(line)
            if match:
                hits += 1
                print(f"{path}:{lineno}: {label}: ...{match.group(0)[:48]}...")
                break

if hits:
    print(f"credscan: {hits} suspect line(s) in {path} — do not commit it", file=sys.stderr)
    sys.exit(1)

print(f"credscan: {path} clean", file=sys.stderr)
PY
