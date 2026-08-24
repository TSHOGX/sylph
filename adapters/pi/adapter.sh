#!/usr/bin/env bash
# pi adapter entry point for `sylph.agent.v3`.
#
# A shell shim over `adapter.py`, for the same reason the claude adapter has one:
# the protocol needs real JSON handling and a concurrent read loop, which bash does
# badly, while the discovery contract is an executable at a known path.
#
# Held to bash 3.2, which is what macOS ships.
set -euo pipefail
exec python3 "$(dirname "$0")/adapter.py" "$@"
