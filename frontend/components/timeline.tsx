"use client";

/**
 * The conversation canvas: a left-aligned work log, nested where work was delegated.
 *
 * # Why this shape
 *
 * `docs/agent-ui-design-analysis.md` compares Claude Code, Cursor, and Zed
 * against chat UIs and lands on one distinction: in an agent workspace the tool
 * calls are the content and the prose is the connective tissue, so the layout
 * must make tool execution scannable. That rules out message bubbles, alternating
 * alignment, and per-event cards, all of which spend vertical space separating
 * speakers rather than surfacing work.
 *
 * Concretely:
 * - No bubbles, no backgrounds on prose. Tool calls get a left border as a
 *   visual anchor; everything else is flush text.
 * - Thinking is collapsed. It is debugging detail, not output.
 * - Reads collapse (context gathering); edits stay open (the actual result);
 *   shell output collapses on success and opens on failure, where it is the
 *   whole point.
 * - Every file path is a button that opens the file in the Artifacts pane.
 *
 * The one departure from flatness is delegation, and it is not a departure from
 * the principle above: a delegated trajectory is a *unit of work*, so giving it
 * one row that expands is the same call as collapsing a read. `lib/timeline-tree.ts`
 * owns the structure and explains why the tree is navigated rather than displayed;
 * this file owns how a node looks.
 *
 * # Performance
 *
 * Streaming deltas arrive at token rate, so the render path is memoized per node
 * by object identity: only the streaming tail changes identity, leaving settled
 * history untouched. A collapsed trajectory mounts none of its subtree, which
 * bounds DOM size by what the user opened rather than by what the agent did.
 *
 * This is still not windowed. A virtualization library would cost most of the
 * remaining first-load budget, and profiling has not shown DOM node count to be
 * the bottleneck — re-render count was, and collapsing now caps the node count a
 * fan-out can produce. Revisit if a session with thousands of *root-level* events
 * shows scroll jank while idle, which is the signature of a DOM-size problem
 * rather than a render-count one.
 */

import dynamic from "next/dynamic";
import { memo, useCallback, useMemo, useState, type ReactElement } from "react";
import type { Fidelity, StreamEvent } from "@/lib/api";
import { formatTime } from "@/lib/format";
import {
  buildRows,
  buildTree,
  pathTo,
  trajectoryName,
  trajectoryReport,
  type TimelineNode,
  type TimelineRow,
  type TrajectoryStatus,
} from "@/lib/timeline-tree";
import {
  basename,
  describeTool,
  resultStatus,
  type ToolKind,
  type ToolShape,
} from "@/lib/tool-shape";
import { CodeBlock, OutputBlock } from "@/components/code-block";
import { DiffView } from "@/components/diff-view";
import { Markdown } from "@/components/markdown";
import { useSettings } from "@/lib/settings";
import {
  BranchIcon,
  CheckIcon,
  ChevronRightIcon,
  EyeIcon,
  FilePlusIcon,
  PencilIcon,
  ReplyIcon,
  SearchIcon,
  SparklesIcon,
  TerminalIcon,
  ToolIcon,
  XIcon,
  type IconProps,
} from "@/components/icons";

/**
 * The sibling-comparison strip, split out of the first-load bundle.
 *
 * Every session route pays for this file; only a turn that delegated *and* whose user
 * opened the strip needs the waterfall. `ssr: false` because it renders nothing until
 * a state change, so there is no markup worth streaming — and the chunk has fetched
 * long before the click that reveals it.
 */
const DelegationOverview = dynamic(
  () => import("@/components/delegation-overview").then((m) => m.DelegationOverview),
  { ssr: false },
);

/**
 * Presentation category to icon.
 *
 * Lives here rather than in `lib/tool-shape.ts` so that module stays pure logic
 * with no React import — it is unit-tested in a `node` environment. `kind` is the
 * semantic answer; which picture represents it is a rendering decision.
 */
const TOOL_ICONS: Record<ToolKind, (props: IconProps) => ReactElement> = {
  read: EyeIcon,
  edit: PencilIcon,
  write: FilePlusIcon,
  shell: TerminalIcon,
  search: SearchIcon,
  task: BranchIcon,
  other: ToolIcon,
};

/**
 * Row and tree construction live in `lib/timeline-tree.ts`, which has no React
 * import so it can be unit-tested in a `node` environment. Re-exported here
 * because that is where consumers have always imported them from.
 */
export { buildRows, buildTree };
export type { TimelineRow, TimelineNode };

