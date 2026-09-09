"use client";

/**
 * Reading local files into staged attachments.
 *
 * This was a full component with its own dropzone until the composer absorbed
 * that UI — attachments belong inside the input they are attached to, not in a
 * strip below it. What remains is the part that was never presentational: the
 * limits, the base64 encoding, and the accept/reject accounting.
 *
 * Kept as its own module because the limits must stay next to the code that
 * enforces them, and because `readAttachments` is the one place that knows the
 * backend wants bare base64 while a thumbnail wants a full data URL.
 */

import type { AttachmentPayload } from "@/lib/api-types";
import { formatBytes } from "@/lib/format";

/**
 * Limits mirrored from the backend (`crates/server/src/attachments.rs`).
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

/**
 * Encode `files` and append them to `existing`, enforcing the shared limits.
 *
 * Returns the full next list rather than only the additions, so a caller can
 * treat it as the new state, plus a human-readable reason for each rejection.
 * Rejections are collected rather than thrown: dropping five files of which one
 * is too large should attach the other four and say what happened to the fifth.
 */
export async function readAttachments(
  files: FileList,
  existing: StagedAttachment[],
): Promise<{ accepted: StagedAttachment[]; rejected: string[] }> {
  const next = [...existing];
  const rejected: string[] = [];
  let running = existing.reduce((sum, item) => sum + item.size, 0);

  for (const file of Array.from(files)) {
    if (next.length >= MAX_ATTACHMENTS) {
      rejected.push(`${file.name}: at most ${MAX_ATTACHMENTS} attachments per message`);
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
        // Browsers leave `type` empty for unknown extensions; the backend only
        // records the value, so a generic fallback is safe.
        type: file.type || "application/octet-stream",
        data: base64,
        size: file.size,
        previewUrl: file.type.startsWith("image/") ? url : undefined,
      });
      running += file.size;
    } catch (error) {
      rejected.push(error instanceof Error ? error.message : `${file.name}: unreadable`);
    }
  }

  return { accepted: next, rejected };
}
