"use client";

import { useRouter, useParams } from "next/navigation";
import { useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { api, type ConfigOption, type LinkState, type PreparedCommandResult } from "@/lib/api";
import { Composer } from "@/components/composer";
import type { StagedAttachment } from "@/components/attachment-picker";
import { useEnvironments } from "@/lib/environments";

/**
 * Ephemeral prepared-session surface.
 *
 * A draft owns a live adapter link, not a durable identity.  The route therefore
 * deliberately has no history queries and the shell's file API addresses the draft
 * handle through its prepared routes until the first turn promotes it.
 */
export default function DraftSessionPage() {
  const params = useParams<{ id: string }>();
  const draftId = decodeURIComponent(params.id ?? "");
  const router = useRouter();
  const queryClient = useQueryClient();
  const environments = useEnvironments();
  const [state, setState] = useState<LinkState | null>(null);
  const [metadata, setMetadata] = useState<{
    environment: string;
    agent: string;
    workdir: string;
    title: string | null;
    injectIdentity: boolean;
  } | null>(null);
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<StagedAttachment[]>([]);
  const [sending, setSending] = useState(false);
  const [accepted, setAccepted] = useState<PreparedCommandResult | null>(null);
  const [switchingAgent, setSwitchingAgent] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // React Strict Mode probes effects by running setup → cleanup → setup.  Discarding
  // synchronously in that cleanup deleted a perfectly valid draft before its first
  // capability/config request could finish, producing the intermittent "unknown
  // prepared session" errors.  A generation check lets the probe's cleanup cancel
  // itself while still discarding the draft when the route genuinely unmounts.
  const generation = useRef(0);
  const activeDraftId = useRef(draftId);
  const promoted = useRef(false);
  useEffect(() => {
    // Keep the currently mounted route id separately from the generation. React
    // Strict Mode re-runs the same effect with the same id, while a real navigation
    // from one draft to another runs cleanup for the old id before setup for the new
    // one. That distinction is what lets cleanup discard a replaced draft without
    // deleting the draft during Strict Mode's development probe.
    activeDraftId.current = draftId;
    const current = ++generation.current;
    return () => {
      queueMicrotask(() => {
        // The generation is intentionally read at execution time: a Strict Mode
        // probe may have installed the next generation before this microtask runs.
        if (
          // eslint-disable-next-line react-hooks/exhaustive-deps
          (generation.current === current || activeDraftId.current !== draftId) &&
          !promoted.current
        ) {
          void api.sessions.discardPrepared(draftId);
        }
      });
    };
  }, [draftId]);

  useEffect(() => {
    let cancelled = false;
    void api.sessions.preparedCapabilities(draftId).then((value) => {
      if (!cancelled) setState(value);
    }).catch((reason: unknown) => {
      if (!cancelled) {
        setError(reason instanceof Error ? reason.message : "Could not load agent capabilities");
      }
    });
    return () => { cancelled = true; };
  }, [draftId]);

  useEffect(() => {
    let cancelled = false;
    void api.sessions.preparedGet(draftId).then((value) => {
      if (!cancelled) setMetadata(value);
    }).catch((reason: unknown) => {
      if (!cancelled) {
        setError(reason instanceof Error ? reason.message : "Could not load the new session");
      }
    });
    return () => { cancelled = true; };
  }, [draftId]);

  const environment = environments.find(metadata?.environment ?? "");
  const agentOptions = useMemo(() => {
    const names = new Set((environment?.agents ?? []).map((agent) => agent.name));
    if (metadata?.agent) names.add(metadata.agent);
    return [...names].sort();
  }, [environment, metadata]);

  const config = state?.capabilities?.config ?? [];
  const setConfig = async (option: ConfigOption, value: string | boolean) => {
    try {
      const next = await api.sessions.preparedConfig(draftId, option.id, value);
      setState(next);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Could not change configuration");
    }
  };

  const sendTurn = async (request: {
    text?: string;
    command?: string;
    args?: Record<string, string>;
  }) => {
    const body = request.command ? `/${request.command}${Object.values(request.args ?? {}).length ? ` ${Object.values(request.args ?? {}).join(" ")}` : ""}` : request.text ?? "";
    // A prompt command is a real first turn even when it has no free-form text.
    // Control commands never reach this callback: Composer resolves those against
    // the typed config surface without creating a turn.
    if (!body.trim() || sending || accepted) return;
    setSending(true);
    setError(null);
    try {
      const result = await api.sessions.preparedCommand(draftId, {
        text: body,
        attachments: attachments.map(({ name, type, data }) => ({ name, type, data })),
        idempotencyKey: crypto.randomUUID(),
      });
      promoted.current = true;
      setAccepted(result);
      // Promotion creates the first durable row.  Refresh the sidebar/workspace
      // projections before navigating so the new session is immediately discoverable.
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      if (result.session) router.replace(`/agents/${encodeURIComponent(result.session.traceAgentRef)}`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Could not start the turn");
      setSending(false);
    }
  };

  const send = () => sendTurn({ text });

  const switchAgent = async (agent: string) => {
    if (!metadata || agent === metadata.agent || switchingAgent) return;
    setSwitchingAgent(true);
    setError(null);
    try {
      // Create first so a transient spawn failure leaves the current draft usable.
      // Only after the replacement exists do we close this live link.
      const next = await api.sessions.create({
        environment: metadata.environment,
        agent,
        workdir: metadata.workdir,
        injectIdentity: metadata.injectIdentity,
      });
      try {
        await api.sessions.discardPrepared(draftId);
      } catch (discardError) {
        // Do not leave the replacement process running when the old draft could not
        // be closed.  The user is still on the original draft, so preserving it is
        // the useful failure mode; the replacement is otherwise unreachable except
        // through the server's idle reaper.
        await api.sessions.discardPrepared(next.draftId).catch(() => {});
        throw discardError;
      }
      router.replace(`/sessions/draft/${encodeURIComponent(next.draftId)}`);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Could not switch agent");
      setSwitchingAgent(false);
    }
  };

  const session = {
    id: draftId,
    agent: metadata?.agent ?? state?.agent ?? "",
    workdir: metadata?.workdir ?? "",
    flagsJson: "{}",
  };

  useEffect(() => {
    if (!accepted || accepted.session || ["unknown", "rejected", "cancelled", "conflicting"].includes(accepted.command.state)) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      void api.commands.get(accepted.command.acceptance.commandId, controller.signal).then((next) => {
        if (controller.signal.aborted) return;
        setAccepted(next);
        if (next.session) {
          void queryClient.invalidateQueries({ queryKey: ["sessions"] });
          void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
          router.replace(`/agents/${encodeURIComponent(next.session.traceAgentRef)}`);
        }
      }, (reason: unknown) => { if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason)); });
    }, 1000);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [accepted, queryClient, router]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="shrink-0 border-b border-line px-3 py-2 md:px-4">
        <div className="flex items-center gap-2">
          <h1 className="min-w-0 flex-1 truncate text-sm font-semibold">New session</h1>
          {metadata?.agent && agentOptions.length > 0 && (
            <label className="flex shrink-0 items-center gap-2 text-xs text-ink-muted">
              <span className="hidden sm:inline">Agent</span>
              <select
                value={metadata.agent}
                onChange={(event) => void switchAgent(event.target.value)}
                disabled={switchingAgent || Boolean(accepted)}
                className="max-w-32 rounded border border-line bg-canvas px-2 py-1 text-xs"
                aria-label="Agent"
              >
                {agentOptions.map((agent) => (
                  <option key={agent} value={agent}>{agent}</option>
                ))}
              </select>
            </label>
          )}
        </div>
        <p className="mt-0.5 truncate text-2xs text-ink-muted" title={metadata?.workdir}>
          Temporary until the first message · {metadata?.workdir ?? "Starting agent…"}
        </p>
      </header>

      <div data-scroller className="pane min-h-0 flex-1 px-3 py-3 md:px-4">
        <div className="mx-auto flex min-h-full max-w-3xl items-center justify-center text-center">
          <div className="max-w-md text-sm text-ink-muted">
            <p className="font-medium text-ink-soft">Ready when you are</p>
            <p className="mt-1">Choose a model or thinking level below, then send your first message to save this session.</p>
            {error && <p role="alert" className="mt-3 text-danger">{error}</p>}
          </div>
        </div>
      </div>

      {accepted && <p role="status" className="border-t border-line p-3 text-sm">{accepted.command.state === "unknown" ? "Delivery unconfirmed. Recover status before submitting again." : `Command ${accepted.command.state.replaceAll("_", " ")} · Waiting for the vendor identity`}<button className="ml-2 min-h-11 rounded px-3 underline focus-ring" onClick={() => { void api.commands.get(accepted.command.acceptance.commandId).then(setAccepted); }}>Recover status</button></p>}
      {state && (
        <Composer
          session={session}
          agents={environment?.agents ?? []}
          agentLocked={Boolean(accepted)}
          busyExecution={false}
          text={text}
          onTextChange={setText}
          attachments={attachments}
          onAttachmentsChange={setAttachments}
          onSend={() => void send()}
          onRunCommand={(name, rest) =>
            void sendTurn({
              command: name,
              args: rest ? { args: rest } : {},
            })
          }
          commands={state.capabilities?.commands ?? []}
          config={config}
          control={{ available: true }}
          sending={sending}
          sendError={error ? new Error(error) : null}
          onStop={() => {
            void api.sessions.discardPrepared(draftId).finally(() => router.push("/"));
          }}
          stopping={false}
          onSetConfig={(id, value) => {
            const option = config.find((candidate) => candidate.id === id);
            if (option) void setConfig(option, value);
          }}
          onAgentChange={(agent) => switchAgent(agent)}
          configError={error ? new Error(error) : null}
        />
      )}
    </div>
  );
}
