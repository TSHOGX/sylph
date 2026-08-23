"use client";

/**
 * The mutating half of the Artifacts pane: new folder, upload, delete.
 *
 * # Why one hook rather than per-row state
 *
 * A tree can hold 2000 rows in one directory. Giving each row its own dialog
 * state, its own mutation, and its own hidden `<input type="file">` would put
 * three pieces of machinery on every row to serve the one row a user acts on —
 * and would mount 2000 file inputs. So the pane owns exactly one dialog and one
 * input, and a row only says *what* it wants done. That also makes the dialog
 * unambiguous: there is one modal question on screen at a time because there is
 * one place able to ask it.
 *
 * # Confirmation is here, not at the row
 *
 * Delete is the only irreversible operation in the app — the backend has no trash
 * to restore from, deliberately (see `api/files.rs`). Routing every delete through
 * this hook means no caller can skip the confirm by accident: the row cannot
 * *call* delete, it can only *request* it.
 *
 * # Folders delete recursively in one question
 *
 * The backend refuses a non-empty directory unless asked, which is the right
 * default for an API. Surfacing that as two dialogs — confirm, fail, confirm
 * again — would be the wrong UI for it: Finder does not ask twice either. So the
 * dialog states plainly that the contents go with it and sends `recursive`, and
 * the backend guard stays as the protection against a client bug rather than as a
 * step in the flow.
 */

import { useCallback, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ApiError, api } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { MAX_ATTACHMENT_BYTES } from "@/components/attachment-picker";
import { AlertDialog, PromptDialog } from "@/components/ui/dialog";

/** What the pane is currently asking the user about. */
type Pending =
  | { kind: "delete"; path: string; name: string; isDir: boolean }
  | { kind: "mkdir"; parent: string }
  /**
   * An upload stopped by an existing file. Held rather than auto-resolved: the
   * backend refuses to overwrite, and silently retrying with `overwrite` would
   * destroy a file the user never chose to replace.
   */
  | { kind: "overwrite"; parent: string; files: File[]; name: string };

export interface FileOperations {
  /** Ask to create a folder inside `parent` (relative path; empty is the root). */
  requestMkdir: (parent: string) => void;
  /** Open the file picker, uploading into `parent`. */
  requestUpload: (parent: string) => void;
  /** Upload files the user already produced — a drop, or a paste. */
  uploadInto: (parent: string, files: File[]) => void;
  /** Ask to delete one entry. Never deletes without a confirmation. */
  requestDelete: (entry: { path: string; name: string; isDir: boolean }) => void;
  /** True while a mutation is in flight, for disabling the affordances. */
  busy: boolean;
  /** The directory currently receiving an upload, for a progress hint. */
  uploadingInto: string | null;
  /**
   * The dialog and the file input. Rendered once by the pane; both are absent
   * from the DOM until something is pending, except the input, which must exist
   * to be clicked.
   */
  surface: React.ReactNode;
}

export interface FileOperationHooks {
  /** A path stopped existing, so the viewer can stop showing it. */
  onDeleted?: (path: string) => void;
  /**
   * Something was written into this directory.
   *
   * The pane uses it to expand that directory, because a mutation whose result is
   * inside a collapsed folder is indistinguishable from a mutation that did
   * nothing — an upload into a closed folder reported success and changed nothing
   * on screen, which reads as a broken feature rather than a hidden one.
   */
  onWrote?: (parent: string) => void;
}

