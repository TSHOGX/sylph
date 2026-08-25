/**
 * What a leading slash means.
 *
 * Extracted from the composer because it is the one rule that decides whether a
 * keystroke becomes a turn, a picker, or a message — and a rule that important
 * should be assertable without a DOM.
 *
 * The distinction it encodes is the reason the session protocol has a v3. Every
 * slash command used to be delivered as prompt text, which is right for `/verify`
 * and wrong for `/model`: asked through a headless channel, "which model?" can only
 * be answered in prose, because there is no picker on the agent's side to open. So a
 * control command resolves against typed session config instead, and the picker is
 * the client's job.
 */

import type { SessionCommand } from "@/lib/api";

/** A leading slash and the command word, with whatever follows it. */
const INVOCATION = /^\/([^\s]+)\s*([\s\S]*)$/;

/**
 * The command a typed word names, matching aliases as well as canonical names.
 *
 * Aliases are matched because the agent honours them — `claude` answers `/cost` and
 * `/stats` for `/usage` — and a UI that ignores them sends a working command through
 * as prose.
 */
export function findCommand(
  commands: SessionCommand[],
  typed: string,
): SessionCommand | undefined {
  return commands.find(
    (command) => command.name === typed || (command.aliases ?? []).includes(typed),
  );
}

/**
 * Why the command menu has nothing to offer, or `null` when it has.
 *
 * `null` also covers "not typing a command", so a caller has one thing to check.
 *
 * This exists because the composer previously required at least one command
 * before opening the menu at all, which made `/` a dead keystroke whenever the
 * list was empty — and an empty list has three unrelated causes. A backend that
 * discovered no adapters, an agent still handshaking, and an agent that genuinely
 * declares no commands all looked identical, and all looked like a broken UI.
 */
export type SlashStatus = "connecting" | "unavailable" | "none" | "nomatch";

/**
 * Classify why nothing can be offered for what has been typed.
 *
 * Split out from the component so the precedence is assertable without a DOM.
 * The order matters and is not obvious: an in-flight handshake outranks a
 * failure, because a retry that is already running should not read as a failure
 * the user has to act on; and both outrank "this agent has none", because
 * claiming an agent declares no commands before it has answered is a statement
 * about the agent that we do not yet know to be true.
 */
export function slashStatus({
  typing,
  matchCount,
  commandCount,
  connecting,
  failed,
}: {
  /** Whether the text is a command being typed: a leading slash, no whitespace. */
  typing: boolean;
  matchCount: number;
  commandCount: number;
  connecting: boolean;
  failed: boolean;
}): SlashStatus | null {
  if (!typing || matchCount > 0) return null;
  // With commands in hand, an empty match set is about the query, not the agent.
  if (commandCount > 0) return "nomatch";
  if (connecting) return "connecting";
  if (failed) return "unavailable";
  return "none";
}

/** What submitting this text should do. */
export type CommandRoute =
  /** Run as a turn. `name` is canonical even when an alias was typed. */
  | { kind: "prompt"; name: string; rest: string }
  /** Open a picker. `configId` is absent when the command names no single option. */
  | { kind: "control"; configId: string | undefined }
  /** Send as an ordinary message. */
  | { kind: "message" };

/**
 * Route submitted text.
 *
 * An unknown command is deliberately **not** intercepted: it goes through as text
 * and the agent says it does not recognize it, which is what a terminal does. The
 * alternative is this UI deciding a command does not exist, when the agent is the
 * only thing that knows — and the command list can be stale by a keystroke.
 */
export function commandRoute(commands: SessionCommand[], text: string): CommandRoute {
  const match = INVOCATION.exec(text.trim());
  if (!match) return { kind: "message" };

  const command = findCommand(commands, match[1]);
  if (!command) return { kind: "message" };

  if (command.kind === "control") {
    return { kind: "control", configId: command.configId };
  }
  return { kind: "prompt", name: command.name, rest: match[2].trim() };
}
