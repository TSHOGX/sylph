"use client";

/**
 * The message composer.
 *
 * One bordered surface holding everything a turn needs: attachments, the message,
 * and the runner configuration that turn will use. It replaces a layout where the
 * textarea, the attachment dropzone, and the buttons were three stacked blocks —
 * which read as three unrelated form fields rather than one input, and left the
 * agent and model invisible even though both decide what the message will do.
 *
 * The controls are generated from the runner's `--describe` output, never
 * hardcoded. This is not defensiveness: the four installed runners genuinely
 * disagree. `codex` declares no `model` at all, and every runner declares a
 * different `effort` enumeration — `claude` has `xhigh`/`max`, `codex` has
 * `minimal`, `pi` has `off`. A fixed pair of dropdowns would be wrong for `codex`
 * on the first render.
 */

import dynamic from "next/dynamic";
import { useRouter } from "next/navigation";
import { useEffect, useId, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  api,
  ApiError,
  sessionFlags,
  type Agent,
  type Session,
} from "@/lib/api";
import { formatBytes } from "@/lib/format";
import {
  MAX_ATTACHMENTS,
  MAX_TOTAL_BYTES,
  readAttachments,
  type StagedAttachment,
} from "@/components/attachment-picker";

const DirectoryPicker = dynamic(
  () => import("@/components/directory-picker").then((m) => m.DirectoryPicker),
  { ssr: false },
);

/** Order flags are shown in, left to right. Anything else follows, sorted. */
const FLAG_ORDER = ["model", "effort"];

/** Human label for a flag key. `effort` is the reasoning budget. */
function flagLabel(key: string): string {
  if (key === "effort") return "Thinking";
  if (key === "model") return "Model";
  return key;
}

