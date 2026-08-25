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

import { memo, useCallback, useMemo, useState, type ReactElement } from "react";
import type { StreamEvent } from "@/lib/api";
import { formatTime } from "@/lib/format";
import {
  buildRows,
  buildTree,
  pathTo,
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
  onOpenFile,
  /** Force every collapsible open or closed. Null means per-row defaults. */
  expandAll,
}: {
  events: StreamEvent[];
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
  const shown = trail ? trail[trail.length - 1].children : roots;

  return (
    <div className="space-y-1.5">
      {trail && (
        <FocusTrail
          trail={trail}
          onFocus={setFocus}
          onClear={() => setFocus(null)}
        />
      )}
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
  const [open, setOpen] = useState(node.status === "failed");
  const isOpen = expandAll ?? open;
  const name = trajectoryName(node);
  const tone = STATUS_TONE[node.status ?? "running"];

  return (
    <section
      aria-label={`${name} delegation`}
      className={`rounded border-l-2 ${tone.border} bg-raised/40 pl-2.5`}
    >
      <div className="flex items-baseline gap-1.5 py-1 text-xs">
        <span className={`shrink-0 ${tone.icon}`}>
          <BranchIcon size={13} />
        </span>

        <button
          type="button"
          onClick={() => setOpen(!isOpen)}
          aria-expanded={isOpen}
          className="min-w-0 shrink truncate rounded px-0.5 font-medium text-ink hover:bg-hover focus-ring"
        >
          {name}
        </button>

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

      <p className="pb-1 pr-2 text-3xs text-ink-muted">
        <RollupSummary node={node} />
      </p>

      {isOpen && (
        <div
          className={`space-y-1.5 pb-1.5 ${
            depth < MAX_INDENT ? "border-l border-line pl-2" : ""
          }`}
        >
          {node.children.length === 0 ? (
            <p className="py-1 text-2xs text-ink-muted">
              {node.status === "running"
                ? "Waiting for the delegate's first output…"
                : "This delegation reported no events of its own."}
            </p>
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

/** A trajectory's display name, never the vendor's tool name. */
function trajectoryName(node: TimelineNode): string {
  return node.row.event.name?.trim() || "Subagent";
}

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
      // as a status line would put a bare "Subagent" row in the timeline; the
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

  const defaultOpen =
    shape.hunks.length > 0 || status.failed || settings.expandToolOutput || running;
  const [open, setOpen] = useState(defaultOpen);
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
              onClick={() => setOpen(!isOpen)}
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
  const [open, setOpen] = useState(status.failed);
  const isOpen = expandAll ?? open;

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
            onClick={() => setOpen(!isOpen)}
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
