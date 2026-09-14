/**
 * Interpreting a `tool_call` event well enough to render it as work.
 *
 * The session protocol deliberately does not standardize tool payloads: `data`
 * is an open object, and each adapter fills it with whatever its own vendor
 * produced. So this module normalizes what the shipped adapters actually emit,
 * verified against their source:
 *
 * - **claude** (`crates/adapters/src/claude/translate.rs`): `data.input` is the parsed
 *   tool input object, or the raw string when the streamed partial JSON did not
 *   parse. `text` is that raw JSON.
 * - **pi** (`crates/adapters/src/pi.rs`): `data.input` is the tool-call arguments
 *   object, from `args` on `tool_execution_start` and from `toolCall.arguments`
 *   on a delta.
 * - **codex** (`crates/adapters/src/codex/`): the same `data.input` shape.
 *
 * The single shape is therefore `data.input` holding the tool's arguments, with
 * `text` as a stringified fallback. Nothing else is read off `data`: a spelling
 * no adapter produces is not tolerated here, per AGENTS.md § Compatibility.
 *
 * What *is* open is the argument vocabulary *inside* `input`. Tool names and
 * their argument keys come from whichever MCP servers, plugins, and user
 * adapters (`SYLPH_AGENTS_DIR`) are installed, which no list in this repo can
 * close. So key matching below is generous, and everything degrades to "unknown
 * tool with some arguments" rather than throwing — a timeline that crashes on an
 * unfamiliar tool is worse than one that renders it plainly.
 *
 * # No icon here
 *
 * `ToolShape` used to carry an `icon` string, and each rule below named a Unicode
 * glyph. That put presentation in a pure, unit-tested logic module and forced the
 * choice to be a *character* — which is decided by the reader's installed fonts,
 * not by this project. `kind` is the semantic answer, and `components/execution-timeline.tsx`
 * maps it to an icon. This module stays free of React and testable in `node`.
 */

import type { StreamEvent } from "@/lib/api-types";

/** Presentation category, chosen by tool name across agent vocabularies. */
export type ToolKind = "read" | "edit" | "write" | "shell" | "search" | "task" | "other";

export interface EditHunk {
  /** File the edit applies to, relative or absolute as the agent named it. */
  path?: string;
  before: string;
  after: string;
}

export interface ToolShape {
  kind: ToolKind;
  /** Tool name as the agent reported it, for display. */
  name: string;
  /** File or directory the call targets, when it names one. */
  path?: string;
  /** Shell command, for shell tools. */
  command?: string;
  /** Search pattern, for grep/glob style tools. */
  pattern?: string;
  /** One-line summary for the collapsed header. */
  summary: string;
  /** Edits to render as diffs. Empty for non-edit tools. */
  hunks: EditHunk[];
  /** Remaining arguments, shown under "Arguments" when expanded. */
  args: Record<string, unknown>;
}

/**
 * Tool-name vocabulary across agents.
 *
 * Matching is on the lowercased name with non-alphanumerics stripped, so
 * `str_replace_editor`, `strReplaceEditor`, and `StrReplace` all land together.
 * Names are matched by substring after that, because agents prefix and suffix
 * freely (`mcp__fs__read_file`).
 */
const KIND_RULES: Array<{ kind: ToolKind; patterns: string[] }> = [
  // Order matters: `multiedit` must be tested before `edit`, and `writefile`
  // before `file`, so the more specific rule wins.
  { kind: "edit", patterns: ["multiedit", "strreplace", "applypatch", "edit", "patch"] },
  { kind: "write", patterns: ["writefile", "createfile", "write", "create"] },
  { kind: "read", patterns: ["readfile", "read", "cat", "view", "notebookread"] },
  { kind: "shell", patterns: ["bash", "shell", "exec", "terminal", "command", "run"] },
  { kind: "search", patterns: ["grep", "glob", "search", "find", "ripgrep", "ls", "list"] },
  { kind: "task", patterns: ["task", "agent", "todo", "plan", "delegate"] },
];

