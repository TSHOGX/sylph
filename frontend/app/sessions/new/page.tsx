"use client";

import { useMutation, useQuery } from "@tanstack/react-query";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useMemo, useState } from "react";
import { api, ApiError } from "@/lib/api";

export default function NewSessionPage() {
  const router = useRouter();
  const [chosenAgent, setChosenAgent] = useState("");
  const [workdir, setWorkdir] = useState("");
  const [title, setTitle] = useState("");
  /**
   * Flag values tagged with the agent they were chosen for.
   *
   * Flags are declared per-agent by `--describe`, so switching agents must not
   * carry values the new runner never declared. Tagging the selection makes
   * that a derivation rather than a reset effect.
   */
  const [flagState, setFlagState] = useState<{
    agent: string;
    values: Record<string, string>;
  }>({ agent: "", values: {} });
  const [validation, setValidation] = useState<string | null>(null);

  const agentsQuery = useQuery({
    queryKey: ["agents"],
    queryFn: () => api.agents.list(),
  });

  const agents = useMemo(() => agentsQuery.data?.agents ?? [], [agentsQuery.data]);

  // Default to the first discovered runner instead of a hardcoded name: the
  // list comes from `--describe`, so hardcoding "claude" would offer an agent
  // that may not be installed.
  const agent = chosenAgent || agents[0]?.name || "";
  const selected = agents.find((a) => a.name === agent);
  const flags = flagState.agent === agent ? flagState.values : {};

  const createSession = useMutation({
    mutationFn: () =>
      api.sessions.create({
        agent,
        workdir: workdir.trim(),
        title: title.trim() || undefined,
        flags: Object.keys(flags).length > 0 ? flags : undefined,
      }),
    onSuccess: (result) => router.push(`/sessions/${result.session.id}`),
  });

  function setFlag(name: string, value: string) {
    setFlagState((prev) => {
      const values = prev.agent === agent ? { ...prev.values } : {};
      if (value) values[name] = value;
      else delete values[name];
      return { agent, values };
    });
  }

  function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!agent) {
      setValidation("Choose an agent.");
      return;
    }
    if (!workdir.trim()) {
      setValidation("A working directory is required — it is where the agent runs.");
      return;
    }
    setValidation(null);
    createSession.mutate();
  }

  const noAgents = !agentsQuery.isPending && agents.length === 0;

  return (
    <div className="min-h-screen bg-white">
      <header className="border-b">
        <div className="container mx-auto px-4 py-3">
          <Link
            href="/"
            className="text-sm text-gray-500 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            ← All sessions
          </Link>
        </div>
      </header>

      <main id="main" className="container mx-auto max-w-lg px-4 py-8">
        <h1 className="text-2xl font-semibold">New session</h1>
        <p className="mt-1 text-sm text-gray-500">
          Creates a Sylph session: Sylph identity is injected and Sylph lifecycle policy
          applies.
        </p>

        {noAgents ? (
          <div
            role="alert"
            className="mt-6 rounded-lg border border-amber-200 bg-amber-50 p-4 text-sm"
          >
            <p className="font-medium text-amber-900">No agents discovered</p>
            <p className="mt-1 text-amber-800">
              The backend found no working runners. Check that the runners directory
              contains executables answering <code>--describe</code>, then refresh.
            </p>
            <button
              type="button"
              onClick={() => void api.agents.refresh().then(() => agentsQuery.refetch())}
              className="mt-3 rounded border border-amber-300 bg-white px-3 py-1.5 text-sm font-medium hover:bg-amber-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-amber-600"
            >
              Rescan runners
            </button>
          </div>
        ) : (
          <form onSubmit={submit} className="mt-6 space-y-5">
            <div>
              <label htmlFor="agent" className="block text-sm font-medium">
                Agent
              </label>
              <select
                id="agent"
                value={agent}
                onChange={(e) => setChosenAgent(e.target.value)}
                disabled={agentsQuery.isPending}
                className="mt-1 w-full rounded-lg border px-3 py-2 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              >
                {agentsQuery.isPending && <option value="">Loading agents...</option>}
                {agents.map((a) => (
                  <option key={a.name} value={a.name}>
                    {a.name}
                  </option>
                ))}
              </select>
              {selected?.description && (
                <p className="mt-1 text-xs text-gray-500">{selected.description}</p>
              )}
            </div>

            <div>
              <label htmlFor="workdir" className="block text-sm font-medium">
                Working directory
              </label>
              <input
                id="workdir"
                type="text"
                value={workdir}
                onChange={(e) => setWorkdir(e.target.value)}
                placeholder="/Users/you/project"
                aria-describedby="workdir-help"
                className="mt-1 w-full rounded-lg border px-3 py-2 font-mono text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              />
              <p id="workdir-help" className="mt-1 text-xs text-gray-500">
                Absolute path. It must already exist on this machine.
              </p>
            </div>

            <div>
              <label htmlFor="title" className="block text-sm font-medium">
                Title <span className="font-normal text-gray-500">(optional)</span>
              </label>
              <input
                id="title"
                type="text"
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                placeholder="Refactor the parser"
                className="mt-1 w-full rounded-lg border px-3 py-2 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              />
            </div>

            {/*
              Flag selectors are generated from the runner's own --describe
              output, so a new agent gets its knobs with no frontend change.
              Values apply to every turn in the session; the backend drops keys
              the runner did not declare.
            */}
            {selected?.flags &&
              Object.entries(selected.flags).map(([name, flag]) => (
                <div key={name}>
                  <label htmlFor={`flag-${name}`} className="block text-sm font-medium capitalize">
                    {name}
                  </label>
                  <select
                    id={`flag-${name}`}
                    value={flags[name] ?? ""}
                    onChange={(e) => setFlag(name, e.target.value)}
                    className="mt-1 w-full rounded-lg border px-3 py-2 text-sm focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                  >
                    <option value="">Agent default</option>
                    {flag.choices?.map((choice) => (
                      <option key={choice} value={choice}>
                        {choice}
                      </option>
                    ))}
                  </select>
                  {flag.description && (
                    <p className="mt-1 text-xs text-gray-500">{flag.description}</p>
                  )}
                </div>
              ))}

            {validation && (
              <p role="alert" className="text-sm text-red-700">
                {validation}
              </p>
            )}

            {createSession.error && (
              <div
                role="alert"
                className="rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800"
              >
                {describeCreateError(createSession.error)}
              </div>
            )}

            <div className="flex gap-3">
              <button
                type="button"
                onClick={() => router.push("/")}
                className="flex-1 rounded-lg border px-4 py-2 text-sm font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={createSession.isPending}
                className="flex-1 rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
              >
                {createSession.isPending ? "Creating..." : "Create session"}
              </button>
            </div>
          </form>
        )}
      </main>
    </div>
  );
}

function describeCreateError(error: unknown): string {
  if (error instanceof ApiError && error.code === "INVALID_REQUEST") {
    // The backend distinguishes a missing directory from an unknown agent;
    // both arrive as INVALID_REQUEST, and its message names which.
    return error.message;
  }
  return error instanceof Error ? error.message : "Could not create the session.";
}
