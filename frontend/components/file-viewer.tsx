"use client";

/**
 * One file, at either scale.
 *
 * This owns the header — back, breadcrumb, expand, reload, close — and chooses a
 * viewer. The viewers beneath it are scale-agnostic: they lay out fluidly and know
 * nothing about whether they sit in the 320px pane or the full-screen dialog,
 * which is why fullscreen needs no second rendering path.
 *
 * # Lazy loading
 *
 * Every format viewer is a `next/dynamic` import. The Artifacts pane is already
 * split out of first-load JS, and these split again beneath it, so opening a `.ts`
 * file never fetches the table or document code. With ~4 KB of first-load budget
 * left, that is not an optimization but the condition for the feature existing.
 *
 * # Which endpoint
 *
 * Decided before any request, from the server's `previewKind` hint on the tree
 * entry, with an extension fallback for files opened from a tool call. A
 * mislabelled file is the one case a table cannot settle, so `document` answering
 * `INVALID_REQUEST` falls back to `content`, which decides text-vs-binary from the
 * bytes themselves.
 */

import dynamic from "next/dynamic";
import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ApiError, api, type PreviewKind } from "@/lib/api";
import { formatBytes, formatRelative } from "@/lib/format";
import { describeKind, sourceFor, viewerKindOf, type ViewerKind } from "@/lib/file-kind";
import { CodeBlock } from "@/components/code-block";
import { Markdown } from "@/components/markdown";
import { Button, IconButton } from "@/components/ui/button";
import {
  ArrowLeftIcon,
  MaximizeIcon,
  RefreshIcon,
  XIcon,
} from "@/components/icons";

const GridView = dynamic(
  () => import("@/components/viewers/grid-view").then((m) => m.GridView),
  { ssr: false, loading: () => <Loading label="table" /> },
);
const DocumentView = dynamic(
  () => import("@/components/viewers/document-view").then((m) => m.DocumentView),
  { ssr: false, loading: () => <Loading label="document" /> },
);
const HtmlView = dynamic(
  () => import("@/components/viewers/html-view").then((m) => m.HtmlView),
  { ssr: false, loading: () => <Loading label="page" /> },
);
const ImageView = dynamic(
  () => import("@/components/viewers/media-view").then((m) => m.ImageView),
  { ssr: false, loading: () => <Loading label="image" /> },
);
const PdfView = dynamic(
  () => import("@/components/viewers/media-view").then((m) => m.PdfView),
  { ssr: false, loading: () => <Loading label="PDF" /> },
);

