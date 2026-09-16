"use client";
import { useT } from "@/lib/i18n";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ApiError, api } from "@/lib/api";
import {
  attachmentsFor,
  draftFor,
  saveDraft,
  saveDraftAttachments,
} from "@/lib/trace-cache";
import { commandControl } from "@/lib/trace-command";
import { traceAPI } from "@/lib/trace-api";
import { useTraceResource } from "@/lib/use-trace-resource";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import {
  agentName,
  type CommandReceipt,
  type CommandRequest,
  type ControlAction,
  type ReaderBootstrap,
} from "@/lib/trace-types";
import type { StagedAttachment } from "./attachment-picker";
import { Composer } from "./composer";
import { ApprovalDialog } from "./approval-dialog";

/** Operational controls are refreshed separately from the immutable reading snapshot. */
export function TraceComposer({
  bootstrap,
  restoreSelection = null,
  onSelectionRestored,
  onFind,
}: {
  bootstrap: ReaderBootstrap;
  restoreSelection?: { start: number; end: number } | null;
  onSelectionRestored?: () => void;
  onFind?: () => void;
}) {
  const t = useT();
  const agent = bootstrap.header.agent.address;
  const root = useRef<HTMLDivElement>(null);
  const [text, setText] = useState(() => draftFor(agent));
  const [attachments, setAttachments] = useState<StagedAttachment[]>(() =>
    attachmentsFor(agent),
  );
  const [refresh, setRefresh] = useState(0);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [receipts, setReceipts] = useState<CommandReceipt[]>([]);
  const [lastCommand, setLastCommand] = useState<{
    id: string;
    action: ControlAction;
  } | null>(null);
  const [requestId, setRequestId] = useState<string | null>(null);
  const ui = useWorkspaceUI();
  const operational = useTraceResource(
    `controls:${agent}`,
    async (signal) => {
      const control = await traceAPI.control(agent, signal);
      const id =
        control.kind === "available" && control.target.kind === "session"
          ? control.target.sessionId
          : null;
      const [link, requests] = id
        ? await Promise.all([
            api.sessions.capabilities(id, signal),
            api.sessions.requests(id, signal),
          ])
        : [null, []];
      const queue = await traceAPI.commands(agent, signal);
      return { control, link, requests, commands: queue.commands };
    },
    refresh + bootstrap.view.viewRevision,
    false,
  );
  useEffect(() => {
    const timer = setInterval(() => setRefresh((value) => value + 1), 5000);
    return () => clearInterval(timer);
  }, []);
  const control = operational.data?.control;
  const available = control?.kind === "available";
  const actions = available ? control.actions : [];
  const input = (value: string) => {
    setText(value);
    saveDraft(agent, value);
  };
  const attachmentInput = (value: StagedAttachment[]) => {
    setAttachments(value);
    saveDraftAttachments(agent, value);
  };
  const submit = async (
    action: ControlAction,
    submitted: string | null = null,
    extra: Partial<CommandRequest> = {},
  ) => {
    if (sending) return;
    setSending(true);
    setError(null);
    const capturedAttachments = attachments;
    let dispatchAttempted = false;
    try {
      // A fresh route and lease are the authorization source for every command. The
      // inspector never supplies a target, and an embedded route keeps its selector.
      const fresh = commandControl(
        agent,
        action,
        control,
        await traceAPI.control(agent),
      );
      dispatchAttempted = true;
      const receipt = await traceAPI.command({
        target: fresh.target,
        action,
        lease: fresh.lease,
        affectedExecution: fresh.affectedExecution,
        idempotencyKey: crypto.randomUUID(),
        text: submitted,
        attachments:
          action === "send"
            ? capturedAttachments.map(({ name, type, data }) => ({
                name,
                type,
                data,
              }))
            : [],
        config: null,
        answer: null,
        ...extra,
      });
      setLastCommand({ id: receipt.commandId, action });
      setReceipts((previous) =>
        [
          ...previous.filter((item) => item.commandId !== receipt.commandId),
          receipt,
        ].slice(-32),
      );
      if (
        !["rejected", "failed", "unconfirmed"].includes(receipt.state) &&
        submitted !== null
      ) {
        setText((current) => {
          if (current !== submitted) return current;
          saveDraft(agent, "");
          return "";
        });
        if (action === "send")
          setAttachments((current) => {
            const next = current.filter(
              (item) =>
                !capturedAttachments.some((sent) => sent.id === item.id),
            );
            saveDraftAttachments(agent, next);
            return next;
          });
      }
      if (receipt.error) setError(receipt.error);
      setRefresh((value) => value + 1);
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(
        dispatchAttempted &&
          (!(reason instanceof ApiError) || reason.status >= 500)
          ? `${message} Delivery may be unconfirmed. Recover command status before submitting again.`
          : message,
      );
    } finally {
      setSending(false);
    }
  };
  useEffect(() =>
    ui.registerSessionActions((action) => {
      if (action === "findInSession") {
        onFind?.();
        return true;
      }
      if (action !== "stopTurn" || !actions.includes("stop")) return false;
      void submit("stop");
      return true;
    }),
  );
  const byCommand = new Map(
    (operational.data?.commands ?? []).map((receipt) => [
      receipt.commandId,
      receipt,
    ]),
  );
  for (const receipt of receipts) {
    const current = byCommand.get(receipt.commandId);
    if (!current || receipt.revision > current.revision)
      byCommand.set(receipt.commandId, receipt);
  }
  const queue = [...byCommand.values()].slice(-32);
  const latestError = lastCommand
    ? queue.find((receipt) => receipt.commandId === lastCommand.id)?.error
    : null;
  const request = operational.data?.requests.find(
    (item) => item.id === requestId,
  );
  const busyExecution = !actions.includes("send");
  useLayoutEffect(() => {
    if (!available || !restoreSelection) return;
    const editor = root.current?.querySelector<HTMLTextAreaElement>(
      "[data-composer-editor]",
    );
    if (editor) {
      editor.focus();
      editor.setSelectionRange(restoreSelection.start, restoreSelection.end);
      onSelectionRestored?.();
    }
  }, [available, restoreSelection, onSelectionRestored]);
  return (
    <div ref={root} className="shrink-0 border-t border-line">
      <div className="flex flex-wrap items-center justify-between gap-2 px-3 py-1 text-xs">
        <span>
          {available
            ? t("trace.sendTo", { agent: agentName(bootstrap.header.agent, t) })
            : control?.kind === "unavailable"
              ? control.reason
              : t("trace.checkingControls")}
        </span>
        <button
          className="min-h-11 rounded px-2 hover:bg-hover focus-ring"
          onClick={() => setRefresh((value) => value + 1)}
        >
          {t("trace.refreshControls")}
        </button>
      </div>
      {operational.error && (
        <p role="alert" className="px-3 text-sm">
          {operational.error.message}
        </p>
      )}
      {queue.length > 0 && (
        <details className="px-3 pb-2 text-xs">
          <summary className="min-h-8 cursor-pointer">
            {t("trace.commandStatus", { count: queue.length })}
          </summary>
          <ul className="max-h-60 overflow-auto">
            {queue.map((receipt) => (
              <CommandRow
                key={`${receipt.commandId}:${receipt.revision}`}
                receipt={receipt}
                changed={(next) => {
                  setReceipts((previous) =>
                    [
                      ...previous.filter(
                        (item) => item.commandId !== next.commandId,
                      ),
                      next,
                    ].slice(-32),
                  );
                  setRefresh((value) => value + 1);
                }}
              />
            ))}
          </ul>
          <button
            className="min-h-11 rounded px-2 focus-ring"
            onClick={() => setRefresh((value) => value + 1)}
          >
            {t("trace.recoverCommand")}
          </button>
        </details>
      )}

      {available ? (
        <Composer
          session={{
            id: bootstrap.header.fileContext?.sessionId ?? null,
            agent: bootstrap.header.agent.vendor,
            workdir: bootstrap.header.fileContext?.workdir ?? "",
            flagsJson: "{}",
          }}
          agents={[]}
          agentLocked
          busyExecution={busyExecution}
          text={text}
          onTextChange={input}
          attachments={attachments}
          onAttachmentsChange={attachmentInput}
          onSend={() => void submit("send", text)}
          onRunCommand={(name, rest) =>
            void submit("send", `/${name}${rest ? ` ${rest}` : ""}`)
          }
          commands={operational.data?.link?.capabilities?.commands ?? []}
          config={operational.data?.link?.capabilities?.config ?? []}
          control={{ available: true }}
          sending={sending}
          sendError={
            error
              ? new Error(error)
              : latestError && lastCommand?.action !== "configure"
                ? new Error(latestError)
                : null
          }
          configError={
            latestError && lastCommand?.action === "configure"
              ? new Error(latestError)
              : null
          }
          onStop={() => void submit("stop")}
          stopping={sending}
          canInterrupt={actions.includes("interrupt")}
          onInterrupt={() => void submit("interrupt")}
          canSteer={actions.includes("steer")}
          onSteer={(value) => void submit("steer", value)}
          canQueue={actions.includes("queue")}
          canNativeQueue={actions.includes("native_queue")}
          canStop={actions.includes("stop")}
          canQueueSteering={actions.includes("native_steer")}
          onNativeQueue={(value, priority) =>
            void submit(priority ? "native_steer" : "native_queue", value)
          }
          onQueue={(value) => void submit("queue", value)}
          onSetConfig={(id, value) =>
            void submit("configure", null, { config: { id, value } })
          }
          blockedCount={operational.data?.requests.length ?? 0}
          onShowBlocked={() =>
            setRequestId(operational.data?.requests[0]?.id ?? null)
          }
        />
      ) : (
        <div className="p-3">
          <textarea
            aria-label={t("trace.unsentDraft")}
            value={text}
            onChange={(event) => input(event.target.value)}
            placeholder={t("trace.keepDraft")}
            className="min-h-20 w-full resize-y rounded border border-line bg-canvas p-3 text-base"
          />
          {error && <p role="alert">{error}</p>}
        </div>
      )}
      {request && (
        <ApprovalDialog
          request={request}
          busy={sending}
          onDismiss={() => setRequestId(null)}
          onResolve={(optionId, answer) => {
            void submit("approval", null, {
              answer: {
                requestId: request.id,
                optionId,
                input: answer ?? null,
              },
            });
          }}
        />
      )}
    </div>
  );
}

