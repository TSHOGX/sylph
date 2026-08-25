#!/usr/bin/env bash
#
# Measure first-load JS for the root route, gzipped.
#
# The number is a review trigger rather than a limit that forbids a feature
# (docs/README.md principle 4), so this exits 0 when it is exceeded and says so
# loudly. `--strict` makes it exit non-zero for anyone who wants a gate; nothing
# in this repo turns that on, because the decision on record is that a budget
# does not get to veto value.
#
# It exists because the recipe was run by hand, and a measurement run by hand
# goes stale silently: `docs/roadmap.md` carried 198.2 KB through two phases that
# each measured higher. A stale number is worse than no number, because it is
# read as current.
#
# Written for bash 3.2 — macOS ships that, and this script has to run there.

set -euo pipefail

BUDGET_KB=200
STRICT=0
for arg in "$@"; do
  case "$arg" in
    --strict) STRICT=1 ;;
    --budget=*) BUDGET_KB="${arg#--budget=}" ;;
    -h|--help)
      echo "usage: $0 [--strict] [--budget=KB]"
      exit 0
      ;;
    *)
      echo "unknown argument: $arg" >&2
      exit 2
      ;;
  esac
done

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
frontend="$repo_root/frontend"
# Next renders the root route to this file, and it is the only place that lists
# the scripts `/` actually references. Reading the chunk directory instead would
# count chunks belonging to other routes, which is what makes a naive
# "du -sh .next/static" answer meaningless.
entry="$frontend/.next/server/app/index.html"

if [ ! -f "$entry" ]; then
  echo "No production build found at .next/server/app/index.html" >&2
  echo "Run this first, with the backend URL set — Next compiles rewrites into" >&2
  echo "the route manifest at build time, so the variable must be present:" >&2
  echo >&2
  echo "  cd frontend && SYLPH_WEB_BACKEND=http://localhost:4317 npm run build" >&2
  exit 1
fi

total=0
count=0
# Sorted and de-duplicated: a chunk referenced twice is downloaded once, so
# counting it twice would overstate the figure.
chunks="$(grep -o '/_next/static/chunks/[^"]*\.js' "$entry" | sort -u)"

echo "chunk                                                     gzipped"
echo "---------------------------------------------------------  -------"
while IFS= read -r chunk; do
  [ -n "$chunk" ] || continue
  file="$frontend/.next${chunk#/_next}"
  if [ ! -f "$file" ]; then
    echo "referenced chunk is missing from the build: $chunk" >&2
    exit 1
  fi
  size="$(gzip -c "$file" | wc -c | tr -d ' ')"
  total=$((total + size))
  count=$((count + 1))
  printf '%-57s %6.1f KB\n' "$(basename "$chunk")" "$(echo "scale=3; $size/1024" | bc)"
done <<EOF
$chunks
EOF

kb="$(echo "scale=1; $total/1024" | bc)"
echo
echo "first-load JS: ${kb} KB gzipped across ${count} chunks (budget ${BUDGET_KB} KB)"

over="$(echo "$kb > $BUDGET_KB" | bc)"
if [ "$over" = "1" ]; then
  delta="$(echo "scale=1; $kb - $BUDGET_KB" | bc)"
  echo "OVER BUDGET by ${delta} KB — worth a look at what landed, not a veto."
  echo "Record the new figure in docs/roadmap.md; do not carry the old one forward."
  [ "$STRICT" = "1" ] && exit 1
fi

exit 0
