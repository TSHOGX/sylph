#!/usr/bin/env bash
#
# Install Sylph Web for local use.
#
# Builds both components, stages them under a prefix, and optionally installs a
# LaunchAgent so they start at login. Idempotent: re-running upgrades in place.
#
#   ./scripts/install.sh                    # build and install, no autostart
#   ./scripts/install.sh --launchagent      # also start at login
#   ./scripts/install.sh --rollback         # restore the previous version
#   ./scripts/install.sh --uninstall        # remove everything it installed
#
# Never reads or writes the TraceDB archive: that is a separate project with its
# own install. Uninstall leaves ~/.sylph alone — your config, identity, sessions,
# and history — and it is removed only by an explicit --purge.

set -euo pipefail

PREFIX="${SYLPH_WEB_PREFIX:-$HOME/.local/share/sylph-web}"
BIN_DIR="${SYLPH_WEB_BIN_DIR:-$HOME/.local/bin}"
# The one root the app owns: config, identity, data, scratch, logs. Propagating
# this single variable is what replaced writing six of them into the plist and
# discovering later that a seventh had been added.
SYLPH_HOME="${SYLPH_HOME:-$HOME/.sylph}"
DATA_DIR="$SYLPH_HOME/data"
BACKEND_PORT="${SYLPH_PORT:-4317}"
FRONTEND_PORT="${SYLPH_WEB_FRONTEND_PORT:-3000}"
# Overridable so an install into a throwaway prefix cannot unload or delete the
# agent belonging to the real install.
LABEL="${SYLPH_WEB_LAUNCH_LABEL:-dev.sylph.web}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

WANT_LAUNCHAGENT=0
DO_UNINSTALL=0
DO_PURGE=0
DO_ROLLBACK=0

for arg in "$@"; do
  case "$arg" in
    --launchagent) WANT_LAUNCHAGENT=1 ;;
    --uninstall) DO_UNINSTALL=1 ;;
    --purge) DO_UNINSTALL=1; DO_PURGE=1 ;;
    --rollback) DO_ROLLBACK=1 ;;
    -h|--help) sed -n '3,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $arg (try --help)" >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$1" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$1" >&2; exit 1; }

unload_agent() {
  if [ -f "$PLIST" ]; then
    launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null ||
      launchctl unload "$PLIST" 2>/dev/null || true
  fi
}

if [ "$DO_ROLLBACK" -eq 1 ]; then
  [ -d "$PREFIX.old" ] || die "No previous version at $PREFIX.old to roll back to."
  say "Rolling back to the previous version"
  unload_agent

  # Swap current and previous, so a rollback is itself reversible: running
  # --rollback again returns to the version you just left.
  rm -rf "$PREFIX.rollback-tmp"
  [ -d "$PREFIX" ] && mv "$PREFIX" "$PREFIX.rollback-tmp"
  mv "$PREFIX.old" "$PREFIX"
  [ -d "$PREFIX.rollback-tmp" ] && mv "$PREFIX.rollback-tmp" "$PREFIX.old"

  if [ -d "$PREFIX/journal-backup" ] && [ -f "$PREFIX/journal-backup/journal.db" ]; then
    # The version being left may have migrated the schema past what this binary
    # accepts, so restore the journal as it was when this version last ran.
    # Its journal is set aside as the backup for the tree we just moved to
    # $PREFIX.old, which makes rolling forward again restore it symmetrically.
    say "Restoring the journal from this version's backup"
    rm -rf "$PREFIX.old/journal-backup"
    mkdir -p "$PREFIX.old/journal-backup"
    for f in "$DATA_DIR"/journal.db "$DATA_DIR"/journal.db-wal "$DATA_DIR"/journal.db-shm; do
      [ -f "$f" ] && mv "$f" "$PREFIX.old/journal-backup/"
    done
    # Guarded because a glob that matches nothing expands to itself, and a `cp`
    # of that non-match would abort the restore under `set -e`.
    for f in "$PREFIX/journal-backup"/journal.db*; do
      [ -f "$f" ] && cp "$f" "$DATA_DIR/"
    done
    say "The other version's journal is kept at $PREFIX.old/journal-backup"
  else
    warn "No journal backup found; keeping the current journal. If the version you are leaving migrated the schema, the restored binary may refuse to start."
  fi

  ln -sf "$PREFIX/start.sh" "$BIN_DIR/sylph-web"
  ln -sf "$PREFIX/sylph-web-backend" "$BIN_DIR/sylph-web-backend"
  if [ -f "$PLIST" ]; then
    launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null || launchctl load "$PLIST"
    say "Restarted the LaunchAgent"
  fi
  say "Rolled back. Run --rollback again to return to the other version."
  exit 0
fi

