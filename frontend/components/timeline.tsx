"use client";

/**
 * The conversation canvas: a flat, left-aligned work log.
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
 * # Performance
 *
 * Streaming deltas arrive at token rate, so the render path is memoized per row
 * by object identity: only the streaming tail changes identity, leaving settled
 * history untouched. This is still not windowed — see the note on
 * `MemoizedRow` for the measurement that would reverse that call.
 */

import { memo, useCallback, useMemo, useState, type ReactElement } from "react";
import type { StreamEvent } from "@/lib/api";
import { formatTime } from "@/lib/format";
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
 * Collapse a raw event stream into displayable rows.
 *
 * Two transformations happen here, and both are about not showing the same thing
 * twice:
 *
 * 1. **Delta accumulation.** A `delta` carries only the newly arrived suffix, and
 *    the matching `completed` carries the whole value. Rendering both shows every
 *    token twice, so deltas accumulate under their `streamId` and are replaced
 *    when `completed` arrives.
 * 2. **Result pairing.** A `tool_result` belongs to the `tool_call` that produced
 *    it — the protocol says they SHOULD share an id — so the result is attached
 *    to its call rather than drawn as a separate row. That is what makes a call
 *    and its output read as one operation instead of two log lines.
 */
export interface TimelineRow {
  event: StreamEvent;
  /** Result paired to this row, when it is a tool call. */
  result?: StreamEvent;
  key: string;
}

export function buildRows(events: StreamEvent[]): TimelineRow[] {
  const rows: TimelineRow[] = [];
  const openBlocks = new Map<string, number>();
  /** streamId -> index in `rows` of the tool call awaiting its result. */
  const callIndex = new Map<string, number>();

  for (const event of events) {
    const id = event.streamId;

    if (event.kind === "tool_result") {
      // Attach to the call sharing this id. Runners that do not reuse the id
      // fall through and the result renders as its own row, which is still
      // correct, just less tightly grouped.
      const at = id ? callIndex.get(id) : undefined;
      if (at !== undefined) {
        const existing = rows[at].result;
        rows[at] = {
          ...rows[at],
          // Deltas on a result append; a completed result replaces.
          result:
            event.phase === "delta" && existing
              ? { ...event, text: (existing.text ?? "") + (event.text ?? "") }
              : event,
        };
        continue;
      }
    }

    if (event.phase === "delta" && id) {
      const at = openBlocks.get(id);
      if (at !== undefined) {
        const existing = rows[at].event;
        rows[at] = {
          ...rows[at],
          event: { ...existing, text: (existing.text ?? "") + (event.text ?? "") },
        };
      } else {
        openBlocks.set(id, rows.length);
        if (event.kind === "tool_call") callIndex.set(id, rows.length);
        rows.push({ event: { ...event }, key: rowKey(event, rows.length) });
      }
      continue;
    }

    if (event.phase === "completed" && id && openBlocks.has(id)) {
      const at = openBlocks.get(id)!;
      // The completed payload is authoritative; the accumulated text is dropped.
      rows[at] = { ...rows[at], event: { ...event } };
      openBlocks.delete(id);
      continue;
    }

    // `started` with no text would render an empty row; the first delta or the
    // completed event creates it instead. Tool calls are the exception: a call
    // that has started is worth showing immediately, since the user wants to
    // see that the agent is doing something.
    if (event.phase === "started" && !event.text && event.kind !== "tool_call") {
      continue;
    }

    if (id) {
      openBlocks.set(id, rows.length);
      if (event.kind === "tool_call") callIndex.set(id, rows.length);
    }
    rows.push({ event, key: rowKey(event, rows.length) });
  }

  return rows;
}

function rowKey(event: StreamEvent, index: number): string {
  return `${event.seq}-${event.streamId ?? index}`;
}

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
  const rows = useMemo(() => buildRows(events), [events]);

  return (
    <div className="space-y-1.5">
      {rows.map((row) => (
        <MemoizedRow key={row.key} row={row} onOpenFile={onOpenFile} expandAll={expandAll} />
      ))}
    </div>
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
const MemoizedRow = memo(
  TimelineRowView,
  (prev, next) =>
    prev.row === next.row &&
    prev.expandAll === next.expandAll &&
    prev.onOpenFile === next.onOpenFile,
);

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
