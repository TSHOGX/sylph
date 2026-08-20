#!/bin/sh
# Deterministic runner for end-to-end tests.
#
# Speaks the `sylph.runner.v1` NDJSON protocol on stdout: one event per line,
# then a final envelope line. Real agents are non-deterministic and need
# credentials, so E2E uses this instead and still exercises the production
# streaming path end to end.
#
# The tool_call/tool_result events matter as much as the prose ones: the Phase 6
# timeline renders tool calls as its primary content, derives diffs from edit
# arguments, and decides expansion from failure status. None of that is reachable
# without a runner that emits the shapes a real agent does, so the payloads below
# mirror what `runners/claude` and `runners/codex` actually produce — tool input
# under `data.input`, with the same JSON in `text`.

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

# The prompt arrives on stdin. Reading it lets one runner serve several
# scenarios, which is cheaper than maintaining a second fixture runner.
PROMPT=$(cat 2>/dev/null || true)

emit '{"stream":"sylph.runner.v1","event":{"kind":"status","phase":"started","text":"starting"}}'
emit '{"stream":"sylph.runner.v1","event":{"kind":"thinking","phase":"completed","id":"blk-think","text":"Considering the request."}}'

case "$PROMPT" in
  *bulk*)
    # A long transcript, for measuring render cost. Emitted as completed blocks
    # so every one is journalled and survives a reload, which is what makes the
    # measurement reproducible rather than dependent on stream timing.
    i=1
    while [ "$i" -le 250 ]; do
      emit "{\"stream\":\"sylph.runner.v1\",\"event\":{\"kind\":\"assistant\",\"phase\":\"completed\",\"id\":\"blk-a$i\",\"text\":\"Step $i: inspecting the module and its callers.\"}}"
      emit "{\"stream\":\"sylph.runner.v1\",\"event\":{\"kind\":\"tool_call\",\"phase\":\"completed\",\"id\":\"blk-t$i\",\"name\":\"Read\",\"text\":\"{\\\"file_path\\\":\\\"src/mod_$i.rs\\\"}\",\"data\":{\"input\":{\"file_path\":\"src/mod_$i.rs\"}}}}"
      emit "{\"stream\":\"sylph.runner.v1\",\"event\":{\"kind\":\"tool_result\",\"phase\":\"completed\",\"id\":\"blk-t$i\",\"name\":\"Read\",\"text\":\"pub fn helper_$i() -> usize { $i }\"}}"
      emit "{\"stream\":\"sylph.runner.v1\",\"event\":{\"kind\":\"thinking\",\"phase\":\"completed\",\"id\":\"blk-th$i\",\"text\":\"Module $i looks consistent with the others.\"}}"
      i=$((i + 1))
    done
    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"blk-reply","text":"Hello from the E2E runner."}}'
    ;;
  *tools*)
    # A read, an edit with a real diff, and a failing shell command: the three
    # renderings whose expansion policy differs.
    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_call","phase":"completed","id":"blk-read","name":"Read","text":"{\"file_path\":\"src/lib.rs\"}","data":{"input":{"file_path":"src/lib.rs"}}}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_result","phase":"completed","id":"blk-read","name":"Read","text":"fn main() {\n    println!(\"before\");\n}"}}'

    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"blk-plan","text":"I will replace the placeholder with **real** logging."}}'

    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_call","phase":"completed","id":"blk-edit","name":"Edit","text":"{\"file_path\":\"src/lib.rs\",\"old_string\":\"    println!(\\\"before\\\");\",\"new_string\":\"    tracing::info!(\\\"after\\\");\"}","data":{"input":{"file_path":"src/lib.rs","old_string":"fn main() {\n    println!(\"before\");\n}","new_string":"fn main() {\n    tracing::info!(\"after\");\n}"}}}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_result","phase":"completed","id":"blk-edit","name":"Edit","text":"Applied 1 edit."}}'

    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_call","phase":"completed","id":"blk-bash","name":"Bash","text":"{\"command\":\"cargo test --lib\"}","data":{"input":{"command":"cargo test --lib"}}}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"tool_result","phase":"completed","id":"blk-bash","name":"Bash","text":"error[E0425]: cannot find value `after` in this scope","isError":true,"data":{"exitCode":101}}}'

    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"blk-reply","text":"Hello from the E2E runner."}}'
    ;;
  *)
    # Stream the reply as deltas so the test sees incremental rendering rather
    # than a single finished block.
    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"started","id":"blk-reply"}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"delta","id":"blk-reply","text":"Hello from "}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"delta","id":"blk-reply","text":"the E2E runner."}}'
    emit '{"stream":"sylph.runner.v1","event":{"kind":"assistant","phase":"completed","id":"blk-reply","text":"Hello from the E2E runner."}}'
    ;;
esac

emit '{"stream":"sylph.runner.v1","event":{"kind":"usage","phase":"completed","data":{"inputTokens":12,"outputTokens":7}}}'

# The final non-empty stdout line is the authoritative outcome.
emit '{"ok":true,"text":"Hello from the E2E runner.","session_id":"e2e-native-session"}'
