/**
 * Matching and routing a slash command.
 *
 * Two decisions the composer makes from a command list, both of which were wrong
 * before the v3 protocol carried enough to make them:
 *
 * * **Aliases count.** The agent answers to them, so a menu that does not match
 *   them finds nothing for a name that works — `/cost` and `/stats` are `/usage`.
 * * **A control command is not a turn.** Sent as prompt text to a headless CLI,
 *   "which model?" comes back as a paragraph, because there is no picker on that
 *   side of the wire. The picker has to be the client's.
 *
 * These are pure functions rather than component tests on purpose: the routing rule
 * is the thing worth pinning, and it should not need a DOM to assert.
 */

import { describe, expect, it } from "vitest";
import { slashMatches } from "@/components/slash-menu";
import { commandRoute, findCommand } from "@/lib/slash";
import type { SessionCommand } from "@/lib/api";

const COMMANDS: SessionCommand[] = [
  { name: "verify", description: "Verify a change end to end", kind: "prompt" },
  { name: "code-review", description: "Review the current diff", kind: "prompt" },
  {
    name: "usage",
    description: "Show session cost",
    kind: "prompt",
    aliases: ["cost", "stats"],
  },
  { name: "model", description: "Choose the model", kind: "control", configId: "model" },
  { name: "config", description: "Session settings", kind: "control" },
];

describe("findCommand", () => {
  it("matches a command by its own name", () => {
    expect(findCommand(COMMANDS, "verify")?.name).toBe("verify");
  });

  it("matches an alias the agent answers to", () => {
    // `claude` ships these, and the terminal honours them. A UI that only matches
    // the canonical name sends `/cost` through as plain prose.
    expect(findCommand(COMMANDS, "cost")?.name).toBe("usage");
    expect(findCommand(COMMANDS, "stats")?.name).toBe("usage");
  });

  it("does not match a name no command claims", () => {
    expect(findCommand(COMMANDS, "nonesuch")).toBeUndefined();
  });
});

describe("commandRoute", () => {
  it("routes a prompt command to a turn, carrying its arguments", () => {
    expect(commandRoute(COMMANDS, "/verify the auth change")).toEqual({
      kind: "prompt",
      name: "verify",
      rest: "the auth change",
    });
  });

  it("routes a control command to its config option, never to a turn", () => {
    expect(commandRoute(COMMANDS, "/model")).toEqual({
      kind: "control",
      configId: "model",
    });
  });

  it("routes a control command with no config option to the whole config surface", () => {
    // `/config` names no single option, so it opens all of them rather than
    // nothing — a command the agent offered must do something when chosen.
    expect(commandRoute(COMMANDS, "/config")).toEqual({
      kind: "control",
      configId: undefined,
    });
  });

  it("sends an unknown command as text rather than guessing", () => {
    // What a terminal does. Intercepting it would mean this UI deciding a command
    // does not exist, when the agent is the only thing that knows.
    expect(commandRoute(COMMANDS, "/nonesuch and some args")).toEqual({
      kind: "message",
    });
  });

  it("sends ordinary prose as a message", () => {
    expect(commandRoute(COMMANDS, "fix the failing test")).toEqual({ kind: "message" });
  });

  it("treats a bare slash as a message rather than a command", () => {
    expect(commandRoute(COMMANDS, "/")).toEqual({ kind: "message" });
  });

  it("routes an alias the same way as the command it names", () => {
    expect(commandRoute(COMMANDS, "/cost")).toEqual({
      kind: "prompt",
      name: "usage",
      rest: "",
    });
  });

  it("treats a mid-sentence slash as prose", () => {
    // Only a *leading* slash is a command. "and/or" is not an invocation.
    expect(commandRoute(COMMANDS, "use and/or here")).toEqual({ kind: "message" });
  });
});

describe("slashMatches ranking", () => {
  it("prefers a command named for the query over one that merely mentions it", () => {
    const matches = slashMatches(COMMANDS, "verify");
    expect(matches[0].name).toBe("verify");
  });

  it("finds a command through its alias", () => {
    expect(slashMatches(COMMANDS, "stats").map((c) => c.name)).toContain("usage");
  });

  it("offers everything when nothing has been typed yet", () => {
    expect(slashMatches(COMMANDS, "")).toHaveLength(COMMANDS.length);
  });

  it("has nothing to offer for an agent that reports no commands", () => {
    expect(slashMatches([], "ver")).toEqual([]);
  });
});
