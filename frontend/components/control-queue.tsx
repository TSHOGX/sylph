"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type ControlRequest, type ControlState } from "@/lib/api";
import { IconButton } from "@/components/ui/button";
import { PencilIcon, StopIcon, SendIcon, XIcon } from "@/components/icons";
import { useT } from "@/lib/i18n";

const TERMINAL: ReadonlySet<ControlState> = new Set(["confirmed", "unconfirmed", "consumed", "rejected", "failed", "cancelled"]);

/** Shared, bounded view of the server's control ledger. */
export function ControlQueue({
  sessionId,
  canQueue,
  canSteer,
  turnId,
}: {
  sessionId: string;
  canQueue: boolean;
  canSteer: boolean;
  turnId?: string | null;
}) {
  const queryClient = useQueryClient();
  const t = useT();
  const query = useQuery({
    queryKey: ["session", sessionId, "controls"],
    queryFn: () => api.controls.list(sessionId),
    refetchInterval: 20_000,
  });
  const requests = query.data?.requests ?? [];
  const pending = requests.filter((request) => !TERMINAL.has(request.state)).sort((a, b) => a.serverSeq - b.serverSeq);
  const recent = requests.filter((request) => TERMINAL.has(request.state)).sort((a, b) => b.serverSeq - a.serverSeq).slice(0, 20);
  const [open, setOpen] = useState(true);
  const visible = [...pending, ...recent];
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ["session", sessionId, "controls"] });

  if (!query.isPending && visible.length === 0) return null;
  return (
    <section aria-label={t("control.queueTitle")} className="border-b border-line bg-sunken px-3 py-2">
      <div className="flex items-center justify-between gap-2">
        <button type="button" aria-expanded={open} onClick={() => setOpen((value) => !value)} className="tap-row rounded px-1 text-xs font-medium text-ink-soft focus-ring">
          {t("control.queueCount", { count: pending.length })}
        </button>
        {!canQueue && pending.length > 0 && <span className="text-3xs text-ink-muted">{t("control.queueUnavailable")}</span>}
      </div>
      {open && (
        <ol className="mt-1 max-h-48 space-y-1 overflow-y-auto" aria-live="polite">
          {visible.map((request) => (
            <ControlRow key={`${request.requestId}:${request.revision}`} request={request} canSteer={canSteer} canQueue={canQueue} turnId={turnId} onChanged={refresh} />
          ))}
        </ol>
      )}
    </section>
  );
}

function ControlRow({ request, canSteer, canQueue, turnId, onChanged }: {
  request: ControlRequest;
  canSteer: boolean;
  canQueue: boolean;
  turnId?: string | null;
  onChanged: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const t = useT();
  const [text, setText] = useState(request.text);
  const mutable = request.kind === "queue" && request.state === "queued";
  const edit = useMutation({
    mutationFn: () => api.controls.edit(request.sessionId, request.requestId, text, request.revision),
    onSuccess: () => { setEditing(false); onChanged(); },
  });
  const cancel = useMutation({
    mutationFn: () => api.controls.cancel(request.sessionId, request.requestId, request.revision),
    onSuccess: onChanged,
  });
  const steer = useMutation({
    mutationFn: () => api.controls.steer(request.sessionId, request.requestId, request.revision, turnId!, crypto.randomUUID()),
    onSuccess: onChanged,
  });
  return (
    <li className="flex min-w-0 items-start gap-2 rounded border border-line bg-canvas px-2 py-1.5 text-xs">
      <span className="min-w-0 flex-1">
        <span className="mr-1 text-3xs text-ink-muted">{request.originSubject}</span>
        {editing ? (
          <input value={text} onChange={(event) => setText(event.target.value)} className="w-full rounded border border-line bg-canvas px-1 py-0.5" aria-label="Edit queued text" />
        ) : <span className="whitespace-pre-wrap break-words">{request.text}</span>}
        <span className="ml-2 text-3xs text-ink-muted">{request.state}</span>
        {(request.kind === "native_steer" || request.kind === "native_queue") && Boolean(request.vendorReceipt) && <span className="ml-2 text-3xs text-ink-muted">{t("control.nativeReceipt")}</span>}
        {request.errorMessage && <span role="alert" className="ml-2 text-3xs text-danger">{request.errorMessage}</span>}
      </span>
      {mutable && editing && <IconButton label={t("control.save")} onClick={() => edit.mutate()}><SendIcon size={13} /></IconButton>}
      {mutable && !editing && <IconButton label={t("control.edit")} onClick={() => setEditing(true)}><PencilIcon size={13} /></IconButton>}
      {mutable && <IconButton label={t("action.cancel")} onClick={() => cancel.mutate()}><XIcon size={13} /></IconButton>}
      {mutable && canSteer && turnId && <IconButton label={t("control.steerNow")} onClick={() => steer.mutate()}><StopIcon size={13} /></IconButton>}
      {!canQueue && request.state === "rejected" && <span className="text-3xs text-danger">{t("control.queueUnsupportedShort")}</span>}
    </li>
  );
}