export function Timeline({
  events,
  fidelity,
  onOpenFile,
  /** Force every collapsible open or closed. Null means per-row defaults. */
  expandAll,
}: {
  events: StreamEvent[];
  /**
   * The record's fidelity, from the session these events belong to.
   *
   * Read here only to hand to `DelegationOverview`, which must not draw a duration a
   * `blocks` or `messages` record never measured. Threaded rather than defaulted at any
   * point on the way down, because a default is indistinguishable from having forgotten
   * it and the consequence is a chart of invented numbers.
   */
  fidelity: Fidelity;
  onOpenFile?: (path: string, line?: number) => void;
  expandAll?: boolean | null;
}) {
  const roots = useMemo(() => buildTree(buildRows(events)), [events]);

  /**
   * The trajectory the view is rooted at, or null for the whole turn.
   *
   * Focus is what makes unbounded depth navigable: at four levels of nesting
   * there is no indentation budget left, so instead of shrinking the content the
   * view re-roots and a breadcrumb carries the way back. This is the same move a
   * file manager makes, and the reason it works is that a user drilling into a
   * delegation has stopped caring about its siblings.
   */
  const [focus, setFocus] = useState<string | null>(null);

  // Resolved every render rather than stored: the tree is rebuilt as events
  // arrive, so a held node would go stale within a token. `null` means the
  // trajectory is gone — a cleared turn, a switched session — and the view falls
  // back to the root instead of rendering an empty pane.
  const trail = focus ? pathTo(roots, focus) : null;
  const focused = trail ? trail[trail.length - 1] : null;
  const shown = focused ? focused.children : roots;

  return (
    <div className="space-y-1.5">
      {trail && (
        <FocusTrail
          trail={trail}
          onFocus={setFocus}
          onClear={() => setFocus(null)}
        />
      )}
      {/* The focused trajectory's own row, above the work beneath it.
          Focus re-roots to `children`, which is right for a declared `subagent` —
          its row carries no content of its own. A *promoted* host is a tool call as
          well as a container (§ Promotion), so dropping its row discards the call
          that defines what its delegates are: focusing a `Workflow` whose interior
          was never forwarded showed a breadcrumb over an empty pane, which is what
          made a delegation that had genuinely reported nothing look like a broken
          one. The card renders this inline for the same reason; focus is the same
          question one level up. */}
      {focused && focused.row.event.kind !== "subagent" && (
        <TimelineRowView
          row={focused.row}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
        />
      )}
      {focused && shown.length === 0 && (
        <p className="py-1 text-2xs text-ink-muted">
          {focused.status === "running"
            ? "Waiting for this delegation's first output…"
            : "This delegation forwarded none of its own events."}
        </p>
      )}
      {/* Scoped to what is on screen, so the comparison is always between rows the
          user can see as siblings — under focus that is the branch in hand, not the
          hundred trajectories of the whole turn. Above the rows because it is a map of
          them, and it renders nothing at all unless two delegations are in scope: one
          delegation compares to nothing, and the card already states its duration. */}
      <DelegationOverview nodes={shown} fidelity={fidelity} onFocus={setFocus} />
      {shown.map((node) => (
        <MemoizedNode
          key={node.key}
          node={node}
          depth={0}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
          onFocus={setFocus}
        />
      ))}
    </div>
  );
}

/**
 * Where focus is, and every level back to the turn.
 *
 * A real `<nav>` with an ordered list, because that is what a screen reader needs
 * to announce this as navigation rather than as a row of buttons. The current node
 * is not a link: it is where you already are.
 */
function FocusTrail({
  trail,
  onFocus,
  onClear,
}: {
  trail: TimelineNode[];
  onFocus: (id: string) => void;
  onClear: () => void;
}) {
  const here = trail[trail.length - 1];
  return (
    <nav aria-label="Delegation path" className="sticky top-0 z-10 bg-canvas/95 py-1.5 backdrop-blur">
      <ol className="flex flex-wrap items-center gap-1 text-2xs">
        <li>
          <button
            type="button"
            onClick={onClear}
            className="rounded px-1 py-0.5 text-ink-muted hover:bg-hover hover:text-ink focus-ring"
          >
            Turn
          </button>
        </li>
        {trail.map((node, index) => {
          const last = index === trail.length - 1;
          return (
            <li key={node.key} className="flex items-center gap-1">
              <ChevronRightIcon size={10} className="text-ink-faint" aria-hidden="true" />
              {last ? (
                <span aria-current="page" className="px-1 py-0.5 font-medium text-ink">
                  {trajectoryName(node)}
                </span>
              ) : (
                <button
                  type="button"
                  onClick={() => onFocus(node.trajectory!)}
                  className="rounded px-1 py-0.5 text-ink-muted hover:bg-hover hover:text-ink focus-ring"
                >
                  {trajectoryName(node)}
                </button>
              )}
            </li>
          );
        })}
      </ol>
      <p className="px-1 pt-0.5 text-3xs text-ink-muted">
        <RollupSummary node={here} />
      </p>
    </nav>
  );
}

