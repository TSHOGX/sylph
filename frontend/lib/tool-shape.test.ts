import { describe, expect, it } from "vitest";
import { basename, describeTool, resultStatus } from "@/lib/tool-shape";
import type { StreamEvent } from "@/lib/api";

/**
 * Tool interpretation across agent vocabularies.
 *
 * The payloads here are the shapes the shipped adapters actually emit —
 * `adapters/claude/adapter.py` and `adapters/pi/adapter.py` — transcribed from
 * their source. That is the point of these tests: the session protocol leaves
 * `data` unspecified beyond `input`, so the only guarantee is what those two
 * adapters do, and a change in either should fail here rather than silently
 * render a tool call as "unknown". Nothing here pins a shape no producer emits.
 */
function event(partial: Partial<StreamEvent>): StreamEvent {
  return {
    seq: 1,
    kind: "tool_call",
    stability: "stable",
    source: "runner",
    createdAtMs: 0,
    ...partial,
  };
}

describe("describeTool", () => {
  it("reads claude's parsed tool input", () => {
    // adapters/claude/adapter.py:1108: `data = {"input": json.loads(block["text"])}`
    const shape = describeTool(
      event({
        name: "Read",
        text: '{"file_path":"backend/src/auth.rs"}',
        data: { input: { file_path: "backend/src/auth.rs" } },
      }),
    );
    expect(shape.kind).toBe("read");
    expect(shape.path).toBe("backend/src/auth.rs");
    expect(shape.summary).toBe("backend/src/auth.rs");
  });

  it("falls back to parsing text when data is absent", () => {
    // The `started` and `delta` phases carry no `data`, so the raw JSON in `text`
    // is the only source — this is what lets a call render before it completes.
    const shape = describeTool(
      event({ name: "Edit", phase: "started", text: '{"file_path":"a/b.ts"}' }),
    );
    expect(shape.path).toBe("a/b.ts");
  });

  it("survives the partial JSON a streaming tool call produces", () => {
    // claude streams tool input as `partial_json` deltas, so a mid-stream parse
    // failure is the normal case, not an error.
    const shape = describeTool(event({ name: "Edit", phase: "delta", text: '{"file_pa' }));
    expect(shape.kind).toBe("edit");
    expect(shape.path).toBeUndefined();
    // It still names something rather than rendering a blank row.
    expect(shape.summary).toBe("Edit");
  });

  it("treats a non-JSON string input as a command", () => {
    // adapters/claude/adapter.py:1110 falls back to the raw block text when the
    // streamed partial JSON did not parse, and a shell command is the commonest
    // thing that leaves behind.
    const shape = describeTool(
      event({ name: "bash", data: { input: "ls -la /tmp" } }),
    );
    expect(shape.command).toBe("ls -la /tmp");
  });

  it("classifies tool names across vocabularies and affixes", () => {
    const kindOf = (name: string) => describeTool(event({ name })).kind;
    expect(kindOf("Read")).toBe("read");
    expect(kindOf("read_file")).toBe("read");
    expect(kindOf("mcp__fs__read_file")).toBe("read");
    expect(kindOf("Write")).toBe("write");
    expect(kindOf("Bash")).toBe("shell");
    expect(kindOf("Grep")).toBe("search");
    expect(kindOf("Glob")).toBe("search");
    expect(kindOf("Task")).toBe("task");
    expect(kindOf("SomethingNovel")).toBe("other");
  });

  it("prefers the more specific rule for MultiEdit and StrReplace", () => {
    // `multiedit` contains `edit`, and `str_replace_editor` contains both `edit`
    // and `replace`. Either landing on the wrong branch would lose the diff.
    expect(describeTool(event({ name: "MultiEdit" })).kind).toBe("edit");
    expect(describeTool(event({ name: "str_replace_editor" })).kind).toBe("edit");
  });

  it("extracts a diffable hunk from an Edit", () => {
    const shape = describeTool(
      event({
        name: "Edit",
        data: {
          input: {
            file_path: "src/lib.rs",
            old_string: "let x = 1;",
            new_string: "let x = 2;",
          },
        },
      }),
    );
    expect(shape.hunks).toHaveLength(1);
    expect(shape.hunks[0]).toMatchObject({
      path: "src/lib.rs",
      before: "let x = 1;",
      after: "let x = 2;",
    });
    // Diff inputs are consumed, not repeated under "Arguments".
    expect(shape.args).not.toHaveProperty("old_string");
    expect(shape.args).not.toHaveProperty("new_string");
  });

  it("extracts every hunk from a MultiEdit", () => {
    const shape = describeTool(
      event({
        name: "MultiEdit",
        data: {
          input: {
            file_path: "src/lib.rs",
            edits: [
              { old_string: "a", new_string: "A" },
              { old_string: "b", new_string: "B" },
            ],
          },
        },
      }),
    );
    expect(shape.hunks).toHaveLength(2);
    expect(shape.hunks.map((hunk) => hunk.after)).toEqual(["A", "B"]);
  });

  it("renders a Write as an all-additions hunk", () => {
    // Creating a file has no "before", and showing the new content as additions
    // is exactly what a creation is.
    const shape = describeTool(
      event({
        name: "Write",
        data: { input: { file_path: "new.txt", content: "hello\nworld" } },
      }),
    );
    expect(shape.hunks).toHaveLength(1);
    expect(shape.hunks[0].before).toBe("");
    expect(shape.hunks[0].after).toBe("hello\nworld");
  });

  it("summarizes an unrecognized tool from its first scalar argument", () => {
    const shape = describeTool(
      event({ name: "weird_tool", data: { input: { subject: "the thing" } } }),
    );
    expect(shape.kind).toBe("other");
    expect(shape.summary).toBe("subject: the thing");
  });

  it("never throws on a malformed or hostile payload", () => {
    // Tool argument keys come from whatever MCP servers, plugins, and user
    // adapters are installed, so an unfamiliar shape is expected. A timeline that
    // crashes on one is worse than one that renders it plainly.
    const payloads: unknown[] = [
      null,
      "a string",
      42,
      [],
      { input: null },
      { input: [] },
      { input: { path: null } },
    ];
    for (const data of payloads) {
      expect(() => describeTool(event({ name: "x", data }))).not.toThrow();
    }
    expect(() => describeTool(event({ name: undefined }))).not.toThrow();
  });
});