export function FileViewer({
  sessionId,
  path,
  previewKind,
  onBack,
  onClose,
  onExpand,
  registerExpand,
  scale = "pane",
}: {
  sessionId: string;
  path: string;
  previewKind?: PreviewKind;
  /** Absent in the full-screen viewer, which is dismissed rather than navigated. */
  onBack?: () => void;
  onClose?: () => void;
  onExpand?: () => void;
  /**
   * Hands the expand button to the pane.
   *
   * Opening fullscreen unmounts this button — the pane shows a placeholder while
   * the dialog is up — so `Overlay` cannot restore focus to it: its `isConnected`
   * guard correctly refuses to focus a detached node. The pane therefore holds a
   * live reference and restores focus itself once the button remounts.
   */
  registerExpand?: (node: HTMLButtonElement | null) => void;
  scale?: "pane" | "full";
}) {
  const kind = viewerKindOf(path, previewKind);
  const name = path.split("/").pop() ?? path;
  const directory = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "";
  const [reloadToken, setReloadToken] = useState(0);
  const sectionRef = useRef<HTMLElement>(null);

  // Swapping the tree out hides the node that had focus, which would drop focus
  // to <body>. The same class of bug the Overlay focus comments guard against,
  // handled here because this transition involves no dialog.
  //
  // Keyed on `path` alone, deliberately *not* on every mount: the pane viewer
  // re-renders when the fullscreen dialog closes, and taking focus there would
  // steal it back from the Expand button that `Overlay` had just restored it to.
  // A ref holds the last path focused for, so returning from fullscreen to the
  // same file is a no-op.
  const focusedFor = useRef<string | null>(null);
  useEffect(() => {
    if (scale !== "pane") return;
    if (focusedFor.current === path) return;
    focusedFor.current = path;
    sectionRef.current?.focus();
  }, [scale, path]);

  return (
    <section
      ref={sectionRef}
      tabIndex={-1}
      aria-label={`${describeKind(kind)} preview of ${name}`}
      className="flex min-h-0 flex-1 flex-col overflow-hidden outline-none"
    >
      <header className="flex shrink-0 items-center gap-1 border-b border-line bg-raised px-1.5 py-1">
        {onBack && (
          // Keeps its "Files" label rather than becoming an icon: this is the only
          // route back to the tree, and an unlabelled arrow beside a filename reads
          // as "previous file". E2E matches it by name.
          <Button
            variant="ghost"
            onClick={onBack}
            title="Back to the file tree"
            icon={<ArrowLeftIcon size={13} />}
            className="shrink-0"
          >
            Files
          </Button>
        )}

        <div className="min-w-0 flex-1">
          <h3 className="truncate text-2xs font-medium text-ink" title={path}>
            {name}
          </h3>
          {directory && (
            <p
              className="truncate font-mono text-3xs leading-tight text-ink-muted"
              title={directory}
            >
              {directory}
            </p>
          )}
        </div>

        <IconButton
          label="Reload this file from disk"
          onClick={() => setReloadToken((token) => token + 1)}
        >
          <RefreshIcon size={14} />
        </IconButton>

        {onExpand && (
          <IconButton label="Open full screen" ref={registerExpand} onClick={onExpand}>
            <MaximizeIcon size={14} />
          </IconButton>
        )}

        {onClose && (
          <IconButton label="Close preview" onClick={onClose}>
            <XIcon size={14} />
          </IconButton>
        )}
      </header>

      {sourceFor(kind) === "document" ? (
        <DocumentBody
          key={`${path}:${reloadToken}`}
          sessionId={sessionId}
          path={path}
          name={name}
        />
      ) : (
        <ContentBody
          key={`${path}:${reloadToken}`}
          sessionId={sessionId}
          path={path}
          name={name}
          kind={kind}
        />
      )}
    </section>
  );
}

/**
 * A parsed document: spreadsheet, CSV, Word, or PowerPoint.
 *
 * Sheet and window live in the query key, which is what makes switching back to
 * an already-viewed sheet a cache hit rather than another ~500 ms parse.
 *
 * Paging *replaces* the window rather than accumulating rows. Accumulation was
 * the first design and it is the wrong one twice over: it needs the previous
 * window as state, which means either an effect that cascades a render or a ref
 * read during render, and it grows the DOM without bound while every row is laid
 * out again on each append. Replacing keeps the table at one window
 * permanently — measured at 46 ms to build — so the grid stays cheap no matter how
 * far the user pages, and React reconciles a fixed number of rows.
 */
function DocumentBody({
  sessionId,
  path,
  name,
}: {
  sessionId: string;
  path: string;
  name: string;
}) {
  const [sheet, setSheet] = useState(0);
  const [row, setRow] = useState(0);

  const { data, isPending, isFetching, error } = useQuery({
    queryKey: ["file-document", sessionId, path, sheet, row],
    queryFn: () => api.files.document(sessionId, path, { sheet, row }),
    // Hold the previous window on screen while the next one loads, so paging
    // does not blank the table and lose the user's place.
    placeholderData: (previous) => previous,
    staleTime: 5_000,
  });

  const selectSheet = useCallback((index: number) => {
    setSheet(index);
    setRow(0);
  }, []);

  const grid = data?.grid;

  if (isPending && !grid) return <Loading label={name} />;
  if (error) return <BodyError error={error} />;

  if (data?.kind === "grid" && grid) {
    return (
      <GridView
        grid={grid}
        onSelectSheet={selectSheet}
        onPage={setRow}
        loading={isFetching}
      />
    );
  }

  if (data?.kind === "blocks" && data.blocks) {
    return <DocumentView payload={data.blocks} slides={data.format === "pptx"} />;
  }

  return null;
}

