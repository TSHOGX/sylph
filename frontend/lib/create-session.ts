"use client";

/**
 * Creating a session with no dialog.
 *
 * The directory is the only thing a session genuinely needs beyond an agent,
 * and every caller here already knows it: Quick Start has the server-resolved
 * Quick Start directory, and a sidebar group *is* a directory. So the interaction
 * is a mutation, not a route — asking for a working directory that the invocation
 * already named is a form the user has to dismiss to get what they clicked.
 *
 * The directory picker remains the entry point for the two cases this cannot serve:
 * no directory chosen yet (`Mod+N`), and choosing a workspace before starting.
 */

import { useCallback, useMemo } from "react";
import { useRouter } from "next/navigation";
import { useMutation } from "@tanstack/react-query";
import { api, type Environment } from "@/lib/api";
import { defaultEnvironment, useEnvironments } from "@/lib/environments";
import { useSettings, type Settings } from "@/lib/settings";

/** Environment, agent, flags, and identity a new session gets when nobody picks them. */
export interface SessionDefaults {
  /**
   * Which machine to run on.
   *
   * **Required and never omitted.** `POST /sessions` has no default environment,
   * because there is no privileged machine — not even the one the server runs on
   * (`docs/environments.md` § Self-installed). A client that does not send this cannot
   * create a session at all, which is why resolution fails loudly below rather than
   * falling back to something.
   */
  environment: string;
  agent: string;
  /** Omitted rather than empty when no usable model default applies. */
  flags?: Record<string, string>;
  /**
   * Whether `SOUL.md` is appended.
   *
   * Always explicit, never omitted, even though `true` is also the backend's default.
   * An omitted field would make "the user asked for Sylph's identity" and "a client
   * that does not know about identity" the same request, and the created session's own
   * record is the only place this decision is ever readable afterwards.
   */
  injectIdentity: boolean;
}

/** Why defaults could not be resolved. Each has a different thing for a user to do. */
export type DefaultsProblem =
  /** Nothing is paired, or everything paired is offline. Wake a machine, or pair one. */
  | "no-environment"
  /** The chosen machine reports no agent CLI on its `PATH`. Install one there. */
  | "no-agent";

/**
 * Resolve the configured defaults against the machines and agents that actually exist.
 *
 * # Every fallback here is a stored preference outliving what it names
 *
 * A `defaultAgent` whose CLI was uninstalled would be sent verbatim and rejected as an
 * unknown agent; a `defaultModels` entry the agent no longer declares would be sent as
 * a flag and fail at turn time, well away from the setting that caused it. Checking
 * both against what the environment reports turns a stale preference into a silent,
 * correct fallback.
 *
 * # Agents are resolved *within* an environment, which is the change that matters
 *
 * Availability is per machine. `settings.defaultAgent` is a preference expressed with
 * no environment in view, so it names an agent that may exist on one machine and not
 * on the one this session is about to run on — resolving it against the chosen
 * environment's own list is what stops "start a session here" from failing with an
 * unknown agent on a machine where that agent was never installed. That is also why
 * this takes an `Environment` rather than an agent array: there is no global list to
 * pass.
 *
 * Returns a {@link DefaultsProblem} rather than `null`, because the two failures have
 * different fixes and a caller that cannot tell them apart can only say "something is
 * wrong": pair or wake a machine, against install a CLI on the one you have.
 */
export function resolveSessionDefaults(
  environment: Environment | null,
  settings: Pick<Settings, "defaultAgent" | "defaultModels" | "injectSoul">,
): SessionDefaults | DefaultsProblem {
  if (!environment) return "no-environment";

  const agents = environment.agents;
  const agent = agents.find((a) => a.name === settings.defaultAgent) ?? agents[0];
  if (!agent) return "no-agent";

  const model = settings.defaultModels[agent.name];
  const declared = agent.flags?.model?.choices ?? [];
  const usable = model && declared.includes(model) ? model : undefined;

  return {
    environment: environment.id,
    agent: agent.name,
    flags: usable ? { model: usable } : undefined,
    // Not validated against anything, unlike the two above: identity is a policy
    // choice the backend accepts from any client, not a value that has to exist in an
    // agent's descriptor. An absent SOUL.md makes `true` behave like `false` anyway,
    // decided at spawn time by the envagent rather than here.
    injectIdentity: settings.injectSoul,
  };
}

/** Whether resolution produced defaults or a problem. Narrows for the caller. */
export function isDefaultsProblem(
  resolved: SessionDefaults | DefaultsProblem,
): resolved is DefaultsProblem {
  return typeof resolved === "string";
}

export interface QuickCreate {
  /** Create in `workdir` and navigate to the new session. */
  create: (workdir: string | undefined, environmentId?: string | null) => void;
  isPending: boolean;
  error: unknown;
}

/**
 * Create a Sylph session in a known directory, then open it.
 *
 * One instance of this lives in the shell and is shared through
 * `WorkspaceUIProvider`, so the top bar and the sidebar report a single pending
 * state and a single error banner instead of each owning their own.
 */
export function useQuickCreateSession(): QuickCreate {
  const router = useRouter();
  const { settings } = useSettings();

  const environments = useEnvironments();

  const mutation = useMutation({
    mutationFn: async ({ workdir, environmentId }: { workdir: string | undefined; environmentId?: string | null }) => {
      // Falsy covers both "the workspaces query has not answered yet" and the
      // sidebar's placeholder Quick Sessions group, whose path is `""` until it
      // does. Sending either would create a session in an unusable directory.
      if (!workdir) {
        throw new Error("Still resolving the directory. Try again in a moment.");
      }
      const resolved = resolveSessionDefaults(
        environmentId
          ? environments.all.find((environment) => environment.id === environmentId) ?? null
          : defaultEnvironment(environments.all),
        settings,
      );
      if (isDefaultsProblem(resolved)) {
        // Two distinct messages, because the fixes differ and a merged one would send
        // the reader to the wrong place. This path has no environment picker by
        // design — it exists so that clicking a directory creates a session — so the
        // message is the only thing telling the user what to go and do.
        throw new Error(
          resolved === "no-environment"
            ? "No machine is connected, so there is nowhere to start a session. Pair one, or wake the machine you have."
            : "That machine reports no agent CLIs on its PATH, so there is nothing to start a session with.",
        );
      }
      return api.sessions.create({
        environment: resolved.environment,
        agent: resolved.agent,
        workdir,
        flags: resolved.flags,
        injectIdentity: resolved.injectIdentity,
      });
    },
    onSuccess: ({ draftId }) => {
      // A prepared link is intentionally absent from the sidebar. It becomes a
      // durable row only when the draft page sends its first real turn.
      router.push(`/sessions/draft/${encodeURIComponent(draftId)}`);
    },
  });

  // Memoized, and `mutate` passed by reference rather than wrapped in a fresh
  // arrow. This value is threaded into the shell context, whose memo would
  // otherwise be invalidated on every render of the shell — re-rendering every
  // consumer of `useWorkspaceUI`, and re-registering the document-level keydown
  // listener each time, because `runAction` closes over it.
  const { mutate, isPending, error } = mutation;
  const create = useCallback(
    (workdir: string | undefined, environmentId?: string | null) => mutate({ workdir, environmentId }),
    [mutate],
  );
  return useMemo(() => ({ create, isPending, error }), [create, isPending, error]);
}
