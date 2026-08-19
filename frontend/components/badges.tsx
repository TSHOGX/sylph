import type { SessionOrigin, TurnStatus } from "@/lib/api";

/**
 * Session origin badge.
 *
 * Origin is durable product data, not decoration: a `sylph` session runs under
 * Sylph identity and lifecycle policy, while a `native` session preserves the
 * originating agent's identity and must never be given SOUL.md, reset, or
 * compacted. Green vs violet is the spec's convention (docs/roadmap.md, UX).
 *
 * The colour is paired with text because colour alone is not an accessible
 * distinction (WCAG 1.4.1 Use of Color).
 */
export function OriginBadge({ origin }: { origin: SessionOrigin }) {
  const native = origin === "native";
  return (
    <span
      className={`inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium ${
        native
          ? "bg-violet-100 text-violet-900"
          : "bg-emerald-100 text-emerald-900"
      }`}
      title={
        native
          ? "Native session: the originating agent's identity and lifecycle are preserved."
          : "Sylph session: Sylph identity is injected and Sylph lifecycle policy applies."
      }
    >
      <span aria-hidden="true">{native ? "◆" : "●"}</span>
      {native ? "native" : "sylph"}
    </span>
  );
}

const TURN_STATUS_STYLES: Record<TurnStatus, string> = {
  queued: "bg-gray-100 text-gray-800",
  running: "bg-blue-100 text-blue-900",
  stopping: "bg-amber-100 text-amber-900",
  completed: "bg-emerald-100 text-emerald-900",
  failed: "bg-red-100 text-red-900",
  cancelled: "bg-gray-200 text-gray-800",
  interrupted: "bg-orange-100 text-orange-900",
};

export function TurnStatusBadge({ status }: { status: TurnStatus }) {
  return (
    <span
      className={`inline-flex rounded px-2 py-0.5 text-xs font-medium ${
        TURN_STATUS_STYLES[status] ?? "bg-gray-100 text-gray-800"
      }`}
    >
      {status}
    </span>
  );
}

export function SessionStatusBadge({ status }: { status: "idle" | "running" }) {
  const running = status === "running";
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded px-2 py-0.5 text-xs font-medium ${
        running ? "bg-blue-100 text-blue-900" : "bg-gray-100 text-gray-700"
      }`}
    >
      {running && (
        <span
          aria-hidden="true"
          className="h-1.5 w-1.5 animate-pulse rounded-full bg-blue-600"
        />
      )}
      {status}
    </span>
  );
}
