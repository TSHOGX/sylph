import type { SessionOrigin, TurnStatus } from "@/lib/api";
import { OriginNativeIcon, OriginSylphIcon } from "@/components/icons";

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
      className={`inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-2xs font-medium ${
        native
          ? "border-native-line bg-native-bg text-native"
          : "border-success-line bg-success-bg text-success"
      }`}
      title={
        native
          ? "Native session: the originating agent's identity and lifecycle are preserved."
          : "Sylph session: Sylph identity is injected and Sylph lifecycle policy applies."
      }
    >
      {native ? <OriginNativeIcon size={9} /> : <OriginSylphIcon size={9} />}
      {native ? "native" : "sylph"}
    </span>
  );
}

/**
 * Turn status, by outcome.
 *
 * `stopping` shares the warning hue with `interrupted` but is the only *transient*
 * status among them, so it also gets the pulsing dot that `running` has — the
 * distinction that matters to a reader is "is this still moving", and hue alone
 * cannot say that. It previously used amber against `interrupted`'s orange, two
 * adjacent hues that were indistinguishable at badge size and carried no meaning
 * a reader could recover.
 *
 * Every badge carries a border. A 10% tint is nearly invisible against a dark
 * canvas without one, which is what the tokens resolve to in dark mode.
 */
const TURN_STATUS_STYLES: Record<TurnStatus, string> = {
  queued: "border-line bg-hover text-ink-soft",
  running: "border-info-line bg-info-bg text-info",
  stopping: "border-warning-line bg-warning-bg text-warning",
  completed: "border-success-line bg-success-bg text-success",
  failed: "border-danger-line bg-danger-bg text-danger",
  cancelled: "border-line bg-active text-ink-muted",
  interrupted: "border-warning-line bg-warning-bg text-warning",
};

export function TurnStatusBadge({ status }: { status: TurnStatus }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-2xs font-medium ${
        TURN_STATUS_STYLES[status] ?? "border-line bg-hover text-ink-soft"
      }`}
    >
      {(status === "running" || status === "stopping") && (
        <span
          aria-hidden="true"
          className={`h-1.5 w-1.5 animate-pulse rounded-full ${
            status === "running" ? "bg-info-solid" : "bg-warning-solid"
          }`}
        />
      )}
      {status}
    </span>
  );
}

export function SessionStatusBadge({ status }: { status: "idle" | "running" }) {
  const running = status === "running";
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-2xs font-medium ${
        running ? "border-info-line bg-info-bg text-info" : "border-line bg-hover text-ink-soft"
      }`}
    >
      {running && (
        <span
          aria-hidden="true"
          className="h-1.5 w-1.5 animate-pulse rounded-full bg-info-solid"
        />
      )}
      {status}
    </span>
  );
}