if [ "$DO_UNINSTALL" -eq 1 ]; then
  say "Removing LaunchAgent"
  unload_agent
  rm -f "$PLIST"
  say "Removing $PREFIX and launchers"
  rm -rf "$PREFIX" "$PREFIX.old" "$PREFIX.new"
  rm -f "$BIN_DIR/sylph-web" "$BIN_DIR/sylph-web-backend"
  if [ "$DO_PURGE" -eq 1 ]; then
    # The whole root, not just the journal: with one directory, "delete everything
    # this app owns" is finally expressible. It is also why --purge is separate
    # from --uninstall — the config, the identity file, and every session live
    # here, and no uninstall should take them without being asked.
    warn "Purging $SYLPH_HOME (config, identity, sessions, and history)"
    rm -rf "$SYLPH_HOME"
  else
    say "Left your data at $SYLPH_HOME (use --purge to delete it)"
  fi
  say "Uninstalled."
  exit 0
fi

# --- preflight ------------------------------------------------------------

command -v cargo >/dev/null || die "cargo not found. Install Rust 1.85+."
command -v node >/dev/null || die "node not found. Install Node.js 22.5+."
command -v npm >/dev/null || die "npm not found. Install Node.js 22.5+."

node_major="$(node -p 'process.versions.node.split(".")[0]')"
[ "$node_major" -ge 22 ] || die "Node.js 22.5+ required, found $(node -v)."

# An upgrade legitimately finds the port held by the agent we are replacing, so
# stop that first and only then treat a busy port as somebody else's.
[ -f "$PLIST" ] && unload_agent
if lsof -ti ":$BACKEND_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  die "Port $BACKEND_PORT is already in use by another process. Stop it, or set SYLPH_PORT to a free port."
fi

say "Installing into $PREFIX (backend port $BACKEND_PORT, frontend $FRONTEND_PORT)"

# --- build ----------------------------------------------------------------

say "Building backend (release)"
cargo build --release --manifest-path "$REPO/backend/Cargo.toml"

say "Building frontend (standalone)"
(
  cd "$REPO/frontend"
  [ -d node_modules ] || npm ci
  # The proxy target is compiled into the route manifest, so it must be set for
  # the build. Setting it only at start time leaves the app on the default port.
  SYLPH_WEB_BACKEND="http://127.0.0.1:$BACKEND_PORT" npm run build
)

# --- stage ----------------------------------------------------------------

say "Staging artifacts"
rm -rf "$PREFIX.new"
# `logs` is created unconditionally: start.sh rotates the files there whether or
# not a LaunchAgent is what launched it.
mkdir -p "$PREFIX.new/frontend" "$PREFIX.new/logs" "$BIN_DIR" "$DATA_DIR"

cp "$REPO/backend/target/release/sylph-web-backend" "$PREFIX.new/sylph-web-backend"
cp -R "$REPO/frontend/.next/standalone/." "$PREFIX.new/frontend/"

# No adapters are staged, because there is no directory of them to stage. They are
# compiled into the binary copied above, so an agent is available exactly when its
# vendor CLI is on the PATH the server runs under.
#
# This deletes a check that was load-bearing while it was needed, and the reason it
# was is worth keeping: with no adapters staged the backend fell through to a
# compile-time repo path, so an install worked on the build machine and found zero
# agents anywhere else — and zero agents means no session can be created and no
# slash command offered, while every health probe still reports the server up.
# There is no equivalent failure to guard now: a missing CLI is reported per agent
# by `sylph-web doctor` and per environment by `GET /api/v1/environments`, both of
# which name the binary to install.
# Standalone omits these two; without them every asset request 404s.
mkdir -p "$PREFIX.new/frontend/.next"
cp -R "$REPO/frontend/.next/static" "$PREFIX.new/frontend/.next/static"
[ -d "$REPO/frontend/public" ] && cp -R "$REPO/frontend/public" "$PREFIX.new/frontend/public"

cat > "$PREFIX.new/start.sh" <<EOF
#!/usr/bin/env bash
# Start backend and frontend together; stop both on exit or Ctrl-C.
set -euo pipefail
export SYLPH_PORT="\${SYLPH_PORT:-$BACKEND_PORT}"
export SYLPH_HOME="\${SYLPH_HOME:-$SYLPH_HOME}"
here="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"

# launchd appends to its log paths forever, so rotate here. Copy then truncate
# in place rather than renaming: launchd opened these paths before exec'ing this
# script and holds the descriptors, so a rename would send this run's output to
# the rotated file. Truncating keeps the inode, and O_APPEND writes land at the
# new end. One generation is kept.
for log in "\$here/logs/stdout.log" "\$here/logs/stderr.log"; do
  [ -f "\$log" ] || continue
  size=\$(wc -c < "\$log" | tr -d ' ')
  if [ "\${size:-0}" -gt 16777216 ]; then
    cp "\$log" "\$log.1" && : > "\$log"
  fi
done

"\$here/sylph-web-backend" &
backend=\$!
trap 'kill \$backend 2>/dev/null || true' EXIT INT TERM