/**
 * Identity comparison, not deep equality.
 *
 * `buildRows` produces a new object only for rows whose content changed, so
 * comparing by identity keeps the settled prefix of a long conversation out of
 * every streaming re-render. Cost is then O(new rows) per token rather than
 * O(conversation length).
 *
 * This is deliberately not windowing. A virtualization library would cost most of
 * the remaining first-load budget, and profiling has not shown DOM node count to
 * be the bottleneck — re-render count was. Revisit if a session with thousands of
 * events shows scroll jank while idle, which is the signature of a DOM-size
 * problem rather than a render-count one.
 */
const MemoizedNode = memo(
  TimelineNodeView,
  (prev, next) =>
    prev.node === next.node &&
    prev.depth === next.depth &&
    prev.expandAll === next.expandAll &&
    prev.onOpenFile === next.onOpenFile &&
    prev.onFocus === next.onFocus,
);

/**
 * One node: a plain row, or a trajectory with its subtree behind a disclosure.
 *
 * `depth` is the *rendered* depth, not the tree's. It resets when focus re-roots,
 * because the indentation budget is a property of the viewport and not of what
 * the agent did.
 */
function TimelineNodeView({
  node,
  depth,
  onOpenFile,
  expandAll,
  onFocus,
}: {
  node: TimelineNode;
  depth: number;
  onOpenFile?: (path: string, line?: number) => void;
  expandAll?: boolean | null;
  onFocus: (id: string) => void;
}) {
  if (node.trajectory) {
    return (
      <TrajectoryCard
        node={node}
        depth={depth}
        onOpenFile={onOpenFile}
        expandAll={expandAll}
        onFocus={onFocus}
      />
    );
  }
  return <TimelineRowView row={node.row} onOpenFile={onOpenFile} expandAll={expandAll} />;
}

/**
 * How deep the timeline will indent before it stops.
 *
 * Two levels, then depth is carried by a badge instead. Past that the content
 * column on a phone is narrower than a line of code, and a tool call's path
 * truncates to nothing — so the indent stops conveying structure and starts
 * destroying content. Focus mode is the way further down, which is also the point
 * where a user has told us they want the whole width for this branch.
 */
const MAX_INDENT = 2;

/**
 * A delegated trajectory: one row until opened.
 *
 * Collapsed by default, and that default is the performance design as much as the
 * visual one — a closed card mounts none of its descendants, so a turn that fanned
 * out to twenty agents costs twenty rows until someone asks for more. The rollup
 * in the header is what makes that deferral honest: every failure, tool, and token
 * beneath it is counted at the top, so nothing hidden is unreported.
 */
