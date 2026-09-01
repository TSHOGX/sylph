import type { Fidelity, TurnStatus } from "@/lib/api-types";
import { BlocksFidelityIcon, MessagesFidelityIcon, RelayIcon } from "@/components/icons";

/**
 * How much resolution a session's record has, marked when it is not `stream`.
 *
 * **Not an origin badge.** The distinction this replaces divided sessions by who
 * started them, which the backend never branched on; this one is a property of the
 * *record*, and it is the fact a reader actually needs — a session Sylph launched,
 * read back from the vendor's store after the process died, is `messages` too.
 *
 * `stream` is unmarked, because a marker on the ordinary case carries no information.
 * That is the same rule the deleted badge got right and the only thing worth keeping
 * from it.
 *
 * Shape plus text, never colour alone (WCAG 1.4.1), and the two non-stream values get
 * *different* shapes rather than two tints of one: the difference between them decides
 * whether a duration inside a block is recoverable, so it has to survive greyscale.
 * The tooltip says what is missing rather than naming the level again — "blocks" is
 * meaningless to a reader who has not read the protocol, and what they need to know is
 * that the timings are coarse.
 */
export function FidelityMarker({ value }: { value: Fidelity }) {
  if (value === "stream") return null;
  const blocks = value === "blocks";
  return (
    <span
      className="inline-flex items-center gap-1 rounded-md border border-line bg-hover px-1.5 py-0.5 text-2xs font-medium text-ink-soft"
      title={
        blocks
          ? "Read from the agent's own files, which flush at block boundaries. Durations inside a block were never recorded."
          : "Read from the agent's own files, which flush once per message. Only message-level timing was recorded."
      }
    >
      {blocks ? <BlocksFidelityIcon size={9} /> : <MessagesFidelityIcon size={9} />}
      {value}
    </span>
  );
}

/**
 * Which machine a session runs on.
 *
 * Rendered from the environment resource's display name, never from the id: the id is
 * opaque and base32, so showing it names nothing a user recognizes. The name is mutable and is never a key,
 * which is exactly why it is safe to render and unsafe to compare.
 *
 * `reachableVia` is drawn *differently* rather than merely annotated, and that is a
 * promise rather than decoration: a relayed environment is reachable only while the
 * named machine relays for it, so control dies when that machine leaves
 * (`docs/environments.md` § SSH-bootstrapped). Rendering it identically to a directly
 * connected environment would promise AFK continuity Sylph cannot keep.
 */
export function EnvironmentChip({
  name,
  reachableVia,
}: {
  name: string;
  /** The relaying environment's *name*, already resolved. Null when direct. */
  reachableVia?: string | null;
}) {
  const relayed = Boolean(reachableVia);
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-2xs font-medium ${
        relayed
          ? "border-warning-line bg-warning-bg text-warning"
          : "border-line bg-hover text-ink-soft"
      }`}
      title={
        relayed
          ? `Runs on ${name}, reachable only while ${reachableVia} relays for it — control ends when that machine leaves.`
          : `Runs on ${name}.`
      }
    >
      {relayed && <RelayIcon size={9} />}
      {name}
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
 * `settling` takes `running`'s hue rather than a hue of its own. The agent's work is over
 * and the server is closing the record, which is a distinction about Sylph's internals —
 * giving it a fourth colour would spend a reader's attention on something they cannot act
 * on. What it must not do is read as *finished*, hence the moving dot.
 *
 * It is close to unreachable here, and is present because the `Record` demands it rather
 * than as defensive padding: this badge draws on history entries, a settling turn is the
 * live turn, and the live turn is filtered out of that list until history absorbs it. The
 * exhaustive type is what makes that a deliberate answer instead of a `??` fallback nobody
 * chose — and it is why adding a status to `TurnStatus` fails the build here.
 *
 * Every badge carries a border. A 10% tint is nearly invisible against a dark
 * canvas without one, which is what the tokens resolve to in dark mode.
 */
const TURN_STATUS_STYLES: Record<TurnStatus, string> = {
  queued: "border-line bg-hover text-ink-soft",
  running: "border-info-line bg-info-bg text-info",
  stopping: "border-warning-line bg-warning-bg text-warning",
  settling: "border-info-line bg-info-bg text-info",
  completed: "border-success-line bg-success-bg text-success",
  failed: "border-danger-line bg-danger-bg text-danger",
  cancelled: "border-line bg-active text-ink-muted",
  interrupted: "border-warning-line bg-warning-bg text-warning",
};

/** The statuses whose badge carries a moving dot: the turn is not done yet. */
const IN_FLIGHT: ReadonlySet<TurnStatus> = new Set<TurnStatus>([
  "running",
  "stopping",
  "settling",
]);

export function TurnStatusBadge({ status }: { status: TurnStatus }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-md border px-1.5 py-0.5 text-2xs font-medium ${
        TURN_STATUS_STYLES[status] ?? "border-line bg-hover text-ink-soft"
      }`}
    >
      {IN_FLIGHT.has(status) && (
        <span
          aria-hidden="true"
          className={`h-1.5 w-1.5 animate-pulse rounded-full ${
            status === "stopping" ? "bg-warning-solid" : "bg-info-solid"
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
