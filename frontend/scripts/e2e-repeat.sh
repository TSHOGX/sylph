#!/usr/bin/env bash
#
# Run the E2E suite N times and report which specs failed in each run.
#
#   scripts/e2e-repeat.sh [runs]        # default 4
#
# The question this answers is "is a failure a defect or is it flake?", and a
# single run cannot answer it: a defect fails the same spec every time, while
# contention fails whichever spec happens to be nearest its timeout budget.
#
# `.e2e-data/` is wiped before every run, deliberately. The journal is never
# cleared otherwise, and each spec's `mkdtemp` workdir becomes a permanent sidebar
# group — so run 4 would render hundreds of dead groups that run 1 did not, and the
# runs would not be comparable. Wiping is free: the directory is gitignored and the
# backend recreates `live.db` on start.
#
# Results go to /tmp/e2e-repeat/, one JSON per run plus a summary.

set -uo pipefail
cd "$(dirname "$0")/.."

RUNS="${1:-4}"
OUT=/tmp/e2e-repeat
mkdir -p "$OUT"
rm -f "$OUT"/run-*.json "$OUT"/run-*.log "$OUT"/summary.txt

export PATH="$HOME/.cargo/bin:$PATH"

for i in $(seq 1 "$RUNS"); do
  echo "=== run $i/$RUNS starting $(date +%H:%M:%S) — load $(uptime | sed 's/.*averages*: //') ===" \
    | tee -a "$OUT/summary.txt"

  # Comparable starting state for every run.
  rm -rf .e2e-data

  # Any orphaned server from a previous iteration would make `reuseExistingServer:
  # false` fail the whole run rather than one spec.
  pkill -f "next start -p 3101" 2>/dev/null
  pkill -f "target/debug/sylph-web-backend" 2>/dev/null
  sleep 2

  PLAYWRIGHT_JSON_OUTPUT_NAME="$OUT/run-$i.json" \
    node_modules/.bin/playwright test --reporter=line,json > "$OUT/run-$i.log" 2>&1
  echo "  run $i exit=$?" | tee -a "$OUT/summary.txt"
done

python3 - "$OUT" "$RUNS" <<'PY'
import json, pathlib, sys, collections
out, runs = pathlib.Path(sys.argv[1]), int(sys.argv[2])

def failures(path):
    """Every spec that did not pass, as 'file:line :: title'."""
    found = []
    def walk(suite, file_hint=None):
        f = suite.get("file", file_hint)
        for spec in suite.get("specs", []):
            if not spec.get("ok", True):
                found.append(f"{f}:{spec.get('line')} :: {spec.get('title','')[:58]}")
        for child in suite.get("suites", []):
            walk(child, f)
    data = json.loads(path.read_text())
    for s in data.get("suites", []):
        walk(s)
    return found, data.get("stats", {})

per_run, all_fails = {}, collections.Counter()
print()
print("=" * 78)
for i in range(1, runs + 1):
    p = out / f"run-{i}.json"
    if not p.exists():
        print(f"run {i}: no JSON (run did not finish)")
        continue
    fails, stats = failures(p)
    per_run[i] = fails
    all_fails.update(fails)
    mins = (stats.get("duration") or 0) / 60000
    print(f"run {i}: {stats.get('expected',0)} passed, {stats.get('unexpected',0)} failed, "
          f"{stats.get('flaky',0)} flaky, {mins:.1f} min")
    for f in fails:
        print(f"    FAIL {f}")

print("=" * 78)
if not all_fails:
    print("No failures in any run.")
else:
    n = len(per_run)
    print(f"Failure frequency across {n} run(s):")
    for spec, count in all_fails.most_common():
        verdict = "DEFECT (every run)" if count == n else f"flake ({count}/{n})"
        print(f"  {count}/{n}  {verdict:22} {spec}")
    print()
    print("A spec failing in every run is a defect in the code under test.")
    print("A spec failing in only some runs is a race — in the spec or the app —")
    print("not automatically 'flake to be ignored'. Read the error before deciding:")
    print("  - timed out at the *test* budget (30s)  -> the machine, or a slow suite")
    print("  - timed out at an *expect* budget (10s) -> something never happened;")
    print("    a keypress dropped before hydration looks exactly like this")
PY