function TrajectoryCard({
  node,
  depth,
  onOpenFile,
  expandAll,
  onFocus,
}: {
  node: TimelineNode;
  depth: number;
  onOpenFile?: (path: string, line?: number) => void;
  expandAll?: boolean | null;
  onFocus: (id: string) => void;
}) {
  // Opens on failure, like a failed shell command: that is the case where the
  // contents are the answer rather than detail.
  //
  // Derived, not initial. `useState(node.status === "failed")` reads the status once
  // at mount and never again, and a delegation's failure almost always arrives
  // *after* its row exists: a descendant fails, the status escalates, and by then the
  // initializer has long since run. So a failed branch stayed collapsed over the tool
  // that failed — the "collapsing must not be lossy" rule breaking silently, and
  // invisible to a mock fast enough to deliver both events in one render.
  //
  // An effect that force-opens on the transition is the wrong repair: it would
  // re-open a branch the user had deliberately collapsed every time another
  // descendant failed. What is wanted is a default that tracks the data until the
  // user overrides it, so the override is remembered rather than the value.
  //
  // Three inputs, one boolean, so the precedence is stated once here: an explicit
  // expand-all/collapse-all wins, then the user's own toggle on this row, then the
  // failure-derived default.
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const open = userOpen ?? node.status === "failed";
  const isOpen = expandAll ?? open;
  const name = trajectoryName(node);
  const tone = STATUS_TONE[node.status ?? "running"];

  // Two different empty cards, and saying the wrong one misdescribes the run.
  //
  // `hollow`: the delegation *finished* and forwarded nothing. Some hosts report
  // that delegated work happened without forwarding any of it — `claude` forwards
  // interior events for its built-in delegation and only lifecycle notifications for
  // the rest — so this is a real terminal state, not a pending one, and it will not
  // acquire an interior later.
  //
  // `unreported`: the work was still running when its turn ended, so the server
  // closed it. Nothing is known about its interior — not that there was none. The
  // card used to say "no interior" here, which claims a measurement nobody took:
  // it read "no interior / never finished" side by side, two statements that cannot
  // both be the point, and focusing it showed nothing with no explanation of why.
  //
  // Either way the disclosure has to stop claiming there is something behind it,
  // because telling the user only *after* they open it is the lossy collapse the
  // tree exists to avoid.
  // A promoted host is a tool call as well as a container (§ Promotion), and its own
  // call is content the card would otherwise discard: the `Workflow` script that
  // defines what the delegates *are*, and the result the tool returned. The row keeps
  // `kind: "tool_call"` precisely so none of that is lost, but this card rendered only
  // trajectory chrome — so a promoted host showed its name, a rollup, and nothing
  // else, and opening or focusing one was answered with an empty pane.
  const promoted = node.row.event.kind !== "subagent";
  const report = trajectoryReport(node);

  const empty = node.children.length === 0;
  const unreported = empty && node.status === "abandoned";
  const hollow = empty && !unreported && node.status !== "running";
  // A promoted host always has its own call to show, so it stays expandable even when
  // nothing was forwarded from inside it. The badge still says which of the two empty
  // states this is; what changes is that opening it is no longer pointless.
  const openable = promoted || (!hollow && !unreported);

  return (
    <section
      aria-label={`${name} delegation`}
      className={`rounded border-l-2 ${tone.border} bg-raised/40 pl-2.5`}
    >
      <div className="flex items-baseline gap-1.5 py-1 text-xs">
        <span className={`shrink-0 ${tone.icon}`}>
          <BranchIcon size={13} />
        </span>

        {!openable ? (
          // Not a button: `aria-expanded` on a control with nothing to expand is a
          // promise the tree cannot keep, and a screen reader would announce it as
          // collapsed content that never appears.
          <span className="min-w-0 shrink truncate px-0.5 font-medium text-ink">
            {name}
          </span>
        ) : (
          <button
            type="button"
            onClick={() => setUserOpen(!isOpen)}
            aria-expanded={isOpen}
            className="min-w-0 shrink truncate rounded px-0.5 font-medium text-ink hover:bg-hover focus-ring"
          >
            {name}
          </button>
        )}

        {hollow && (
          <span
            className="shrink-0 rounded bg-hover px-1 text-3xs text-ink-muted"
            title="This host reported that delegated work ran, but forwarded none of its events."
          >
            no interior
          </span>
        )}

        {/* The badge's own explanation used to deny the mechanism that produces it.
            It said an agent "cannot report back after its turn, because its process
            exits with the turn" — and measured on `claude` 2.x that is false: stdout
            stays open, the delegate's completion arrives *after* its turn's `result`,
            and the agent opens an exchange of its own to report it. That is why
            agent-initiated turns exist at all. So the tooltip explained this state
            with the very behaviour that resolves it, teaching a model of the product
            that the product contradicts.

            What is actually true is narrower and still worth saying: nobody has
            reported an outcome *yet*. `abandoned` is not final — a late outcome is
            written against the turn that owns the trajectory, so this row can still
            acquire one. The one case that is terminal is a restart: startup recovery
            deliberately ignores `is_backgrounded`, because a process that no longer
            exists cannot report, and that has to stay distinguishable from work still
            running in the background. */}
        {unreported && (
          <span
            className="shrink-0 rounded bg-hover px-1 text-3xs text-ink-muted"
            title="Still running when the turn ended, so the server closed it. Nobody has reported an outcome yet — if the agent reports one later it appears here, on this turn. Work left open across a server restart stays unresolved, because a process that no longer exists cannot report."
          >
            outlived its turn
          </span>
        )}

        {node.rollup.depth > 0 && (
          <span
            className="shrink-0 rounded bg-hover px-1 text-3xs text-ink-muted"
            title={`${node.rollup.trajectories} nested delegations, ${node.rollup.depth} levels deep`}
          >
            {node.rollup.trajectories}▾
          </span>
        )}

        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <TrajectoryMark status={node.status ?? "running"} failures={node.rollup.failures} />
          {/* Focus, not expand: the way *into* a branch rather than a peek at it.
              Separate control because they are different intentions and one
              button cannot mean both. */}
          {/* `aria-label` rather than `title`: visible text wins over `title` for
              the accessible name, so twenty delegations would present twenty
              controls all named "focus" — indistinguishable to a screen reader,
              and the same accessible-name collision that once broke 40 specs at
              once. The label names the delegation; the glyph stays short. */}
          <button
            type="button"
            onClick={() => onFocus(node.trajectory!)}
            aria-label={`Focus ${name}`}
            className="rounded px-1 text-3xs text-ink-muted hover:bg-hover hover:text-ink-soft focus-ring"
          >
            focus
          </button>
        </span>
      </div>

      {/* The delegate's own account of its work, which the backend records and this
          card did not show. It sits above the counts because it is the only line here
          that is *content*: the counts say work happened, and this says what came of
          it. Two lines at most — a summary can run long, and a card that grows without
          bound stops being one row per delegation, which is the property the whole tree
          is built on. `title` carries the rest rather than a "more" control, since the
          full text is one click away inside the branch. */}
      {report && (
        <p
          className="line-clamp-2 pb-0.5 pr-2 text-2xs text-ink-soft"
          title={report}
        >
          {report}
        </p>
      )}

      <p className="pb-1 pr-2 text-3xs text-ink-muted">
        <RollupSummary node={node} />
      </p>

      {isOpen && openable && (
        <div
          className={`space-y-1.5 pb-1.5 ${
            depth < MAX_INDENT ? "border-l border-line pl-2" : ""
          }`}
        >
          {/* The host's own call, above the work it dispatched, because that is the
              order it happened in: the script defines the delegates, so reading it
              after them inverts cause and effect. */}
          {promoted && (
            <TimelineRowView
              row={node.row}
              onOpenFile={onOpenFile}
              expandAll={expandAll}
            />
          )}

          {node.children.length === 0 ? (
            // Only reachable while running, or for a promoted host whose interior was
            // never forwarded — the badge above already says which.
            node.status === "running" ? (
              <p className="py-1 text-2xs text-ink-muted">
                Waiting for the delegate&apos;s first output…
              </p>
            ) : null
          ) : (
            node.children.map((child) => (
              <MemoizedNode
                key={child.key}
                node={child}
                depth={depth + 1}
                onOpenFile={onOpenFile}
                expandAll={expandAll}
                onFocus={onFocus}
              />
            ))
          )}
        </div>
      )}
    </section>
  );
}

