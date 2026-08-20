"use client";

/**
 * File preview for the Artifacts pane.
 *
 * The viewer is chosen from the server's `encoding` field, which is derived from
 * the bytes rather than the extension. That matters for two common cases: a
 * `.log` holding binary output must not render as mojibake, and a file with no
 * extension at all is often perfectly readable source.
 *
 * Markdown gets a rendered view with a toggle back to source, because a rendered
 * README is what you want to read and the source is what you want to edit.
 */

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import { formatBytes, formatRelative } from "@/lib/format";
import { CodeBlock } from "@/components/code-block";
import { Markdown } from "@/components/markdown";

export function FilePreview({
  sessionId,
  path,
  onClose,
}: {
  sessionId: string;
  path: string;
  onClose: () => void;
}) {
  const { data, isPending, error, refetch } = useQuery({
    queryKey: ["file-content", sessionId, path],
    queryFn: () => api.files.content(sessionId, path),
    // The agent is actively editing these files, so a cached preview goes stale
    // quickly. Short staleness plus an explicit reload beats polling every file
    // the user happens to have open.
    staleTime: 5_000,
  });

  const [rendered, setRendered] = useState(true);
  const name = path.split("/").pop() ?? path;
  const isMarkdown = data?.mimeType === "text/markdown";
  const isJson =
    data?.mimeType === "application/json" || name.endsWith(".json");

  return (
    <section aria-label={`Preview of ${name}`} className="flex min-h-0 flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-2 border-b border-gray-200 px-2 py-1">
        <h3 className="min-w-0 flex-1 truncate text-[11px] font-medium text-gray-800" title={path}>
          {name}
        </h3>
        {data && (
          <span className="shrink-0 text-[10px] text-gray-500">
            {formatBytes(data.size)}
          </span>
        )}
        {isMarkdown && (
          <button
            type="button"
            onClick={() => setRendered(!rendered)}
            className="shrink-0 rounded px-1 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
          >
            {rendered ? "Source" : "Rendered"}
          </button>
        )}
        <button
          type="button"
          onClick={() => void refetch()}
          className="shrink-0 rounded px-1 text-[10px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Reload
        </button>
        <button
          type="button"
          onClick={onClose}
          aria-label="Close preview"
          className="shrink-0 rounded px-1 text-[11px] text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          ✕
        </button>
      </header>

      <div className="min-h-0 flex-1 overflow-auto p-2">
        {isPending ? (
          <output aria-live="polite" className="block text-[11px] text-gray-500">
            Loading {name}...
          </output>
        ) : error ? (
          <div role="alert" className="text-[11px]">
            <p className="font-medium text-red-800">Could not read this file</p>
            <p className="mt-1 text-red-700">
              {error instanceof Error ? error.message : "Unknown error."}
            </p>
          </div>
        ) : !data ? null : data.encoding === "image" ? (
          <ImagePreview
            mimeType={data.mimeType}
            base64={data.content}
            name={name}
            truncated={data.truncated}
          />
        ) : data.encoding === "binary" ? (
          <p className="text-[11px] text-gray-500">
            {data.truncated
              ? `This image is ${formatBytes(data.size)}, larger than the preview limit.`
              : `Binary file (${formatBytes(data.size)}). No text preview available.`}
          </p>
        ) : (
          <>
            {data.truncated && (
              <p className="mb-1.5 rounded border border-amber-200 bg-amber-50 px-2 py-1 text-[10px] text-amber-900">
                Showing the first part of a {formatBytes(data.size)} file.
              </p>
            )}
            {isMarkdown && rendered ? (
              <div className="text-[13px] leading-relaxed text-gray-800">
                <Markdown text={data.content ?? ""} />
              </div>
            ) : (
              <CodeBlock
                code={isJson ? prettyJson(data.content ?? "") : (data.content ?? "")}
                language={languageLabel(name, data.mimeType)}
                // A preview pane is for reading the file, so the whole visible
                // content is rendered and the pane itself scrolls.
                maxLines={Number.MAX_SAFE_INTEGER}
              />
            )}
            <p className="mt-1.5 text-[10px] text-gray-500">
              Modified {formatRelative(data.modifiedAtMs)} ·{" "}
              <span className="font-mono">{data.absolutePath}</span>
            </p>
          </>
        )}
      </div>
    </section>
  );
}

function ImagePreview({
  mimeType,
  base64,
  name,
  truncated,
}: {
  mimeType: string;
  base64?: string;
  name: string;
  truncated: boolean;
}) {
  if (!base64 || truncated) {
    return <p className="text-[11px] text-gray-500">This image is too large to preview.</p>;
  }
  return (
    // A data URL, not a proxied file route: the bytes already came through the
    // authenticated JSON endpoint, so no second unauthenticated path to the
    // filesystem needs to exist. `next/image` is skipped deliberately — it
    // optimizes remote URLs, and there is nothing to optimize about a data URL
    // that is already in memory.
    // eslint-disable-next-line @next/next/no-img-element
    <img
      src={`data:${mimeType};base64,${base64}`}
      alt={name}
      className="max-h-full max-w-full object-contain"
    />
  );
}

/** Re-indent JSON so a minified file is readable. Invalid JSON passes through. */
function prettyJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

/** Label for the code block header. Cosmetic: nothing highlights by language. */
function languageLabel(name: string, mimeType: string): string {
  const extension = name.includes(".") ? name.split(".").pop()! : "";
  if (extension) return extension;
  return mimeType.replace(/^\w+\//, "");
}
