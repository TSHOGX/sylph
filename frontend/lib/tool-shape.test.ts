import { describe, expect, it } from "vitest";
import { describeTool } from "@/lib/tool-shape";
import type { StreamEvent } from "@/lib/api-types";

function event(partial: Partial<StreamEvent>): StreamEvent {
  return {
    seq: 1,
    kind: "tool_call",
    stability: "stable",
    source: "agent",
    createdAtMs: 0,
    ...partial,
  };
}

describe("describeTool", () => {
  it("uses the server description even when the tool name suggests another category", () => {
    const shape = describeTool(event({
      name: "CustomRead",
      text: "unrelated preview",
      toolSummary: {
        kind: "shell", name: "CustomRead", summary: "cargo test", command: "cargo test",
      },
    }));
    expect(shape.kind).toBe("shell");
    expect(shape.summary).toBe("cargo test");
    expect(shape.command).toBe("cargo test");
  });

  it("extracts complete edit details independently of bounded header paths", () => {
    const shape = describeTool(event({
      name: "Edit",
      toolSummary: { kind: "edit", name: "Edit", summary: "src/lib.rs", path: "src/li" },
      data: { input: { file_path: "src/lib.rs", old_string: "old", new_string: "new" } },
    }));
    expect(shape.path).toBe("src/lib.rs");
    expect(shape.hunks).toEqual([{ path: "src/lib.rs", before: "old", after: "new" }]);
    expect(shape.args).not.toHaveProperty("old_string");
    expect(shape.args).not.toHaveProperty("new_string");
  });

  it("preserves multiple edits and newly written content", () => {
    const multiple = describeTool(event({
      data: { input: { edits: [
        { old_string: "a", new_string: "A" },
        { old_string: "b", new_string: "B" },
      ] } },
    }));
    expect(multiple.hunks.map(hunk => hunk.after)).toEqual(["A", "B"]);
    const written = describeTool(event({ data: { input: { path: "new.txt", content: "hello" } } }));
    expect(written.hunks).toEqual([{ path: "new.txt", before: "", after: "hello" }]);
  });

  it("keeps Codex live execution details", () => {
    const shape = describeTool(event({
      name: "exec",
      data: { command: "cargo test", cwd: "/workspace" },
      toolSummary: { kind: "shell", name: "exec", summary: "cargo test", command: "cargo test" },
    }));
    expect(shape.command).toBe("cargo test");
    expect(shape.args).toEqual({ cwd: "/workspace" });
  });

  it("keeps unfamiliar and partial payloads renderable", () => {
    for (const data of [null, "a string", 42, [], { input: null }, { input: [] }, { input: { path: null } }]) {
      expect(() => describeTool(event({ name: "x", data }))).not.toThrow();
    }
    expect(describeTool(event({ name: "Edit", text: '{"file_pa' })).hunks).toEqual([]);
  });
});
