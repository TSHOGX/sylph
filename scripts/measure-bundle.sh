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
ROUTE="/"
for arg in "$@"; do
  case "$arg" in
    --strict) STRICT=1 ;;
    --budget=*) BUDGET_KB="${arg#--budget=}" ;;
    --route=*) ROUTE="${arg#--route=}" ;;
    -h|--help)
      echo "usage: $0 [--strict] [--budget=KB] [--route=PATH]"
      echo
      echo "  --route=/                 the root route (default)"
      echo "  --route=/sessions/[id]    a dynamic route, by its app-directory name"
      echo
      echo "Routes are measured differently and the figures are not comparable"
      echo "across them: a session route legitimately carries the timeline."
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

build_hint() {
  echo "Run this first, with the backend URL set — Next compiles rewrites into" >&2
  echo "the route manifest at build time, so the variable must be present:" >&2
  echo >&2
  echo "  cd frontend && SYLPH_WEB_BACKEND=http://localhost:4317 npm run build" >&2
}

# Two recipes, because a static and a dynamic route expose their chunks in
# different places — which is the whole reason two people measuring "the bundle"
# got 221.1 and 242.8 and both were right about a different route.
#
# A *prerendered* route has served HTML, and that HTML is the only authority on
# which scripts it actually references. A *dynamic* route emits no build-time
# HTML at all, so its chunks have to come from the build manifest. Reading the
# chunk directory instead would count every route's chunks together, which is
# what makes a naive `du -sh .next/static` meaningless.
if [ "$ROUTE" = "/" ]; then
  entry="$frontend/.next/server/app/index.html"
  if [ ! -f "$entry" ]; then
    echo "No production build found at .next/server/app/index.html" >&2
    build_hint
    exit 1
  fi
  chunks="$(grep -o '/_next/static/chunks/[^"]*\.js' "$entry" | sort -u)"
else
  # A dynamic route has to be *served* to be measured.
  #
  # Next 16's Turbopack build publishes no per-route chunk manifest for the App
  # Router — `build-manifest.json` carries only `/_app`, and
  # `app-path-routes-manifest.json` maps route names without listing their
  # scripts. So there is no build artifact to read, and the only authority on
  # which chunks a dynamic route references is the HTML it actually serves.
  if [ ! -d "$frontend/.next" ]; then
    echo "No production build found at .next" >&2
    build_hint
    exit 1
  fi

  # A concrete path for the route pattern: the page shell references the route's
  # chunks regardless of whether the id resolves, which is what is being measured.
  probe_path="$(printf '%s' "$ROUTE" | sed -e 's/\[\.\.\..*\]/measure/' -e 's/\[[^]]*\]/measure/g')"
  port=3199
  # Refuse a busy port rather than measure whatever is on it. A server left over from
  # an earlier run serves its *own* build, so the HTML names chunk hashes this build
  # never produced — which surfaces as "referenced chunk is missing from the build"
  # and reads as a broken build rather than a stale server.
  if lsof -ti "tcp:$port" >/dev/null 2>&1; then
    echo "port $port is already in use, so this would measure someone else's build." >&2
    echo "Free it first:  lsof -ti tcp:$port | xargs kill" >&2
    exit 1
  fi
  tmp_html="$(mktemp -t sylph-bundle)"
  server_log="$(mktemp -t sylph-bundle-log)"

  cleanup() {
    if [ -n "${server_pid:-}" ]; then
      # Kill the process *group*, not the pid. `npx next start` execs a child, and a
      # kill aimed at the subshell leaves that child holding the port — which then
      # serves its stale build to the next run. Learned the hard way: a survivor from
      # an earlier invocation reported chunks this build never made.
      kill -- "-$server_pid" 2>/dev/null || kill "$server_pid" 2>/dev/null
      disown "$server_pid" 2>/dev/null || true
      wait "$server_pid" 2>/dev/null || true
    fi
    # Belt and braces: whatever is still on the port is ours to clean up, because the
    # busy-port check above guarantees nothing else owned it when we started.
    lsof -ti "tcp:${port:-0}" 2>/dev/null | xargs kill 2>/dev/null || true
    rm -f "$tmp_html" "$server_log"
  }
  trap cleanup EXIT

  # `set -m` puts the background job in its own process group, which is what makes
  # the group kill in `cleanup` able to reach `next`'s child.
  set -m
  (cd "$frontend" && npx next start -p "$port" >"$server_log" 2>&1) &
  set +m
  server_pid=$!

  ready=0
  i=0
  while [ "$i" -lt 60 ]; do
    if curl -sf "http://127.0.0.1:$port$probe_path" -o "$tmp_html" 2>/dev/null; then
      ready=1
      break
    fi
    i=$((i + 1))
    sleep 0.5
  done

  if [ "$ready" != "1" ]; then
    echo "Could not serve $probe_path on port $port. Server output:" >&2
    cat "$server_log" >&2
    exit 1
  fi

  chunks="$(grep -o '/_next/static/chunks/[^"]*\.js' "$tmp_html" | sort -u)"
  if [ -z "$chunks" ]; then
    echo "Served $probe_path but found no chunk references in its HTML." >&2
    exit 1
  fi
fi

total=0
count=0

echo "chunk                                                     gzipped"
echo "---------------------------------------------------------  -------"
while IFS= read -r chunk; do
  [ -n "$chunk" ] || continue
  file="$frontend/.next${chunk#/_next}"
  if [ ! -f "$file" ]; then
    # Almost always a stale serve rather than a broken build: `next start` was given
    # one build and `.next` has since been rewritten by another `npm run build`, so
    # the HTML names chunk hashes that no longer exist. Rebuild, then measure,
    # without a build in between.
    echo "referenced chunk is missing from the build: $chunk" >&2
    echo >&2
    echo "This usually means .next was rebuilt after the page was served." >&2
    echo "Rebuild and re-measure with nothing building in between:" >&2
    echo >&2
    echo "  cd frontend && SYLPH_WEB_BACKEND=http://localhost:4317 npm run build" >&2
    echo "  ./scripts/measure-bundle.sh --route='$ROUTE'" >&2
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
echo "first-load JS for ${ROUTE}: ${kb} KB gzipped across ${count} chunks (budget ${BUDGET_KB} KB)"

over="$(echo "$kb > $BUDGET_KB" | bc)"
if [ "$over" = "1" ]; then
  delta="$(echo "scale=1; $kb - $BUDGET_KB" | bc)"
  echo "OVER BUDGET by ${delta} KB — worth a look at what landed, not a veto."
  echo "Record the new figure in docs/roadmap.md; do not carry the old one forward."
  [ "$STRICT" = "1" ] && exit 1
fi

exit 0