/** Text, Markdown, images, HTML, and PDF — everything read through `content`. */
function ContentBody({
  sessionId,
  path,
  name,
  kind,
}: {
  sessionId: string;
  path: string;
  name: string;
  kind: ViewerKind;
}) {
  const [renderedMarkdown, setRenderedMarkdown] = useState(true);

  // PDF is the one kind needing no JSON at all: the browser fetches the bytes
  // itself from the raw route, so there is nothing to await here.
  const needsContent = kind !== "pdf";

  const { data, isPending, error } = useQuery({
    queryKey: ["file-content", sessionId, path],
    queryFn: () => api.files.content(sessionId, path),
    enabled: needsContent,
    // The agent is actively editing these files, so a cached preview goes stale
    // quickly. Short staleness plus an explicit reload beats polling.
    staleTime: 5_000,
  });

  if (kind === "pdf") {
    return <PdfView src={api.files.rawUrl(sessionId, path)} name={name} />;
  }

  if (isPending) return <Loading label={name} />;
  if (error) return <BodyError error={error} />;
  if (!data) return null;

  if (kind === "image") {
    // SVG stays on the base64 path: served as `image/svg+xml` from our origin it
    // could execute script, whereas inside an `<img>` it cannot. Rasters take the
    // raw route for progressive decode and caching.
    const src =
      data.encoding === "image" && data.content
        ? `data:${data.mimeType};base64,${data.content}`
        : api.files.rawUrl(sessionId, path);

    if (data.truncated && data.encoding !== "image") {
      return (
        <p className="p-3 text-2xs text-ink-muted">
          This image is {formatBytes(data.size)}, larger than the preview limit.
        </p>
      );
    }
    return <ImageView src={src} name={name} />;
  }

  if (kind === "html" && data.encoding === "text") {
    return <HtmlView html={data.content ?? ""} name={name} path={path} />;
  }

  if (data.encoding === "binary") {
    return (
      <p className="p-3 text-2xs text-ink-muted">
        Binary file ({formatBytes(data.size)}). No text preview available.
      </p>
    );
  }

  const isMarkdown = data.mimeType === "text/markdown";
  const isJson = data.mimeType === "application/json" || name.endsWith(".json");

  return (
    <div className="min-h-0 flex-1 overflow-auto overscroll-contain p-2">
      {isMarkdown && (
        <div className="mb-1.5 flex justify-end">
          <Button variant="secondary" onClick={() => setRenderedMarkdown(!renderedMarkdown)}>
            {renderedMarkdown ? "View source" : "View rendered"}
          </Button>
        </div>
      )}

      {data.truncated && (
        <p className="mb-1.5 rounded border border-warning-line bg-warning-bg px-2 py-1 text-3xs text-warning">
          Showing the first part of a {formatBytes(data.size)} file.
        </p>
      )}

      {isMarkdown && renderedMarkdown ? (
        <div className="text-xs leading-relaxed text-ink">
          <Markdown text={data.content ?? ""} />
        </div>
      ) : (
        <CodeBlock
          code={isJson ? prettyJson(data.content ?? "") : (data.content ?? "")}
          language={languageLabel(name, data.mimeType)}
          // A preview pane is for reading the file, so the whole content renders
          // and the pane itself scrolls.
          maxLines={Number.MAX_SAFE_INTEGER}
        />
      )}

      <p className="mt-1.5 text-3xs text-ink-muted">
        Modified {formatRelative(data.modifiedAtMs)} ·{" "}
        <span className="font-mono">{data.absolutePath}</span>
      </p>
    </div>
  );
}

/**
 * Phrase a failed read so the reader knows whether it is the file or the backend.
 *
 * The distinction that matters is the same one the tree already makes: a missing
 * file and a backend that cannot serve this kind of file arrive with the same
 * status and differ only by code.
 */
function BodyError({ error }: { error: unknown }) {
  let message = "Unknown error.";
  if (error instanceof ApiError) {
    switch (error.code) {
      case "FILE_NOT_FOUND":
        message = "This file no longer exists on disk.";
        break;
      case "POLICY_VIOLATION":
        message = "This file is outside the session's working directory.";
        break;
      default:
        message = error.message;
    }
  } else if (error instanceof Error) {
    message = error.message;
  }

  return (
    <div role="alert" className="p-3 text-2xs">
      <p className="font-medium text-danger">Could not read this file</p>
      <p className="mt-1 text-danger">{message}</p>
    </div>
  );
}

function Loading({ label }: { label: string }) {
  return (
    <output aria-live="polite" className="block p-3 text-2xs text-ink-muted">
      Loading {label}...
    </output>
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
