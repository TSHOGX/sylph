/** Shared formatting helpers for wire values. */

/**
 * Render a backend millisecond timestamp for display.
 *
 * Every timestamp on the wire is `*AtMs`, an integer. Passing that integer
 * straight to `new Date(...)` is correct, but passing a missing field yields
 * "Invalid Date" in the UI, so absence is handled explicitly here.
 */
export function formatTimestamp(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "unknown time";
  return new Date(ms).toLocaleString();
}

export function formatTime(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "--:--";
  return new Date(ms).toLocaleTimeString();
}

/** Compact relative age, for list rows where absolute time is noise. */
export function formatRelative(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "unknown";
  const seconds = Math.round((Date.now() - ms) / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(ms).toLocaleDateString();
}

/** Human-readable byte size for attachment rows. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** Sessions may have no title; fall back to something identifiable. */
export function sessionLabel(session: {
  title: string | null;
  agent: string;
  workdir: string;
}): string {
  if (session.title && session.title.trim()) return session.title;
  const leaf = session.workdir.replace(/\/+$/, "").split("/").filter(Boolean).pop();
  return leaf ? `${session.agent} · ${leaf}` : `Untitled ${session.agent} session`;
}

/**
 * Abbreviate the home prefix, the way a shell prompt does.
 *
 * `home` is resolved server-side and arrives with the workspaces listing: a
 * browser cannot expand `~`. It is optional because every caller renders before
 * that listing settles, and an unabbreviated absolute path is a correct first
 * paint — where waiting for the prefix would blank the path entirely.
 *
 * # The sibling guard is not defensive, it is a fix
 *
 * A bare `startsWith` rewrites `/Users/hsw-backup/x` as `~-backup/x`, which names a
 * directory that does not exist and hides the one that does. `home` itself must also
 * abbreviate to `~` rather than to the empty string, which would render a workspace
 * row with no path at all. The directory picker carried a second copy of this
 * function that got both cases right, with tests; this is that behaviour, under the
 * name every other call site already imports.
 */
export function shortenHome(path: string, home?: string): string {
  if (!home || !path.startsWith(home)) return path;
  const rest = path.slice(home.length);
  if (rest === "") return "~";
  // Only a path *separator* after the prefix means "inside home". Anything else is
  // a sibling directory whose name merely starts the same way.
  return rest.startsWith("/") ? `~${rest}` : path;
}

// `originalSessionId` is deleted, and its absence is the point rather than a
// simplification. It answered "which of this session's two ids do you want" — a
// Sylph-minted UUID, or `<agent>:<native-id>` for the vendor's own tools — and
// picking the wrong one produced no error at any layer, just a lookup that found
// nothing. Identity is `(environment, agent, nativeId)` now, so `session.id` is
// simultaneously the route key and the value that names the conversation to its
// vendor: there is one id, so there is no function to choose between them.