export function useFileOperations(
  sessionId: string,
  { onDeleted, onWrote }: FileOperationHooks = {},
): FileOperations {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState<Pending | null>(null);
  const [error, setError] = useState<string | undefined>();
  const inputRef = useRef<HTMLInputElement>(null);
  /**
   * Where a click on the hidden input will deliver its files.
   *
   * A ref, not state: `input.click()` is fired synchronously from the same
   * handler that sets the destination, and the `change` event arrives long after
   * — a state update would not have committed in time to be read, and re-rendering
   * the input mid-gesture can cancel the picker on some browsers.
   */
  const uploadTarget = useRef<string>("");
  const [uploadingInto, setUploadingInto] = useState<string | null>(null);

  /**
   * Refresh the tree after a mutation.
   *
   * Invalidates every directory of this session rather than computing the parent.
   * Only *mounted* directories refetch, so the cost is the expanded subtree the
   * user is looking at — and it is correct in the cases a targeted invalidation
   * gets wrong: an upload that replaced a file changes its size, and a recursive
   * delete removes descendants whose own queries are still cached.
   */
  const refresh = useCallback(
    () => queryClient.invalidateQueries({ queryKey: ["files", sessionId] }),
    [queryClient, sessionId],
  );

  const close = useCallback(() => {
    setPending(null);
    setError(undefined);
  }, []);

  const mkdir = useMutation({
    mutationFn: (variables: { parent: string; name: string }) =>
      api.files.mkdir(sessionId, { path: variables.parent, name: variables.name }),
    onSuccess: async (_result, variables) => {
      onWrote?.(variables.parent);
      await refresh();
      close();
    },
    onError: (cause) => setError(describe(cause)),
  });

  const remove = useMutation({
    mutationFn: (variables: { path: string; isDir: boolean }) =>
      api.files.remove(sessionId, variables.path, { recursive: variables.isDir }),
    onSuccess: async (_result, variables) => {
      onDeleted?.(variables.path);
      await refresh();
      close();
    },
    onError: (cause) => setError(describe(cause)),
  });

  /**
   * Upload sequentially, not in parallel.
   *
   * Each file is base64 in a JSON body, so five 8 MB files in flight at once is
   * ~53 MB of encoded payload held in memory while the event loop is also doing
   * the encoding. Sequential keeps the peak at one file and makes the failure
   * report attributable: the caller learns *which* file collided, which a
   * `Promise.all` rejection cannot say.
   */
  const upload = useMutation({
    mutationFn: async (variables: { parent: string; files: File[]; overwrite?: boolean }) => {
      for (const file of variables.files) {
        if (file.size > MAX_ATTACHMENT_BYTES) {
          throw new Error(
            `${file.name} is ${formatBytes(file.size)}; the limit is ${formatBytes(
              MAX_ATTACHMENT_BYTES,
            )}.`,
          );
        }
        await api.files.upload(sessionId, {
          path: variables.parent,
          name: file.name,
          data: await readBase64(file),
          overwrite: variables.overwrite,
        });
      }
    },
    onMutate: (variables) => setUploadingInto(variables.parent),
    onSettled: () => setUploadingInto(null),
    onSuccess: async (_result, variables) => {
      onWrote?.(variables.parent);
      await refresh();
      close();
    },
    onError: (cause, variables) => {
      // A collision is a question, not a failure: the user picked these files, so
      // the useful next step is offering to replace rather than reporting a 400.
      const collision = collidingName(cause);
      if (collision && !variables.overwrite) {
        setError(undefined);
        setPending({
          kind: "overwrite",
          parent: variables.parent,
          files: variables.files,
          name: collision,
        });
        return;
      }
      setError(describe(cause));
    },
  });

  const uploadInto = useCallback(
    (parent: string, files: File[]) => {
      if (files.length > 0) upload.mutate({ parent, files });
    },
    [upload],
  );

  const requestUpload = useCallback((parent: string) => {
    uploadTarget.current = parent;
    // Cleared first so re-picking the same file still fires `change`.
    if (inputRef.current) inputRef.current.value = "";
    inputRef.current?.click();
  }, []);

  const busy = mkdir.isPending || remove.isPending || upload.isPending;

  return {
    requestMkdir: useCallback((parent: string) => setPending({ kind: "mkdir", parent }), []),
    requestUpload,
    uploadInto,
    requestDelete: useCallback(
      (entry: { path: string; name: string; isDir: boolean }) =>
        setPending({ kind: "delete", ...entry }),
      [],
    ),
    busy,
    uploadingInto,
    surface: (
      <>
        {/*
          One input for the whole pane, always mounted so `requestUpload` can click
          it. `hidden` rather than a visually-hidden class: it is never a tab stop
          and never announced — every route to it is a labelled button.
        */}
        <input
          ref={inputRef}
          type="file"
          multiple
          hidden
          onChange={(event) => {
            const files = Array.from(event.target.files ?? []);
            if (files.length > 0) upload.mutate({ parent: uploadTarget.current, files });
          }}
        />

        {pending?.kind === "mkdir" && (
          <PromptDialog
            title="New folder"
            description={
              pending.parent ? (
                <>
                  Inside <span className="font-mono">{pending.parent}</span>
                </>
              ) : (
                "In the session's working directory"
              )
            }
            label="Folder name"
            placeholder="name"
            confirmLabel="Create"
            busy={mkdir.isPending}
            error={error}
            onSubmit={(name) => {
              setError(undefined);
              mkdir.mutate({ parent: pending.parent, name });
            }}
            onCancel={close}
          />
        )}

        {pending?.kind === "delete" && (
          <AlertDialog
            title={pending.isDir ? "Delete this folder?" : "Delete this file?"}
            description={
              <>
                <span className="font-mono text-ink">{pending.name}</span>
                {pending.isDir
                  ? " and everything inside it will be deleted from disk."
                  : " will be deleted from disk."}{" "}
                {/* Said plainly, because it is the fact that decides the answer:
                    there is no trash on this path, by design. */}
                This cannot be undone.
              </>
            }
            confirmLabel="Delete"
            danger
            busy={remove.isPending}
            error={error}
            onConfirm={() => {
              setError(undefined);
              remove.mutate({ path: pending.path, isDir: pending.isDir });
            }}
            onCancel={close}
          />
        )}

        {pending?.kind === "overwrite" && (
          <AlertDialog
            title="Replace the existing file?"
            description={
              <>
                <span className="font-mono text-ink">{pending.name}</span> already exists in
                this folder. Replacing it overwrites its contents.
              </>
            }
            confirmLabel="Replace"
            danger
            busy={upload.isPending}
            error={error}
            onConfirm={() => {
              setError(undefined);
              upload.mutate({
                parent: pending.parent,
                files: pending.files,
                overwrite: true,
              });
            }}
            onCancel={close}
          />
        )}
      </>
    ),
  };
}