/** Border and icon colour per trajectory status. */
const STATUS_TONE: Record<TrajectoryStatus, { border: string; icon: string }> = {
  running: { border: "border-info-line", icon: "text-info" },
  done: { border: "border-line", icon: "text-ink-muted" },
  failed: { border: "border-danger-line", icon: "text-danger" },
  // Not danger: nobody said it failed, only that nobody said. Colouring it red
  // would report an interrupted turn as a broken one.
  abandoned: { border: "border-line", icon: "text-warn" },
};

function TrajectoryMark({
  status,
  failures,
}: {
  status: TrajectoryStatus;
  failures: number;
}) {
  if (status === "running") {
    return (
      <span className="flex items-center gap-1 text-3xs text-info">
        <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-info-solid" />
        running
      </span>
    );
  }
  if (status === "abandoned") {
    return <span className="text-3xs font-medium text-warn">never finished</span>;
  }
  if (status === "failed") {
    return (
      <span className="flex items-center gap-0.5 text-3xs font-medium text-danger">
        <XIcon size={11} />
        {failures > 1 ? `${failures} failures` : "failed"}
      </span>
    );
  }
  return (
    <span className="text-success">
      <CheckIcon size={12} />
      <span className="sr-only">finished</span>
    </span>
  );
}

/**
 * What a collapsed subtree adds up to, in one line.
 *
 * This is the whole justification for collapsing by default: the counts make the
 * hidden work legible enough to decide whether to open it. Rendered as text rather
 * than chips because it is read once and scanned past.
 */
function RollupSummary({ node }: { node: TimelineNode }) {
  const { rollup } = node;
  const parts: string[] = [];

  if (rollup.trajectories > 0) parts.push(`${rollup.trajectories} nested`);
  if (rollup.tools > 0) parts.push(`${rollup.tools} tool${rollup.tools === 1 ? "" : "s"}`);
  if (rollup.events > 0) parts.push(`${rollup.events} events`);
  if (rollup.tokens) {
    parts.push(`${compact(rollup.tokens.input)} in / ${compact(rollup.tokens.output)} out`);
  }
  if (rollup.durationMs !== undefined) parts.push(formatDuration(rollup.durationMs));

  if (parts.length === 0) return null;
  return <>{parts.join(" · ")}</>;
}

// `trajectoryName` moved to `lib/timeline-tree.ts` and is imported above. It was
// duplicated here and in the overview, which is one fact with two derivations: the
// same node could be labelled differently in the card and in the strip, and the
// overview's `aria-label` would then stop matching the card's region name — a
// divergence no type checks and no test would report.

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  return `${minutes}m ${Math.round((ms % 60_000) / 1000)}s`;
}


