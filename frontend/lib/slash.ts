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
