"use client";

/**
 * Which machines exist, and when it is worth saying so.
 *
 * # Why this is a module rather than a query at each call site
 *
 * Three surfaces need environments for three different reasons — the create flow
 * needs to *choose* one, a session row needs to *label* one, and the settings panel
 * lists what each can run — and two of them need the same non-obvious rule: **a
 * single-environment install must not render an environment anywhere.** Almost every
 * install is one laptop, and a chip repeating that laptop's name on every row is a
 * marker on the ordinary case, which is exactly the mistake the deleted origin badge
 * made. Deciding that per call site is three chances to decide it differently.
 *
 * # Why `reachableVia` needs resolving here
 *
 * The wire carries a relaying environment's **id**, and an id is opaque base32 — a
 * chip rendering it names nothing a user recognizes. Only a component holding the
 * whole list can turn it into a name, so the lookup lives beside the list rather than
 * being duplicated wherever a chip is drawn. Missing is rendered as missing: an id
 * naming an environment this client has not been told about is a real state during a
 * pairing race, and inventing a label for it would be worse than saying nothing.
 */

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type Environment } from "@/lib/api";

/**
 * The one query for environments, shared like `liveSessionsQuery`.
 *
 * Polled, not static. An environment's `connected` changes with no action on this
 * client — a laptop sleeps, a sandbox is paused on idle — and a picker offering a
 * machine that went away ten minutes ago produces a `503` the user cannot explain.
 * Slower than the sessions poll because the set of *machines* changes far less often
 * than what is running on them.
 */
const POLL_MS = 30_000;

export interface Environments {
  all: Environment[];
  /**
   * Whether an environment is worth naming on a row at all.
   *
   * False for the single-machine install, which is almost every install. Note this is
   * about *display*: the create flow still has to send an explicit environment even
   * when there is only one to send, because `POST /sessions` has no default.
   */
  showEnvironment: boolean;
  /** Display name for an id, or `undefined` when this client has never seen it. */
  nameOf: (id: string) => string | undefined;
  /** The environment record for an id, when known. */
  find: (id: string) => Environment | undefined;
  isPending: boolean;
  error: unknown;
}

export function useEnvironments(): Environments {
  const query = useQuery({
    queryKey: ["environments"],
    queryFn: () => api.environments.list(),
    refetchInterval: POLL_MS,
  });

  const all = query.data?.environments;
  const { isPending, error } = query;

  return useMemo(() => {
    // Indexed once rather than scanned per row: a session list renders up to 200
    // rows and each one resolves a name, and a relayed row resolves two.
    const byId = new Map((all ?? []).map((environment) => [environment.id, environment]));
    return {
      all: all ?? [],
      showEnvironment: byId.size > 1,
      nameOf: (id: string) => byId.get(id)?.name,
      find: (id: string) => byId.get(id),
      isPending,
      error,
    };
  }, [all, isPending, error]);
}

/**
 * The *name* of the machine relaying for this one, or null when it is direct.
 *
 * The one place the `reachableVia` id becomes something a chip may render. Two
 * distinct nulls collapse here deliberately, because the chip renders them the same:
 * an environment reached directly, and one whose relay this client has not been told
 * about. The second is a real state during a pairing race, and labelling it with the
 * raw base32 id would name nothing — a plain chip is the honest fallback, since the
 * chip's *warning* form exists to explain a specific machine's departure and cannot
 * do that without its name.
 */
export function relayName(
  environmentId: string,
  environments: Pick<Environments, "find" | "nameOf">,
): string | null {
  const via = environments.find(environmentId)?.reachableVia;
  return via ? (environments.nameOf(via) ?? null) : null;
}

/**
 * Every agent any reachable environment can run, for a surface that has not chosen
 * one yet.
 *
 * **This is deliberately not "the agent list".** There is no such thing: availability
 * is per machine, so a merged list is true of nowhere and offering `codex` from it
 * lets a user pick an agent the environment they then choose cannot run. It exists
 * for exactly one job — populating a *preference* (`settings.defaultAgent`), which
 * names an agent before any environment is in view — and the caller must validate the
 * choice against the chosen environment before creating anything.
 *
 * Disconnected environments are excluded: what they *could* run is unknowable while
 * nothing can ask them, and a stale answer here reads as an available agent.
 */
export function agentNamesAcross(environments: Environment[]): string[] {
  const names = new Set<string>();
  for (const environment of environments) {
    if (!environment.connected) continue;
    for (const agent of environment.agents) names.add(agent.name);
  }
  return [...names].sort();
}

/**
 * The environment a new session should default to.
 *
 * Connected first, because an unreachable default turns the primary action into a
 * `503`. Beyond that, a directly-reachable machine outranks a relayed one: control on
 * a relayed environment ends when its relay leaves, which is not what a default should
 * quietly opt a user into. `null` when nothing is connected, which the caller must
 * report rather than paper over — there is no environment to fall back to, and
 * `POST /sessions` requires one.
 */
export function defaultEnvironment(environments: Environment[]): Environment | null {
  const connected = environments.filter((environment) => environment.connected);
  return (
    connected.find((environment) => environment.reachableVia === null) ??
    connected[0] ??
    null
  );
}
