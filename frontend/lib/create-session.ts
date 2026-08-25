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
 * `/sessions/new` still exists, for the two cases this cannot serve: no
 * directory chosen yet (`Mod+N`), and overriding the agent, model, or title.
 */

import { useMemo } from "react";
import { useRouter } from "next/navigation";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Agent, type IdentityMode } from "@/lib/api";
import { useSettings, type Settings } from "@/lib/settings";

/** Agent, flags, and identity a new session gets when nobody picks them. */
export interface SessionDefaults {
  agent: string;
  /** Omitted rather than empty when no usable model default applies. */
  flags?: Record<string, string>;
  /**
   * Identity policy for the session.
   *
   * Always explicit, never omitted, even though `sylph` is also the backend's
   * default. An omitted field would make "the user chose Sylph identity" and "an
   * old client that does not know about identity" the same request, and the
   * created session's own record is the only place this decision is ever
   * readable afterwards.
   */
  identityMode: IdentityMode;
}

/**
 * Resolve the configured defaults against the runners that actually exist.
 *
 * Both fallbacks matter, and both are about stored preferences outliving the
 * thing they name. A `defaultAgent` whose runner was uninstalled would be sent
 * verbatim and rejected by the backend as an unknown agent; a `defaultModels`
 * entry the runner no longer declares would be passed as `--flag model=...` and
 * fail at turn time, well away from the setting that caused it. Checking both
 * against `--describe` output turns a stale preference into a silent, correct
 * fallback. This mirrors what `/sessions/new` does with its select elements, so
 * the two paths cannot drift into different notions of "the default".
 *
 * Returns `null` when no runner was discovered at all — the one case with no
 * sensible default, which the caller must report rather than paper over.
 */
export function resolveSessionDefaults(
  agents: Agent[],
  settings: Pick<Settings, "defaultAgent" | "defaultModels" | "injectSoul">,
): SessionDefaults | null {
  const agent = agents.find((a) => a.name === settings.defaultAgent) ?? agents[0];
  if (!agent) return null;

  const model = settings.defaultModels[agent.name];
  const declared = agent.flags?.model?.choices ?? [];
  const usable = model && declared.includes(model) ? model : undefined;

  return {
    agent: agent.name,
    flags: usable ? { model: usable } : undefined,
    // Not validated against anything, unlike the two above: identity is a policy
    // choice the backend accepts from any client, not a value that has to exist
    // in a runner's descriptor. A missing SOUL.md makes `sylph` behave like
    // `preserve` anyway, decided at spawn time by the runner rather than here.
    identityMode: settings.injectSoul ? "sylph" : "preserve",
  };
}

export interface QuickCreate {
  /** Create in `workdir` and navigate to the new session. */
  create: (workdir: string | undefined) => void;
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
  const queryClient = useQueryClient();
  const { settings } = useSettings();

  const agentsQuery = useQuery({ queryKey: ["agents"], queryFn: () => api.agents.list() });

  const mutation = useMutation({
    mutationFn: async (workdir: string | undefined) => {
      // Falsy covers both "the workspaces query has not answered yet" and the
      // sidebar's placeholder Quick Sessions group, whose path is `""` until it
      // does. Sending either would create a session in an unusable directory.
      if (!workdir) {
        throw new Error("Still resolving the directory. Try again in a moment.");
      }
      const defaults = resolveSessionDefaults(agentsQuery.data?.agents ?? [], settings);
      if (!defaults) {
        throw new Error(
          "No agent runners were discovered, so there is nothing to start a session with.",
        );
      }
      return api.sessions.create({
        agent: defaults.agent,
        workdir,
        flags: defaults.flags,
        identityMode: defaults.identityMode,
      });
    },
    onSuccess: ({ session }) => {
      // The sidebar and the workspace roll-up both list this session. Without
      // invalidating them it is missing until the sidebar's 10-second poll fires,
      // long enough that the group it was created in looks unchanged.
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      router.push(`/sessions/${session.id}`);
    },
  });

  // Memoized, and `mutate` passed by reference rather than wrapped in a fresh
  // arrow. This value is threaded into the shell context, whose memo would
  // otherwise be invalidated on every render of the shell — re-rendering every
  // consumer of `useWorkspaceUI`, and re-registering the document-level keydown
  // listener each time, because `runAction` closes over it.
  const { mutate, isPending, error } = mutation;
  return useMemo(
    () => ({ create: mutate, isPending, error }),
    [mutate, isPending, error],
  );
}
