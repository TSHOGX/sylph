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

/** The id users can paste into the originating agent or TraceDB. */
export function originalSessionId(session: {
  id: string;
  agent: string;
  nativeSessionId?: string;
  traceSessionId?: string;
}): string {
  if (session.traceSessionId) return session.traceSessionId;
  if (session.nativeSessionId) return `${session.agent}:${session.nativeSessionId}`;
  return session.id;
}