function CommandRow({
  receipt,
  changed,
}: {
  receipt: CommandReceipt;
  changed: (receipt: CommandReceipt) => void;
}) {
  const t = useT();
  const [editing, setEditing] = useState(false);
  const [text, setText] = useState(receipt.text ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const act = async (kind: "edit" | "cancel") => {
    setBusy(true);
    setError(null);
    try {
      changed(
        await (kind === "edit"
          ? traceAPI.editCommand(receipt.commandId, receipt.revision, text)
          : traceAPI.cancelCommand(receipt.commandId, receipt.revision)),
      );
      setEditing(false);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };
  return (
    <li className="border-b border-line py-2">
      <p>
        {receipt.state === "accepted"
          ? t("trace.acceptedUpdating")
          : receipt.state === "unconfirmed"
            ? t("trace.unconfirmed")
            : receipt.state.replaceAll("_", " ")}
        {receipt.error ? ` · ${receipt.error}` : ""}
      </p>
      {editing ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void act("edit");
          }}
        >
          <textarea
            aria-label={t("trace.reviseInput")}
            className="my-2 min-h-20 w-full rounded border border-line bg-canvas p-2 text-base"
            value={text}
            onChange={(event) => setText(event.target.value)}
          />
          <button
            className="min-h-11 rounded px-3 focus-ring"
            disabled={busy || !receipt.editable}
          >
            {t("trace.saveRevision")}
          </button>
          <button
            type="button"
            className="min-h-11 rounded px-3 focus-ring"
            onClick={() => setEditing(false)}
          >
            {t("trace.discardEdit")}
          </button>
        </form>
      ) : (
        receipt.text && (
          <p className="mt-1 whitespace-pre-wrap">{receipt.text}</p>
        )
      )}
      {receipt.editable && !editing && (
        <button
          className="min-h-11 rounded px-2 focus-ring"
          disabled={busy}
          onClick={() => setEditing(true)}
        >
          {t("trace.editQueued")}
        </button>
      )}
      {receipt.cancellable && (
        <button
          className="min-h-11 rounded px-2 focus-ring"
          disabled={busy}
          onClick={() => void act("cancel")}
        >
          {t("trace.cancelQueued")}
        </button>
      )}
      {error && <p role="alert">{error}</p>}
    </li>
  );
}