export function Composer({
  session,
  agents,
  /** Turns already in this session. Zero means the agent is still changeable. */
  turnCount,
  isLive,
  text,
  onTextChange,
  attachments,
  onAttachmentsChange,
  onSend,
  sending,
  sendError,
  onStop,
  stopping,
  hasCommands,
  onOpenCommands,
}: {
  session: Session;
  agents: Agent[];
  turnCount: number;
  isLive: boolean;
  text: string;
  onTextChange: (value: string) => void;
  attachments: StagedAttachment[];
  onAttachmentsChange: (next: StagedAttachment[]) => void;
  onSend: () => void;
  sending: boolean;
  sendError: unknown;
  onStop: () => void;
  stopping: boolean;
  hasCommands: boolean;
  onOpenCommands: () => void;
}) {
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const messageId = useId();
  const problemId = useId();

  const [dragging, setDragging] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [encoding, setEncoding] = useState(false);
  const [uploadTarget, setUploadTarget] = useState<StagedAttachment[] | null>(null);

  const descriptor = agents.find((agent) => agent.name === session.agent);
  const flags = sessionFlags(session);
  const disabled = isLive || sending;
  const canSend = text.trim().length > 0 && !disabled;

  // The agent is fixed once the conversation has content. Switching it would
  // hand an existing transcript to a different runner — and with it a different
  // identity and lifecycle policy — so it is offered only on an empty session.
  const agentLocked = turnCount > 0;

  // Auto-grow to the content, bounded. A fixed two-row box hides the start of a
  // long message while it is being written, and a freely growing one eats the
  // transcript.
  useEffect(() => {
    const node = textareaRef.current;
    if (!node) return;
    node.style.height = "auto";
    node.style.height = `${Math.min(node.scrollHeight, 240)}px`;
  }, [text]);

  const totalBytes = attachments.reduce((sum, item) => sum + item.size, 0);

  async function accept(files: FileList | null) {
    if (!files || files.length === 0) return;
    setProblem(null);
    setEncoding(true);
    const { accepted, rejected } = await readAttachments(files, attachments);
    setEncoding(false);
    if (accepted.length > 0) onAttachmentsChange(accepted);
    setProblem(rejected.length > 0 ? rejected.join("; ") : null);
  }

  const uploadToWorkspace = useMutation({
    mutationFn: async ({ path, items }: { path: string; items: StagedAttachment[] }) => {
      // Sequential rather than concurrent: these are writes into the user's
      // project, and a partial failure is far easier to describe when the files
      // land in a known order.
      for (const item of items) {
        await api.files.upload(session.id, {
          path: relativeTo(path, session.workdir),
          name: item.name,
          data: item.data,
        });
      }
      return items.length;
    },
    onSuccess: () => {
      // The Artifacts pane lists this directory; without this the uploaded file
      // is missing until something else refetches.
      void queryClient.invalidateQueries({ queryKey: ["files", session.id] });
      setUploadTarget(null);
    },
  });

  return (
    <div className="shrink-0 border-t border-gray-200 px-4 py-2">
      <div className="mx-auto max-w-3xl">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (canSend) onSend();
          }}
          // Drop anywhere on the composer, not only on a dedicated strip: the
          // whole surface is the input, so the whole surface accepts files.
          onDragOver={(event) => {
            if (disabled) return;
            event.preventDefault();
            setDragging(true);
          }}
          onDragLeave={() => setDragging(false)}
          onDrop={(event) => {
            if (disabled) return;
            event.preventDefault();
            setDragging(false);
            void accept(event.dataTransfer.files);
          }}
          // Focus-within on the container, so the whole composer reads as one
          // control rather than the textarea lighting up inside a static box.
          //
          // No `opacity` while disabled. It used to carry `opacity-60`, which is a
          // blunt instrument: opacity composites the whole subtree against the
          // page, so every descendant's contrast drops at once. Measured by axe
          // during a live turn, `text-gray-600` fell to 2.87:1 and the Stop
          // button's `text-red-800` to 3.7:1 — both below the 4.5:1 that WCAG
          // 1.4.3 requires, and Stop is the one control a user needs *while* the
          // turn runs. The muted background says "not accepting input" without
          // touching foreground contrast, and the textarea dims its own text.
          className={`rounded-xl border transition-colors focus-within:border-gray-400 focus-within:ring-2 focus-within:ring-gray-900/10 ${
            dragging
              ? "border-gray-900 bg-gray-50"
              : disabled
                ? "border-gray-200 bg-gray-50"
                : "border-gray-200 bg-white"
          }`}
        >
          {attachments.length > 0 && (
            <ul
              aria-label="Staged attachments"
              className="flex flex-wrap gap-1.5 border-b border-gray-100 px-2.5 pb-2 pt-2"
            >
              {attachments.map((item) => (
                <li
                  key={item.id}
                  className="flex items-center gap-1.5 rounded-md border border-gray-200 bg-gray-50 py-1 pl-1 pr-1.5 text-xs"
                >
                  {item.previewUrl ? (
                    // eslint-disable-next-line @next/next/no-img-element
                    <img src={item.previewUrl} alt="" className="h-6 w-6 rounded object-cover" />
                  ) : (
                    <span
                      aria-hidden="true"
                      className="flex h-6 w-6 items-center justify-center rounded bg-gray-200 text-gray-600"
                    >
                      ⎘
                    </span>
                  )}
                  <span className="max-w-[10rem] truncate font-medium" title={item.name}>
                    {item.name}
                  </span>
                  <span className="text-gray-500">{formatBytes(item.size)}</span>
                  <button
                    type="button"
                    onClick={() =>
                      onAttachmentsChange(attachments.filter((a) => a.id !== item.id))
                    }
                    disabled={disabled}
                    aria-label={`Remove ${item.name}`}
                    className="rounded px-1 text-gray-500 hover:bg-gray-200 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}

          <label htmlFor={messageId} className="sr-only">
            Message
          </label>
          <textarea
            id={messageId}
            ref={textareaRef}
            value={text}
            rows={1}
            onChange={(event) => onTextChange(event.target.value)}
            onKeyDown={(event) => {
              // Enter sends; Shift+Enter and Cmd/Ctrl+Enter insert a newline.
              if (
                event.key === "Enter" &&
                !event.shiftKey &&
                !event.metaKey &&
                !event.ctrlKey
              ) {
                event.preventDefault();
                if (canSend) onSend();
              }
            }}
            onPaste={(event) => {
              // Pasted images are attachments. Without this a screenshot in the
              // clipboard pastes as nothing at all.
              const files = event.clipboardData.files;
              if (files.length > 0) {
                event.preventDefault();
                void accept(files);
              }
            }}
            placeholder={
              isLive ? "Waiting for the current turn to finish..." : "Send a message..."
            }
            disabled={disabled}
            aria-describedby={problem ? problemId : undefined}
            // `disabled:text-gray-500` rather than inheriting a container opacity:
            // it dims the one thing that should read as inactive, and 500 on the
            // disabled background still clears 4.5:1.
            className="block max-h-60 w-full resize-none border-0 bg-transparent px-3 py-2.5 text-sm placeholder:text-gray-400 focus:outline-none disabled:cursor-not-allowed disabled:text-gray-500"
          />

          <div className="flex items-center gap-1.5 px-2 pb-2">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              disabled={disabled || encoding}
              onChange={(event) => {
                void accept(event.target.files);
                // Reset so selecting the same file twice in a row still fires.
                event.target.value = "";
              }}
              className="sr-only"
              aria-hidden="true"
              tabIndex={-1}
            />
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              disabled={disabled || encoding}
              title={`Attach files — up to ${MAX_ATTACHMENTS}, ${formatBytes(MAX_TOTAL_BYTES)} total`}
              aria-label="Attach files"
              className="rounded-md p-1.5 text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
            >
              <span aria-hidden="true">📎</span>
            </button>

            {attachments.length > 0 && (
              <button
                type="button"
                onClick={() => setUploadTarget(attachments)}
                disabled={disabled || uploadToWorkspace.isPending}
                title="Copy the staged files into a folder in this workspace"
                className="rounded-md px-1.5 py-1 text-[11px] font-medium text-gray-600 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
              >
                {uploadToWorkspace.isPending ? "Saving..." : "Save to workspace..."}
              </button>
            )}

            <AgentControl
              session={session}
              agents={agents}
              locked={agentLocked}
              disabled={disabled}
            />

            <div className="ml-auto flex items-center gap-1.5">
              {descriptor?.flags &&
                Object.entries(descriptor.flags)
                  .sort(([a], [b]) => {
                    const ai = FLAG_ORDER.indexOf(a);
                    const bi = FLAG_ORDER.indexOf(b);
                    if (ai !== -1 || bi !== -1) {
                      return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
                    }
                    return a.localeCompare(b);
                  })
                  .map(([key, spec]) => (
                    <FlagControl
                      key={key}
                      sessionId={session.id}
                      flagKey={key}
                      label={flagLabel(key)}
                      choices={spec.choices ?? []}
                      value={flags[key] ?? ""}
                      allFlags={flags}
                      // Mid-turn the runner is already running with the old value,
                      // so a change now would silently apply to the *next* turn
                      // while appearing to affect this one.
                      disabled={isLive}
                    />
                  ))}

              {hasCommands && (
                <button
                  type="button"
                  onClick={onOpenCommands}
                  disabled={disabled}
                  className="rounded-md border border-gray-200 px-2 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
                >
                  Command
                </button>
              )}

              {isLive ? (
                <button
                  type="button"
                  onClick={onStop}
                  disabled={stopping}
                  className="rounded-md border border-red-300 px-2.5 py-1 text-xs font-medium text-red-800 hover:bg-red-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-red-600 disabled:opacity-50"
                >
                  {stopping ? "Stopping..." : "Stop"}
                </button>
              ) : (
                <button
                  type="submit"
                  disabled={!canSend}
                  className="rounded-md bg-gray-900 px-3 py-1 text-xs font-medium text-white hover:bg-gray-700 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
                >
                  {sending ? "Sending..." : "Send"}
                </button>
              )}
            </div>
          </div>
        </form>

        {encoding && (
          <output aria-live="polite" className="mt-1 block text-xs text-gray-500">
            Encoding attachments...
          </output>
        )}

        {problem && (
          <p id={problemId} role="alert" className="mt-1 text-xs text-red-700">
            {problem}
          </p>
        )}

        {uploadToWorkspace.error != null && (
          <p role="alert" className="mt-1 text-xs text-red-700">
            {uploadToWorkspace.error instanceof Error
              ? uploadToWorkspace.error.message
              : "Could not save those files into the workspace."}
          </p>
        )}

        {sendError != null && (
          <p role="alert" className="mt-1 text-xs text-red-700">
            {describeSendError(sendError)}
          </p>
        )}

        <div className="mt-1 flex items-center justify-between gap-3 px-1">
          <p className="text-[10px] text-gray-500">
            Enter to send · Shift+Enter for a new line
          </p>
          {attachments.length > 0 && (
            <p className="text-[10px] text-gray-500">
              {attachments.length} of {MAX_ATTACHMENTS} files · {formatBytes(totalBytes)} of{" "}
              {formatBytes(MAX_TOTAL_BYTES)}
            </p>
          )}
        </div>
      </div>

      {uploadTarget && (
        <DirectoryPicker
          title="Save attachments into the workspace"
          confirmLabel="Save here"
          initialPath={session.workdir}
          onClose={() => setUploadTarget(null)}
          onChoose={(path) =>
            uploadToWorkspace.mutate({ path, items: uploadTarget })
          }
        />
      )}
    </div>
  );
}

/**
 * The agent this session runs on.
 *
 * Interactive only before the first turn. After that it renders as a plain
 * label — not a disabled control — because a disabled `<select>` invites the user
 * to keep trying. The tooltip says why.
 */
function AgentControl({
  session,
  agents,
  locked,
  disabled,
}: {
  session: Session;
  agents: Agent[];
  locked: boolean;
  disabled: boolean;
}) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const labelId = useId();

  // Changing the agent replaces the session: `agent` is fixed at creation
  // server-side, and identity policy hangs off it. On an empty session there is
  // nothing to preserve, so this creates a fresh one in the same directory and
  // navigates to it — which is why it is only offered at turn zero.
  const recreate = useMutation({
    mutationFn: (agent: string) =>
      api.sessions.create({ agent, workdir: session.workdir }),
    onSuccess: ({ session: created }) => {
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      router.push(`/sessions/${created.id}`);
    },
  });

  if (locked) {
    return (
      <span
        title="The agent is fixed once a session has history: its transcript and identity policy belong to this runner."
        className="flex items-center gap-1 rounded-md px-1.5 py-1 text-xs font-medium text-gray-600"
      >
        <span aria-hidden="true" className="text-emerald-600">
          ●
        </span>
        {session.agent}
      </span>
    );
  }

  return (
    <>
      <label id={labelId} className="sr-only">
        Agent
      </label>
      <select
        aria-labelledby={labelId}
        value={session.agent}
        disabled={disabled || recreate.isPending}
        onChange={(event) => recreate.mutate(event.target.value)}
        title="Switch agent. Available until this session has its first turn."
        className="cursor-pointer rounded-md px-1.5 py-1 text-xs font-medium text-gray-700 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
      >
        {agents.map((agent) => (
          <option key={agent.name} value={agent.name}>
            {agent.name}
          </option>
        ))}
      </select>
    </>
  );
}

/**
 * One runner flag, rendered from its declared choices.
 *
 * Sends the whole flag set on change, because `PATCH` replaces rather than
 * merges — a partial send would clear the flag the user is not touching.
 */
function FlagControl({
  sessionId,
  flagKey,
  label,
  choices,
  value,
  allFlags,
  disabled,
}: {
  sessionId: string;
  flagKey: string;
  label: string;
  choices: string[];
  value: string;
  allFlags: Record<string, string>;
  disabled: boolean;
}) {
  const queryClient = useQueryClient();
  const labelId = useId();

  const patch = useMutation({
    mutationFn: (next: string) => {
      const flags = { ...allFlags };
      // Empty means "let the runner decide", which is expressed by omitting the
      // flag rather than sending a blank value the runner would pass through.
      if (next) flags[flagKey] = next;
      else delete flags[flagKey];
      return api.sessions.update(sessionId, { flags });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["session", sessionId] });
    },
  });

  // A free-form flag has nothing to enumerate, so there is no sensible control to
  // draw for it here. Omitted rather than shown as an empty dropdown.
  if (choices.length === 0) return null;

  return (
    <>
      <label id={labelId} className="sr-only">
        {label}
      </label>
      <select
        aria-labelledby={labelId}
        value={value}
        disabled={disabled || patch.isPending}
        onChange={(event) => patch.mutate(event.target.value)}
        title={
          patch.error instanceof Error
            ? patch.error.message
            : `${label} for the next turn`
        }
        className={`cursor-pointer rounded-md border px-1.5 py-1 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50 ${
          patch.error != null
            ? "border-red-300 text-red-800"
            : "border-gray-200 text-gray-700 hover:bg-gray-50"
        }`}
      >
        <option value="">{label}: default</option>
        {choices.map((choice) => (
          <option key={choice} value={choice}>
            {label}: {choice}
          </option>
        ))}
      </select>
    </>
  );
}

/**
 * Express an absolute picked directory relative to the session's workdir.
 *
 * The upload endpoint takes a workdir-relative path, while the picker deals in
 * absolute ones. A directory outside the workdir yields `""` — the workdir root —
 * rather than an escaping path, because the backend would refuse it anyway and
 * the root is the harmless interpretation.
 */
export function relativeTo(absolute: string, workdir: string): string {
  const root = workdir.replace(/\/+$/, "");
  if (absolute === root) return "";
  if (absolute.startsWith(`${root}/`)) return absolute.slice(root.length + 1);
  return "";
}

/**
 * Turn an API failure into text that names the actual cause.
 *
 * The backend's stable error codes carry more than their prose does: a 409 on a
 * busy session and a rejected attachment need different next actions from the
 * user, and "Request failed" tells them neither.
 */
function describeSendError(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "SESSION_ALREADY_RUNNING":
        return "This session already has a turn in flight. Wait for it to finish, or stop it first.";
      case "ATTACHMENT_INVALID":
        return `Attachment rejected: ${error.message}`;
      case "PAYLOAD_TOO_LARGE":
        return "The message plus attachments exceed the 10 MB per-turn limit. Remove a file and try again.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : "Could not send the message.";
}
