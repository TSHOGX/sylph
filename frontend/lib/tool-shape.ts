/**
 * Interpreting a `tool_call` event well enough to render it as work.
 *
 * The runner protocol deliberately does not standardize tool payloads: `data`
 * is "compact structured metadata", and each agent fills it with whatever its
 * own backend produced. So this module normalizes what the four shipped runners
 * actually emit, verified against their source:
 *
 * - **claude** (`runner.py`): `data.input` is the parsed tool input object, or
 *   the raw string when it did not parse as JSON. `text` is the raw JSON.
 * - **codex** (`runner.sh`): `data.input`, plus `data.command` and `data.status`.
 * - **opencode** (`runner.sh`): `data.input`, `data.status`, `data.title`.
 * - **pi** (`runner.sh`): `data.input` from the tool-call arguments.
 *
 * The shared shape is therefore `data.input` holding the tool's arguments, with
 * `text` as a stringified fallback. Everything here degrades to "unknown tool
 * with some arguments" rather than throwing, because a fifth runner will spell
 * something differently and a timeline that crashes on it is worse than one that
 * renders it plainly.
 */

import type { StreamEvent } from "@/lib/api";

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
  /** Glyph. Text rather than an icon font, so it costs no bytes. */
  icon: string;
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
const KIND_RULES: Array<{ kind: ToolKind; icon: string; patterns: string[] }> = [
  // Order matters: `multiedit` must be tested before `edit`, and `writefile`
  // before `file`, so the more specific rule wins.
  { kind: "edit", icon: "✎", patterns: ["multiedit", "strreplace", "applypatch", "edit", "patch"] },
  { kind: "write", icon: "◆", patterns: ["writefile", "createfile", "write", "create"] },
  { kind: "read", icon: "▤", patterns: ["readfile", "read", "cat", "view", "notebookread"] },
  { kind: "shell", icon: "❯", patterns: ["bash", "shell", "exec", "terminal", "command", "run"] },
  { kind: "search", icon: "⌕", patterns: ["grep", "glob", "search", "find", "ripgrep", "ls", "list"] },
  { kind: "task", icon: "◇", patterns: ["task", "agent", "todo", "plan", "delegate"] },
];

/** Argument keys that name a file, in the order agents prefer them. */
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
  const icon = rule?.icon ?? "⚙";

  const path = firstString(args, PATH_KEYS);
  const command = firstString(args, COMMAND_KEYS);
  const pattern = firstString(args, PATTERN_KEYS);
  const hunks = extractHunks(args, path);

  return {
    kind,
    name,
    icon,
    path,
    command,
    pattern,
    summary: summarize({ kind, name, path, command, pattern, args, hunks }),
    hunks,
    args: omitKeys(args, [
      ...PATH_KEYS,
      ...COMMAND_KEYS,
      ...PATTERN_KEYS,
      "old_string",
      "oldString",
      "new_string",
      "newString",
      "edits",
      "content",
      "replace_all",
      "replaceAll",
    ]),
  };
}

/**
 * Recover the argument object from an event.
 *
 * Three shapes are accepted because three exist in the wild: `data.input` as an
 * object (every runner's happy path), `data.input` as a JSON string (claude when
 * the tool input arrived as text), and `text` as raw JSON (claude's `text` field
 * always holds it, and is the only source when `data` is absent — which is the
 * case for `started` and `delta` phases).
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
    // Some runners put `command` at the top level of `data` rather than inside
    // `input`, so the outer object is worth reading before giving up on it.
    const outer = data as Record<string, unknown>;
    if (typeof outer.command === "string") return { command: outer.command };
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
  const before = source.old_string ?? source.oldString;
  const after = source.new_string ?? source.newString;
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
 * Exit code from a tool result, when the runner reported one.
 *
 * codex reports `exitCode`; opencode reports a `status` string. Both are read
 * because "did this command fail" is the single most useful fact in a shell
 * result, and it decides whether the output opens expanded.
 */
export function resultStatus(event: StreamEvent): { exitCode?: number; failed: boolean } {
  const failed = event.isError === true;
  const data = event.data;
  if (!data || typeof data !== "object") return { failed };
  const record = data as Record<string, unknown>;
  const raw = record.exitCode ?? record.exit_code;
  const exitCode = typeof raw === "number" ? raw : undefined;
  const status = typeof record.status === "string" ? record.status : undefined;
  return {
    exitCode,
    failed: failed || exitCode !== undefined && exitCode !== 0 || status === "failed" || status === "error",
  };
}
