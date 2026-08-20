#!/bin/sh
# Deterministic runner for end-to-end tests.
#
# Speaks the `sylph.runner.v1` NDJSON protocol on stdout: one event per line,
# then a final envelope line. Real agents are non-deterministic and need
# credentials, so E2E uses this instead and still exercises the production
# streaming path end to end.

set -eu

if [ "${1:-}" = "--describe" ]; then
  cat <<'JSON'
{
  "name": "e2e",
  "description": "Deterministic E2E runner",
  "session_key": "resume",
  "flags": {
    "model": { "choices": ["fast", "thorough"], "description": "Model tier" }
  },
  "commands": {
    "status": { "description": "Report runner status", "args": {} }
  }
}
JSON
  exit 0
fi

emit() { printf '%s\n' "$1"; }

emit '{"stream":"sylph.runner.v1","event":{"kind":"status","phase":"started","text":"starting"}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"thinking","phase":"completed","id":"blk-think","text":"Considering the request."}}'

# Stream the reply as deltas so the test sees incremental rendering rather than
# a single finished block.
emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"started","id":"blk-reply"}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"delta","id":"blk-reply","text":"Hello from "}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"delta","id":"blk-reply","text":"the E2E runner."}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"blk-reply","text":"Hello from the E2E runner."}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"usage","phase":"completed","data":{"inputTokens":12,"outputTokens":7}}}'

# The final non-empty stdout line is the authoritative outcome.
emit '{"ok":true,"text":"Hello from the E2E runner.","session_id":"e2e-native-session"}'