/**
 * Argument keys that name a file, in the order agents prefer them.
 *
 * The camelCase entries (`filePath`, `notebookPath`, `targetFile`) are **not**
 * dead second spellings of the snake_case ones, and an audit for compatibility
 * code should leave them alone. No adapter in this repo decides this vocabulary:
 * tool names and their argument keys come from whatever MCP servers, plugins,
 * and user adapters (`SYLPH_AGENTS_DIR`) a user has installed, and those are
 * written by other people in whatever style they like. Matching a key an
 * installed tool may plausibly use is live vendor adaptation, which
 * AGENTS.md § Compatibility explicitly still permits — unlike tolerating a
 * spelling one of *our* producers used to emit.
 */
const PATH_KEYS = [
  "file_path",
  "filePath",
  "path",
  "notebook_path",
  "notebookPath",
  "target_file",
  "targetFile",
  "filename",
  "file",
];

const COMMAND_KEYS = ["command", "cmd", "script", "shell_command"];
const PATTERN_KEYS = ["pattern", "query", "regex", "glob", "search"];

export function describeTool(event: StreamEvent): ToolShape {
  const name = event.name?.trim() || "tool";
  const args = extractArgs(event);
  const normalized = name.toLowerCase().replace(/[^a-z0-9]/g, "");

  const rule = KIND_RULES.find((candidate) =>
    candidate.patterns.some((pattern) => normalized.includes(pattern)),
  );
  const kind = rule?.kind ?? "other";

  const path = firstString(args, PATH_KEYS);
  const command = firstString(args, COMMAND_KEYS);
  const pattern = firstString(args, PATTERN_KEYS);
  const hunks = extractHunks(args, path);

  return {
    kind,
    name,
    path,
    command,
    pattern,
    summary: summarize({ kind, name, path, command, pattern, args, hunks }),
    hunks,
    args: omitKeys(args, [
      ...PATH_KEYS,
      ...COMMAND_KEYS,
      ...PATTERN_KEYS,
      // The keys `extractHunks` consumes, so a diff is not repeated verbatim
      // under "Arguments". snake_case only: these are `claude`'s own Edit,
      // MultiEdit, and Write inputs, forwarded unchanged by
      // `crates/adapters/src/claude/translate.rs`.
      "old_string",
      "new_string",
      "edits",
      "content",
      "replace_all",
    ]),
  };
}

/**
 * Recover the argument object from an event.
 *
 * Three shapes are accepted because the shipped adapters produce three:
 * `data.input` as an object (both adapters' happy path), `data.input` as a string
 * (`crates/adapters/src/claude/translate.rs`, when the streamed partial JSON did not
 * parse), and `text` as raw JSON (`claude`'s `text` always holds it, and is the
 * only source when `data` is absent — which is the case for the `started` and
 * `delta` phases).
 */
function extractArgs(event: StreamEvent): Record<string, unknown> {
  const data = event.data;
  if (data && typeof data === "object" && !Array.isArray(data)) {
    const input = (data as Record<string, unknown>).input;
    if (input && typeof input === "object" && !Array.isArray(input)) {
      return input as Record<string, unknown>;
    }
    if (typeof input === "string") {
      const parsed = tryParse(input);
      if (parsed) return parsed;
      // A non-JSON string input is the whole argument; the commonest case is a
      // shell command, so naming it `command` renders correctly.
      return { command: input };
    }
    // Nothing else on `data` is read. Every shipped adapter nests a tool's
    // arguments under `input` (`crates/adapters/src/claude/translate.rs`,
    // `crates/adapters/src/pi.rs`), so a top-level `data.command` read would be
    // tolerance for a producer that does not exist.
  }

  if (event.text) {
    const parsed = tryParse(event.text);
    if (parsed) return parsed;
  }
  return {};
}

function tryParse(raw: string): Record<string, unknown> | null {
  const trimmed = raw.trim();
  if (!trimmed.startsWith("{")) return null;
  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    // Partial JSON is expected: claude streams tool input as `partial_json`
    // deltas, so a mid-stream parse failure is normal, not an error.
    return null;
  }
}

/**
 * Pull diffable before/after pairs out of edit arguments.
 *
 * Covers the two shapes that matter: a single `old_string`/`new_string` pair
 * (Edit), and an `edits` array of such pairs (MultiEdit). A `Write` call has no
 * "before", so it yields one hunk with an empty before — which renders as an
 * all-additions diff, exactly what creating a file is.
 */