function TimelineRowView({
  row,
  onOpenFile,
  expandAll,
}: {
  row: TimelineRow;
  onOpenFile?: (path: string, line?: number) => void;
  expandAll?: boolean | null;
}) {
  const { event, result } = row;
  const streaming = event.phase === "delta" || event.phase === "started";

  switch (event.kind) {
    case "user":
      return <UserPrompt event={event} />;
    case "thinking":
      return <ThinkingBlock event={event} streaming={streaming} expandAll={expandAll} />;
    case "tool_call":
      return (
        <ToolCall
          event={event}
          result={result}
          streaming={streaming}
          onOpenFile={onOpenFile}
          expandAll={expandAll}
        />
      );
    case "tool_result":
      // Unpaired result: rendered on its own so no output is ever swallowed.
      return <OrphanResult event={event} expandAll={expandAll} />;
    case "assistant":
      return <AssistantText event={event} streaming={streaming} />;
    case "usage":
      return <UsageLine event={event} />;
    case "status":
      return <StatusLine event={event} />;
    case "subagent":
      // A trajectory reaching here is one `buildTree` did not turn into a node,
      // which means it carried no `streamId` — so it has no identity to nest
      // anything under and nothing to show beyond that it happened. Rendering it
      // as a status line would put a bare "Unnamed" row in the timeline; the
      // node path is the only place a delegation is legible.
      return null;
    case "turn":
      // A turn boundary, not transcript content. Its `text` is the turn's final
      // result, which the `assistant` block already rendered — so drawing it here
      // shows the reply twice. Under v1 the outcome arrived as a process envelope
      // that never reached the timeline; on a live link it is an event like any
      // other, and the default branch rendered it as a status line.
      return null;
    default:
      return <StatusLine event={event} />;
  }
}

/** The user's own request. A `›` marker, not a bubble. */
function UserPrompt({ event }: { event: StreamEvent }) {
  return (
    <div className="flex gap-2 pt-2">
      <span aria-hidden="true" className="select-none pt-px font-mono text-sm text-ink-muted">
        ›
      </span>
      <div className="min-w-0 flex-1">
        <p className="whitespace-pre-wrap break-words text-sm font-medium text-ink">
          {event.text}
        </p>
      </div>
      <time
        className="shrink-0 pt-0.5 text-3xs text-ink-muted"
        dateTime={new Date(event.createdAtMs).toISOString()}
      >
        {formatTime(event.createdAtMs)}
      </time>
    </div>
  );
}

/** Assistant prose. Markdown-rendered, no chrome — it is not a message, it is text. */
function AssistantText({ event, streaming }: { event: StreamEvent; streaming: boolean }) {
  if (!event.text) return null;
  return (
    <div className="py-0.5 text-sm leading-relaxed text-ink">
      <Markdown text={event.text} />
      {streaming && (
        <span
          aria-hidden="true"
          className="ml-0.5 inline-block h-3.5 w-1.5 animate-pulse bg-ink-faint align-middle"
        />
      )}
    </div>
  );
}

/**
 * Reasoning, collapsed by default.
 *
 * `<details>` rather than a state-driven div: it gives keyboard operation,
 * `aria-expanded`, and in-page find-on-page behaviour for free, and browsers
 * expose it to screen readers as a disclosure without any ARIA.
 */
function ThinkingBlock({
  event,
  streaming,
  expandAll,
}: {
  event: StreamEvent;
  streaming: boolean;
  expandAll?: boolean | null;
}) {
  const { settings } = useSettings();
  if (!event.text) return null;

  // While streaming, thinking is open regardless of preference: watching the
  // agent reason is the one moment the text is the interesting thing.
  const open = expandAll ?? (streaming || settings.expandThinking);

  return (
    <details open={open} className="group">
      <summary className="tap-row flex cursor-pointer list-none items-center gap-1.5 py-0.5 text-xs text-ink-muted hover:text-ink">
        {/* Rotates with the disclosure rather than swapping to a second icon: one
            element that turns reads as the same control changing state. */}
        <ChevronRightIcon
          size={12}
          className="transition-transform group-open:rotate-90"
        />
        <SparklesIcon size={13} />
        <span>Thinking</span>
        <span className="text-ink-muted">
          {streaming ? "in progress" : `${wordCount(event.text)} words`}
        </span>
      </summary>
      <div className="ml-4 border-l border-line pl-3 py-1 text-xs leading-relaxed text-ink-muted">
        <Markdown text={event.text} />
      </div>
    </details>
  );
}

/**
 * A tool call and its result, as one operation.
 *
 * Expansion policy encodes the design principle that edits are output and reads
 * are context: an edit's diff is always visible, a read's content is not, and a
 * shell command's output appears when it failed — when it is the answer to
 * "what went wrong".
 */
