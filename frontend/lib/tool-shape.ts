/**
 * Tool headers use the server's bounded semantic description, the same producer
 * that owns aggregate command/read counts. Only expanded edit details are parsed
 * here, from the complete vendor payload supplied by EventBody.
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
  const description = event.toolSummary;
  const kind = description?.kind ?? "other";
  // Expanded paths come from full arguments, since the header is a preview.
  const path = firstString(args, PATH_KEYS) ?? description?.path;
  const command = description?.command;
  const pattern = description?.pattern;
  const hunks = extractHunks(args, path);

  return {
    kind,
    name,
    path,
    command,
    pattern,
    summary: description?.summary ?? name,
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
 * The shipped adapters supply nested input or Codex live command details:
 * `data.input` as an object (Claude, Pi, and native Codex), `data.input` as a string
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
    // Codex live details keep command/cwd beside status and outcome metadata.
    if (typeof (data as Record<string, unknown>).command === "string") {
      return data as Record<string, unknown>;
    }
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