/**
 * The filename in an overwrite refusal.
 *
 * The backend answers `INVALID_REQUEST` with "<name> already exists. Send
 * overwrite to replace it." Matching the *code* plus the sentence, rather than
 * the sentence alone, keeps an unrelated 400 from being offered as a replace —
 * and if that message is ever reworded, the collision simply reports as an error
 * instead of silently offering to overwrite the wrong thing.
 */
function collidingName(cause: unknown): string | null {
  if (!(cause instanceof ApiError) || cause.code !== "INVALID_REQUEST") return null;
  const match = /^(.+) already exists\./.exec(cause.message);
  return match?.[1] ?? null;
}

function describe(cause: unknown): string {
  if (cause instanceof ApiError) {
    switch (cause.code) {
      case "FILE_NOT_FOUND":
        return "That path no longer exists on disk.";
      case "POLICY_VIOLATION":
        return "That path is outside the session's working directory.";
      default:
        return cause.message;
    }
  }
  return cause instanceof Error ? cause.message : "Something went wrong.";
}

/**
 * Read a file as bare base64.
 *
 * The same split `attachment-picker` performs, repeated rather than shared
 * because that module's `readAttachments` also enforces a count and a total
 * budget across a *staged set* — limits that mean nothing for a directory
 * upload, where the destination is a filesystem rather than one message.
 */
function readBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result);
      const comma = result.indexOf(",");
      resolve(comma === -1 ? result : result.slice(comma + 1));
    };
    reader.onerror = () =>
      reject(new Error(`Could not read ${file.name}. The file may be unreadable.`));
    reader.readAsDataURL(file);
  });
}