function ToolCall({
  event,
  result,
  streaming,
  onOpenFile,
  expandAll,
}: {
  event: StreamEvent;
  result?: StreamEvent;
  streaming: boolean;
  onOpenFile?: (path: string, line?: number) => void;
  expandAll?: boolean | null;
}) {
  const { settings } = useSettings();
  const shape = useMemo(() => describeTool(event), [event]);
  const status = result ? resultStatus(result) : { failed: false, exitCode: undefined };
  const running = streaming || (!result && event.phase !== "completed");

  // Derived, not initial — the same repair the trajectory row above already carries, and
  // this was the other half of it. `AGENTS.md` § "`useState(derived)` captures a snapshot"
  // names both sites; only that one had been fixed.
  //
  // `status.failed` comes from the `tool_result`, which arrives *after* the `tool_call`
  // this row mounted on. So `useState(defaultOpen)` read the flag once, before the failure
  // was knowable, and auto-expand-on-failure fired only when both events landed in a single
  // render — which is to say only against a mock fast enough to batch them. Against a real
  // agent, and against the paced fixture, a failed command stayed collapsed behind "more",
  // which is the one case where the output *is* the answer.
  //
  // Three inputs, one boolean, so the precedence is stated rather than left to `??`
  // ordering: an explicit expand-all/collapse-all wins, then this row's own toggle, then
  // the derived default. `userOpen` remembers the *override* rather than the value, so a
  // row the user deliberately collapsed does not re-open when a later event changes the
  // default.
  const defaultOpen =
    shape.hunks.length > 0 || status.failed || settings.expandToolOutput || running;
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const open = userOpen ?? defaultOpen;
  const isOpen = expandAll ?? open;

  const openFile = useCallback(
    (path: string) => onOpenFile?.(path),
    [onOpenFile],
  );

  const hasBody =
    shape.hunks.length > 0 ||
    Boolean(result?.text) ||
    Object.keys(shape.args).length > 0 ||
    Boolean(shape.command);

  return (
    <section
      aria-label={`${shape.name} tool call`}
      className={`border-l-2 pl-2.5 ${
        status.failed ? "border-danger-line" : running ? "border-info-line" : "border-line"
      }`}
    >
      <div className="flex items-baseline gap-1.5 py-0.5 text-xs">
        <span className="shrink-0 text-ink-muted">
          <ToolGlyph kind={shape.kind} />
        </span>
        <span className="shrink-0 font-medium text-ink">{shape.name}</span>

        <ToolTarget shape={shape} onOpenFile={openFile} />

        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <StatusMark running={running} failed={status.failed} exitCode={status.exitCode} />
          {hasBody && (
            <button
              type="button"
              onClick={() => setUserOpen(!isOpen)}
              aria-expanded={isOpen}
              className="rounded px-1 text-3xs text-ink-muted hover:bg-hover hover:text-ink-soft focus-ring"
            >
              {isOpen ? "less" : "more"}
            </button>
          )}
        </span>
      </div>

      {isOpen && hasBody && (
        <div className="space-y-1.5 pb-1.5">
          {shape.hunks.map((hunk, index) => (
            <DiffView
              key={index}
              before={hunk.before}
              after={hunk.after}
              path={hunk.path}
              onOpenFile={onOpenFile ? openFile : undefined}
            />
          ))}

          {/* A multi-line command is unreadable in the header, so it repeats
              here as a code block where the whole thing is visible. */}
          {shape.command && shape.command.includes("\n") && (
            <CodeBlock code={shape.command} language="sh" showLineNumbers={false} maxLines={20} />
          )}

          {Object.keys(shape.args).length > 0 && (
            <details className="text-2xs">
              <summary className="cursor-pointer text-ink-muted hover:text-ink-soft">
                Arguments
              </summary>
              <OutputBlock text={JSON.stringify(shape.args, null, 2)} maxLines={20} />
            </details>
          )}

          {result?.text && (
            <OutputBlock text={result.text} tone={status.failed ? "error" : "neutral"} />
          )}
        </div>
      )}
    </section>
  );
}

/** The clickable target of a tool call: a file path, a command, or a pattern. */
function ToolTarget({
  shape,
  onOpenFile,
}: {
  shape: ToolShape;
  onOpenFile: (path: string) => void;
}) {
  if (shape.path) {
    return (
      <button
        type="button"
        onClick={() => onOpenFile(shape.path!)}
        title={`Open ${shape.path} in Artifacts`}
        className="min-w-0 truncate font-mono text-2xs text-ink-muted hover:text-ink hover:underline focus-ring"
      >
        {shape.path}
      </button>
    );
  }
  return (
    <span className="min-w-0 truncate font-mono text-2xs text-ink-muted">{shape.summary}</span>
  );
}

