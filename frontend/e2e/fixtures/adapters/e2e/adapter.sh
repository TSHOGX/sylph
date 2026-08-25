#!/usr/bin/env bash
# E2E fixture agent entry point for `sylph.agent.v3`.
#
# Discovery looks for `adapter.sh`, so this file existing is what makes the fixture
# an agent at all. Its absence is why the suite ran against `Agent discovery: 0
# usable` after the v1 runner path was deleted.
set -euo pipefail
exec python3 "$(dirname "$0")/adapter.py" "$@"