PORT="\${SYLPH_WEB_FRONTEND_PORT:-$FRONTEND_PORT}" \\
  HOSTNAME=127.0.0.1 node "\$here/frontend/server.js" &
frontend=\$!
trap 'kill \$backend \$frontend 2>/dev/null || true' EXIT INT TERM

# Exit as soon as either half dies, so a half-running install is visible rather
# than looking healthy. Polled instead of \`wait -n\`, which macOS's bash 3.2
# does not have.
while kill -0 \$backend 2>/dev/null && kill -0 \$frontend 2>/dev/null; do
  sleep 1
done
echo "sylph-web: one half exited, shutting down the other" >&2
exit 1
EOF
chmod +x "$PREFIX.new/start.sh" "$PREFIX.new/sylph-web-backend"

# Swap in only once the new tree is complete, so a failed build cannot leave a
# half-installed prefix behind. The previous tree is kept as $PREFIX.old, which
# is what --rollback restores.
unload_agent
rm -rf "$PREFIX.old"
if [ -d "$PREFIX" ]; then
  mv "$PREFIX" "$PREFIX.old"
  # A newer version may add a migration, and sqlx refuses to open a journal
  # whose schema is ahead of the binary. So a rollback needs the journal as it
  # was, not just the old binaries. Safe to copy directly: the agent is stopped
  # and the WAL files come with it.
  if [ -f "$DATA_DIR/journal.db" ]; then
    say "Backing up the journal for rollback"
    rm -rf "$PREFIX.old/journal-backup"
    mkdir -p "$PREFIX.old/journal-backup"
    for f in "$DATA_DIR"/journal.db "$DATA_DIR"/journal.db-wal "$DATA_DIR"/journal.db-shm; do
      [ -f "$f" ] && cp "$f" "$PREFIX.old/journal-backup/"
    done
  fi
fi
mv "$PREFIX.new" "$PREFIX"

ln -sf "$PREFIX/start.sh" "$BIN_DIR/sylph-web"
ln -sf "$PREFIX/sylph-web-backend" "$BIN_DIR/sylph-web-backend"

# --- launchagent ----------------------------------------------------------

if [ "$WANT_LAUNCHAGENT" -eq 1 ]; then
  say "Installing LaunchAgent ($LABEL)"
  mkdir -p "$(dirname "$PLIST")" "$PREFIX/logs"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$PREFIX/start.sh</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <!--
      launchd starts from a bare environment, so anything the backend needs has to
      be written here to survive. That used to mean enumerating a variable per
      path, and a variable added later was silently absent from every existing
      install. Now there is one root: everything else the backend resolves from
      SYLPH_HOME or reads out of its own config.toml, so this list does not grow
      when a setting is added.

      The frontend port is here because it is the launcher's, not the backend's,
      and PATH because launchd provides none and the frontend is a node process.
    -->
    <key>SYLPH_HOME</key><string>$SYLPH_HOME</string>
    <key>SYLPH_PORT</key><string>$BACKEND_PORT</string>
    <key>SYLPH_WEB_FRONTEND_PORT</key><string>$FRONTEND_PORT</string>
    <key>PATH</key><string>$(dirname "$(command -v node)"):/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>$PREFIX/logs/stdout.log</string>
  <key>StandardErrorPath</key><string>$PREFIX/logs/stderr.log</string>
</dict>
</plist>
EOF
  launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null ||
    launchctl load "$PLIST"
fi

# --- verify ---------------------------------------------------------------

say "Verifying"
if [ "$WANT_LAUNCHAGENT" -eq 1 ]; then
  ok=0
  for _ in $(seq 1 40); do
    if curl -fsS "http://127.0.0.1:$BACKEND_PORT/api/v1/health" >/dev/null 2>&1; then
      ok=1; break
    fi
    sleep 0.5
  done
  if [ "$ok" -eq 1 ]; then
    say "Backend healthy on http://127.0.0.1:$BACKEND_PORT"
    say "Open http://localhost:$FRONTEND_PORT"
  else
    warn "Backend did not answer within 20s. Check $PREFIX/logs/stderr.log"
    exit 1
  fi
else
  # The backend takes no arguments, so there is no --version to probe; check the
  # artifacts an install must have instead.
  [ -x "$PREFIX/sylph-web-backend" ] || die "backend binary is missing or not executable"
  [ -f "$PREFIX/frontend/server.js" ] || die "frontend bundle is missing server.js"
  [ -d "$PREFIX/frontend/.next/static" ] || die "frontend bundle is missing .next/static"
  say "Installed. Start it with: sylph-web"
  say "Then open http://localhost:$FRONTEND_PORT"
fi

if ! printf '%s' ":$PATH:" | grep -q ":$BIN_DIR:"; then
  warn "$BIN_DIR is not on your PATH; run $PREFIX/start.sh directly or add it."
fi
