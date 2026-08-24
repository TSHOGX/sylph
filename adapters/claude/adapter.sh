#!/usr/bin/env bash
# Claude adapter entry point for `sylph.agent.v3`.
#
# A shell shim over `adapter.py` for the same reason the v1 claude runner had
# one: the protocol work needs real JSON handling and a concurrent read loop,
# which bash does badly, but the discovery contract is a script at a known path.
#
# Held to bash 3.2, which is what macOS ships.
set -euo pipefail
exec python3 "$(dirname "$0")/adapter.py" "$@"