function StatusMark({
  running,
  failed,
  exitCode,
}: {
  running: boolean;
  failed: boolean;
  exitCode?: number;
}) {
  if (running) {
    return (
      <span className="flex items-center gap-1 text-3xs text-info">
        <span aria-hidden="true" className="h-1.5 w-1.5 animate-pulse rounded-full bg-info-solid" />
        running
      </span>
    );
  }
  if (failed) {
    return (
      <span className="flex items-center gap-0.5 text-3xs font-medium text-danger">
        <XIcon size={11} />
        failed{exitCode !== undefined ? ` (exit ${exitCode})` : ""}
      </span>
    );
  }
  return (
    <span className="text-success">
      <CheckIcon size={12} />
      <span className="sr-only">succeeded</span>
    </span>
  );
}

/** A result whose runner did not reuse the call's id. */
function OrphanResult({
  event,
  expandAll,
}: {
  event: StreamEvent;
  expandAll?: boolean | null;
}) {
  const status = resultStatus(event);
  // Derived from the event, not captured at mount — see the same reasoning on the
  // trajectory row above. A result's text streams in, so a failure can be known
  // after this row first rendered, and an initializer would never see it: a failed
  // command then stayed collapsed with its output — the answer to "what went wrong"
  // — behind a "more" button. `null` means the user has not decided, so the derived
  // default still applies; once they toggle, their choice is what is remembered.
  const [userOpen, setUserOpen] = useState<boolean | null>(null);
  const isOpen = expandAll ?? userOpen ?? status.failed;

  if (!event.text) return null;

  return (
    <section
      aria-label="Tool result"
      className={`border-l-2 pl-2.5 ${status.failed ? "border-danger-line" : "border-line"}`}
    >
      <div className="flex items-baseline gap-1.5 py-0.5 text-xs">
        <span className="text-ink-muted">
          <ReplyIcon size={13} />
        </span>
        <span className="font-medium text-ink-soft">
          {event.name ? `${event.name} result` : "Result"}
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          <StatusMark running={false} failed={status.failed} exitCode={status.exitCode} />
          <button
            type="button"
            onClick={() => setUserOpen(!isOpen)}
            aria-expanded={isOpen}
            className="rounded px-1 text-3xs text-ink-muted hover:bg-hover hover:text-ink-soft focus-ring"
          >
            {isOpen ? "less" : "more"}
          </button>
        </span>
      </div>
      {isOpen && (
        <div className="pb-1.5">
          <OutputBlock text={event.text} tone={status.failed ? "error" : "neutral"} />
        </div>
      )}
    </section>
  );
}

/**
 * Token usage. Rendered as one muted line: it matters for cost, and never enough
 * to earn a card.
 */
function UsageLine({ event }: { event: StreamEvent }) {
  const usage = event.data as Record<string, unknown> | undefined;
  if (!usage) return null;

  // Runners pass their backend's usage object through verbatim, so the spelling
  // differs per agent. Both conventions are read rather than picking one, since
  // showing nothing would be worse than showing an approximate label.
  const input = numeric(usage, ["inputTokens", "input_tokens", "prompt_tokens"]);
  const output = numeric(usage, ["outputTokens", "output_tokens", "completion_tokens"]);
  if (input === undefined && output === undefined) return null;

  return (
    <p className="py-0.5 text-3xs text-ink-muted">
      {input !== undefined && `${compact(input)} in`}
      {input !== undefined && output !== undefined && " / "}
      {output !== undefined && `${compact(output)} out`}
    </p>
  );
}

function StatusLine({ event }: { event: StreamEvent }) {
  if (!event.text) return null;
  return (
    <p
      className={`py-0.5 text-2xs ${
        event.isError ? "font-medium text-danger" : "text-ink-muted"
      }`}
      role={event.isError ? "alert" : undefined}
    >
      {event.isError && <XIcon size={11} className="mr-0.5 inline-block align-[-1px]" />}
      {event.text}
    </p>
  );
}

function numeric(source: Record<string, unknown>, keys: string[]): number | undefined {
  for (const key of keys) {
    const value = source[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return undefined;
}

function compact(value: number): string {
  if (value < 1000) return String(value);
  return `${(value / 1000).toFixed(1)}K`;
}

/** The icon for a tool's presentation category. */
function ToolGlyph({ kind }: { kind: ToolKind }) {
  const Glyph = TOOL_ICONS[kind];
  return <Glyph size={13} />;
}

function wordCount(text: string): number {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

export { basename };