describe("resultStatus", () => {
  it("treats isError as failure", () => {
    // The declared outcome of a `tool_result` (docs/session-protocol.md:663), set
    // by adapters/claude/adapter.py:1167 and adapters/pi/adapter.py:497.
    expect(resultStatus(event({ kind: "tool_result", isError: true })).failed).toBe(true);
  });

  it("reports a non-zero data.exitCode as failure", () => {
    // The only producer is the E2E fixture
    // (`crates/adapters/src/e2e.rs`, `tool`); `components/timeline.tsx:942`
    // renders "failed (exit N)" from it. Pending a protocol change declaring the
    // field, this pins the read that surface depends on.
    const status = resultStatus(event({ kind: "tool_result", data: { exitCode: 101 } }));
    expect(status.exitCode).toBe(101);
    expect(status.failed).toBe(true);
  });

  it("reports data.exitCode zero as success", () => {
    const status = resultStatus(event({ kind: "tool_result", data: { exitCode: 0 } }));
    expect(status.exitCode).toBe(0);
    expect(status.failed).toBe(false);
  });

  it("ignores a status string on a tool result", () => {
    // No adapter writes one, so a `status` on `data` must not decide the outcome:
    // `isError` is what `docs/session-protocol.md:663` declares.
    expect(resultStatus(event({ kind: "tool_result", data: { status: "error" } })).failed).toBe(
      false,
    );
  });

  it("defaults to success when nothing is reported", () => {
    expect(resultStatus(event({ kind: "tool_result" })).failed).toBe(false);
  });
});

describe("basename", () => {
  it("takes the last path component", () => {
    expect(basename("a/b/c.rs")).toBe("c.rs");
    expect(basename("/abs/path/")).toBe("path");
    expect(basename("bare")).toBe("bare");
  });
});