function extractHunks(args: Record<string, unknown>, path?: string): EditHunk[] {
  const hunks: EditHunk[] = [];

  const single = pair(args);
  if (single) hunks.push({ path, ...single });

  const edits = args.edits;
  if (Array.isArray(edits)) {
    for (const entry of edits) {
      if (entry && typeof entry === "object") {
        const found = pair(entry as Record<string, unknown>);
        if (found) hunks.push({ path, ...found });
      }
    }
  }

  // A write with content but no old/new pair: treat the content as an addition.
  if (hunks.length === 0 && typeof args.content === "string" && args.content.length > 0) {
    hunks.push({ path, before: "", after: args.content });
  }

  return hunks;
}

function pair(source: Record<string, unknown>): { before: string; after: string } | null {
  // snake_case only: `claude` forwards its Edit/MultiEdit input verbatim
  // (`crates/adapters/src/claude/translate.rs`), and that vendor spells these
  // `old_string`/`new_string`.
  const before = source.old_string;
  const after = source.new_string;
  if (typeof after !== "string") return null;
  return { before: typeof before === "string" ? before : "", after };
}

function firstString(args: Record<string, unknown>, keys: string[]): string | undefined {
  for (const key of keys) {
    const value = args[key];
    if (typeof value === "string" && value.trim()) return value;
  }
  return undefined;
}

function omitKeys(
  args: Record<string, unknown>,
  keys: string[],
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (!keys.includes(key)) out[key] = value;
  }
  return out;
}

/** The collapsed header line: what this call did, in as few words as possible. */
function summarize(shape: {
  kind: ToolKind;
  name: string;
  path?: string;
  command?: string;
  pattern?: string;
  args: Record<string, unknown>;
  hunks: EditHunk[];
}): string {
  if (shape.command) return shape.command;
  if (shape.pattern) {
    return shape.path ? `${shape.pattern} in ${shape.path}` : shape.pattern;
  }
  if (shape.path) return shape.path;

  // No recognized field: show the first scalar argument rather than nothing, so
  // an unfamiliar tool still says something about what it was asked to do.
  for (const [key, value] of Object.entries(shape.args)) {
    if (typeof value === "string" && value.trim()) {
      return `${key}: ${truncate(value, 120)}`;
    }
    if (typeof value === "number" || typeof value === "boolean") {
      return `${key}: ${value}`;
    }
  }
  return shape.name;
}

function truncate(value: string, max: number): string {
  const flat = value.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

/** Basename, for headers that should not wrap on a deep path. */
export function basename(path: string): string {
  const leaf = path.replace(/\/+$/, "").split("/").filter(Boolean).pop();
  return leaf ?? path;
}

/**
 * Whether a tool result reports failure, and the exit code if it named one.
 *
 * `isError` is the declared outcome of a `tool_result`
 * (`docs/session-protocol.md:663`), and both shipped adapters set it: from
 * `is_error` on the vendor block at `crates/adapters/src/claude/translate.rs`, and from
 * the tool-execution result at `crates/adapters/src/pi.rs`. No `status` string
 * is read off `data` — no producer writes one, so a branch on it was tolerance
 * for a producer that does not exist.
 *
 * `data.exitCode` is a different case, and is deliberately still read even though
 * the protocol does not declare it. `components/execution-timeline.tsx:942` renders
 * "failed (exit N)" from it, which is the most useful single fact about a failed
 * shell call, but today the only producer is the E2E fixture
 * (`crates/adapters/src/e2e.rs`, `tool`) — the adapters put `exitCode` on a
 * `status` event at process exit, which is a different event kind entirely. So
 * this read is ahead of its spec: it stays pending a `docs/session-protocol.md`
 * change declaring `data.exitCode` as an optional refinement of `isError`, and
 * if that change is rejected instead, this and the fixture's shape both go.
 * `exit_code` was never emitted by anything and is gone.
 */
export function resultStatus(event: StreamEvent): { exitCode?: number; failed: boolean } {
  const failed = event.isError === true;
  const data = event.data;
  if (!data || typeof data !== "object") return { failed };
  const record = data as Record<string, unknown>;
  const raw = record.exitCode;
  const exitCode = typeof raw === "number" ? raw : undefined;
  return {
    exitCode,
    failed: failed || (exitCode !== undefined && exitCode !== 0),
  };
}
