"use client";

/**
 * Images, with fit-to-window and zoom.
 *
 * Two byte sources, and the split is a security decision rather than a
 * convenience. A raster image loads from `/files/raw`, which is what buys
 * progressive decode and HTTP caching. An SVG does not: served as
 * `image/svg+xml` from our own origin it can execute script, so it keeps
 * travelling the base64 data-URL path, where an `<img>` element renders it inert.
 *
 * `next/image` is skipped for the same reason as before — it optimizes remote
 * URLs, and there is nothing to optimize about a local file behind an
 * authenticated route.
 */

import { useState } from "react";
import { useT } from "@/lib/i18n";

/** Zoom steps, in the ratios image viewers conventionally offer. */
const STEPS = [0.25, 0.5, 1, 1.5, 2, 4] as const;

export function ImageView({ src, name }: { src: string; name: string }) {
  // Keyed on `src`, so opening another image mounts a fresh viewer rather than
  // inheriting the previous one's zoom and error state. Resetting that in an
  // effect would cascade a render to undo state React had just committed; a key
  // states the intent — this is a different image — in one line.
  return (
    // `Image` renders the <img> with `alt={name}`; the rule cannot see through
    // the wrapper component to know that.
    // eslint-disable-next-line jsx-a11y/alt-text
    <Image key={src} src={src} name={name} />
  );
}

function Image({ src, name }: { src: string; name: string }) {
  const t = useT();
  /** `null` means fit-to-window, which is the useful default in a narrow pane. */
  const [zoom, setZoom] = useState<number | null>(null);
  const [natural, setNatural] = useState<{ w: number; h: number } | null>(null);
  const [failed, setFailed] = useState(false);

  function step(direction: 1 | -1) {
    // Zooming from fit starts at 100%, which is where a user expects the first
    // click to land regardless of how the image happened to be scaled.
    const current = zoom ?? 1;
    const index = STEPS.findIndex((value) => value >= current);
    const next = STEPS[Math.min(Math.max(index + direction, 0), STEPS.length - 1)];
    setZoom(next);
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-1.5 border-b border-line bg-sunken px-2 py-1">
        <span className="flex-1 truncate text-3xs text-ink-muted">
          {natural ? `${natural.w} × ${natural.h}` : ""}
        </span>
        <button
          type="button"
          onClick={() => step(-1)}
          aria-label={t("media.zoomOut")}
          className="rounded px-1.5 text-2xs text-ink-muted hover:bg-hover focus-ring"
        >
          −
        </button>
        <button
          type="button"
          onClick={() => setZoom(zoom === null ? 1 : null)}
          className="rounded border border-line bg-raised px-1.5 py-0.5 text-3xs font-medium text-ink-soft hover:bg-hover focus-ring"
        >
          {zoom === null ? t("media.fit") : `${Math.round(zoom * 100)}%`}
        </button>
        <button
          type="button"
          onClick={() => step(1)}
          aria-label={t("media.zoomIn")}
          className="rounded px-1.5 text-2xs text-ink-muted hover:bg-hover focus-ring"
        >
          +
        </button>
      </div>

      {/* A checkerboard, so a transparent PNG reads as transparent rather than
          as white-on-white. */}
      <div
        className="min-h-0 flex-1 overflow-auto overscroll-contain p-2"
        style={{
          backgroundImage:
            "linear-gradient(45deg, #f3f4f6 25%, transparent 25%, transparent 75%, #f3f4f6 75%), linear-gradient(45deg, #f3f4f6 25%, transparent 25%, transparent 75%, #f3f4f6 75%)",
          backgroundSize: "16px 16px",
          backgroundPosition: "0 0, 8px 8px",
        }}
      >
        {failed ? (
          <p role="alert" className="text-2xs text-danger">
            {t("media.decodeFailed")}
          </p>
        ) : (
          <div className={zoom === null ? "flex h-full items-center justify-center" : ""}>
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={src}
              alt={name}
              onLoad={(event) =>
                setNatural({
                  w: event.currentTarget.naturalWidth,
                  h: event.currentTarget.naturalHeight,
                })
              }
              onError={() => setFailed(true)}
              className={zoom === null ? "max-h-full max-w-full object-contain" : ""}
              style={
                zoom === null
                  ? undefined
                  : {
                      width: natural ? natural.w * zoom : undefined,
                      // `image-rendering: pixelated` past 100% so a zoomed
                      // screenshot shows its pixels instead of a blur.
                      imageRendering: zoom > 1 ? "pixelated" : "auto",
                      maxWidth: "none",
                    }
              }
            />
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * A PDF, handed to the browser's own viewer.
 *
 * An `<iframe>` against the raw route rather than a JS renderer: pdf.js is ~350 KB
 * against a bundle budget with about 4 KB free, and every target browser already
 * ships a competent PDF viewer with text selection, search, and printing.
 *
 * The route sends `Content-Security-Policy: sandbox`, so the document is in an
 * opaque origin regardless of this attribute; `sandbox` is repeated here to keep
 * the guarantee local to the element rather than dependent on a header set
 * elsewhere.
 */
export function PdfView({ src, name }: { src: string; name: string }) {
  const t = useT();
  return (
    <iframe
      src={src}
      title={t("media.previewTitle", { name })}
      sandbox=""
      className="min-h-0 w-full flex-1 border-0 bg-hover"
    />
  );
}
