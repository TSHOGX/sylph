import { describe, expect, it } from "vitest";
import { basename, describeTool, resultStatus } from "@/lib/tool-shape";
import type { StreamEvent } from "@/lib/api";

/**
 * Tool interpretation across agent vocabularies.
 *
 * The payloads here are the shapes the four shipped runners actually emit,
 * transcribed from their source. That is the point of these tests: the runner
 * protocol leaves `data` unspecified, so the only guarantee is what the runners
 * do, and a change in one of them should fail here rather than silently render a
 * tool call as "unknown".
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
    // runners/claude/runner.py: `fields["data"] = {"input": json.loads(text)}`
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

  it("reads codex's command field", () => {
    // runners/codex/runner.sh puts the command in both `input` and `command`.
    const shape = describeTool(
      event({
        name: "shell",
        text: "cargo test --lib",
        data: { status: "completed", command: "cargo test --lib" },
      }),
    );
    expect(shape.kind).toBe("shell");
    expect(shape.command).toBe("cargo test --lib");
    expect(shape.summary).toBe("cargo test --lib");
  });

  it("treats a non-JSON string input as a command", () => {
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
    // A fifth runner will spell something differently, and a timeline that
    // crashes on it is worse than one that renders it plainly.
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
    expect(resultStatus(event({ kind: "tool_result", isError: true })).failed).toBe(true);
  });

  it("reads codex's exit code", () => {
    const status = resultStatus(event({ kind: "tool_result", data: { exitCode: 101 } }));
    expect(status.exitCode).toBe(101);
    expect(status.failed).toBe(true);
  });

  it("treats exit code zero as success", () => {
    const status = resultStatus(event({ kind: "tool_result", data: { exitCode: 0 } }));
    expect(status.exitCode).toBe(0);
    expect(status.failed).toBe(false);
  });

  it("reads opencode's status string", () => {
    expect(resultStatus(event({ kind: "tool_result", data: { status: "error" } })).failed).toBe(
      true,
    );
    expect(
      resultStatus(event({ kind: "tool_result", data: { status: "completed" } })).failed,
    ).toBe(false);
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
