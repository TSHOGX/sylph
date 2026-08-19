"use client";

import { useId, useRef, useState } from "react";
import type { AttachmentPayload } from "@/lib/api";
import { formatBytes } from "@/lib/format";

/**
 * Limits mirrored from the backend (`backend/src/attachments.rs`).
 *
 * Enforcing them client-side is a UX affordance, not the security boundary:
 * the backend re-checks every payload and is the authority. Keeping the numbers
 * identical means a user learns about an oversized file before spending time
 * base64-encoding it and waiting on a 400.
 */
export const MAX_ATTACHMENTS = 5;
export const MAX_ATTACHMENT_BYTES = 8 * 1024 * 1024;
export const MAX_TOTAL_BYTES = 10 * 1024 * 1024;

export interface StagedAttachment extends AttachmentPayload {
  /** Local-only id for list keys and removal. */
  id: string;
  size: number;
  /** Data URL retained for image thumbnails; absent for other types. */
  previewUrl?: string;
}

interface Props {
  attachments: StagedAttachment[];
  onChange: (next: StagedAttachment[]) => void;
  disabled?: boolean;
}

/**
 * Split a `FileReader.readAsDataURL` result into its base64 payload.
 *
 * The backend expects bare base64, not a `data:` URL, so the prefix is removed
 * before sending. The full URL is kept separately for thumbnails, which avoids
 * encoding the same bytes twice.
 */
function splitDataUrl(dataUrl: string): { base64: string; url: string } {
  const comma = dataUrl.indexOf(",");
  return {
    base64: comma === -1 ? dataUrl : dataUrl.slice(comma + 1),
    url: dataUrl,
  };
}

function readFile(file: File): Promise<{ base64: string; url: string }> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(splitDataUrl(String(reader.result)));
    reader.onerror = () =>
      reject(new Error(`Could not read ${file.name}. The file may be unreadable.`));
    reader.readAsDataURL(file);
  });
}

export function AttachmentPicker({ attachments, onChange, disabled }: Props) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputId = useId();
  const problemId = useId();

  const totalBytes = attachments.reduce((sum, item) => sum + item.size, 0);

  async function accept(files: FileList | null) {
    if (!files || files.length === 0) return;
    setProblem(null);
    setBusy(true);

    const next = [...attachments];
    let running = totalBytes;
    const rejected: string[] = [];

    for (const file of Array.from(files)) {
      if (next.length >= MAX_ATTACHMENTS) {
        rejected.push(
          `${file.name}: at most ${MAX_ATTACHMENTS} attachments per message`,
        );
        continue;
      }
      if (file.size > MAX_ATTACHMENT_BYTES) {
        rejected.push(
          `${file.name} is ${formatBytes(file.size)}; the limit is ${formatBytes(
            MAX_ATTACHMENT_BYTES,
          )} per file`,
        );
        continue;
      }
      if (running + file.size > MAX_TOTAL_BYTES) {
        rejected.push(
          `${file.name} would push this message past the ${formatBytes(
            MAX_TOTAL_BYTES,
          )} total limit`,
        );
        continue;
      }

      try {
        const { base64, url } = await readFile(file);
        next.push({
          id: `${file.name}-${file.size}-${Date.now()}-${next.length}`,
          name: file.name,
          // Browsers leave `type` empty for unknown extensions; the backend
          // only records the value, so a generic fallback is safe.
          type: file.type || "application/octet-stream",
          data: base64,
          size: file.size,
          previewUrl: file.type.startsWith("image/") ? url : undefined,
        });
        running += file.size;
      } catch (readError) {
        rejected.push(
          readError instanceof Error ? readError.message : `${file.name}: unreadable`,
        );
      }
    }

    onChange(next);
    setBusy(false);
    if (rejected.length > 0) setProblem(rejected.join("; "));
    // Clear the input so re-picking the same file fires another change event.
    if (inputRef.current) inputRef.current.value = "";
  }

  function remove(id: string) {
    onChange(attachments.filter((item) => item.id !== id));
    setProblem(null);
  }

  return (
    <div className="space-y-2">
      <div
        onDragOver={(e) => {
          if (disabled) return;
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(e) => {
          if (disabled) return;
          e.preventDefault();
          setDragging(false);
          void accept(e.dataTransfer.files);
        }}
        className={`rounded-lg border border-dashed px-3 py-2 text-sm transition ${
          dragging ? "border-gray-900 bg-gray-50" : "border-gray-300"
        } ${disabled ? "opacity-50" : ""}`}
      >
        <label
          htmlFor={inputId}
          className="cursor-pointer font-medium text-gray-900 underline decoration-dotted focus-within:outline-none"
        >
          Attach files
        </label>
        <span className="text-gray-500"> or drop them here.</span>
        <span className="ml-1 text-xs text-gray-500">
          Up to {MAX_ATTACHMENTS} files, {formatBytes(MAX_TOTAL_BYTES)} total.
        </span>
        <input
          ref={inputRef}
          id={inputId}
          type="file"
          multiple
          disabled={disabled || busy}
          aria-describedby={problem ? problemId : undefined}
          onChange={(e) => void accept(e.target.files)}
          className="sr-only"
        />
      </div>

      {busy && (
        <output aria-live="polite" className="block text-xs text-gray-500">
          Encoding attachments...
        </output>
      )}

      {problem && (
        <p id={problemId} role="alert" className="text-xs text-red-700">
          {problem}
        </p>
      )}

      {attachments.length > 0 && (
        <ul className="flex flex-wrap gap-2" aria-label="Staged attachments">
          {attachments.map((item) => (
            <li
              key={item.id}
              className="flex items-center gap-2 rounded border border-gray-200 bg-white px-2 py-1 text-xs"
            >
              {item.previewUrl ? (
                // eslint-disable-next-line @next/next/no-img-element
                <img
                  src={item.previewUrl}
                  alt=""
                  className="h-8 w-8 rounded object-cover"
                />
              ) : (
                <span
                  aria-hidden="true"
                  className="flex h-8 w-8 items-center justify-center rounded bg-gray-100 text-gray-500"
                >
                  ⎘
                </span>
              )}
              <span className="max-w-[12rem] truncate font-medium" title={item.name}>
                {item.name}
              </span>
              <span className="text-gray-500">{formatBytes(item.size)}</span>
              <button
                type="button"
                onClick={() => remove(item.id)}
                disabled={disabled}
                aria-label={`Remove ${item.name}`}
                className="rounded px-1 text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}

      {attachments.length > 0 && (
        <p className="text-xs text-gray-500">
          {attachments.length} of {MAX_ATTACHMENTS} files · {formatBytes(totalBytes)} of{" "}
          {formatBytes(MAX_TOTAL_BYTES)}
        </p>
      )}
    </div>
  );
}
