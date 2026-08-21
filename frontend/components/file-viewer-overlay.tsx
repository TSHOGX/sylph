"use client";

/**
 * The file viewer, full screen.
 *
 * Wraps the same `FileViewer` the pane uses in the shell's `Overlay`, so there is
 * one rendering path for both scales and no second copy to keep in sync.
 *
 * `Overlay` is reused rather than reimplemented because it already gets the three
 * things a modal is easy to get subtly wrong: a focus trap, focus restoration to
 * the opener (guarded against the opener having unmounted), and Escape that stops
 * propagating so the shell's document listener does not also fire. Those are WCAG
 * 2.4.3 obligations, and reimplementing them here would mean two chances to fail.
 *
 * Fullscreen is tracked by its own boolean on the shell rather than as a fourth
 * member of the `Overlay` union. The union answers "which overlay is up"; a file
 * viewer additionally needs to know *which file*, and that already lives beside it
 * in `artifact`. Adding a payload to the union would force every existing consumer
 * to handle a case it does not care about.
 */

import { Overlay } from "@/components/overlay";
import { FileViewer } from "@/components/file-viewer";
import { viewerKindOf } from "@/lib/file-kind";
import type { PreviewKind } from "@/lib/api";

export function FileViewerOverlay({
  sessionId,
  path,
  previewKind,
  onClose,
}: {
  sessionId: string;
  path: string;
  previewKind?: PreviewKind;
  onClose: () => void;
}) {
  const name = path.split("/").pop() ?? path;

  // A rendered page or a PDF lives in an iframe, which reports no intrinsic size,
  // so it needs a dialog that claims the viewport rather than one that measures
  // its content. Everything else — tables, documents, images, text — has a real
  // size worth fitting to, so a small file gets a small dialog.
  const kind = viewerKindOf(path, previewKind);
  const size = kind === "html" || kind === "pdf" ? "viewer-full" : "viewer";

  return (
    <Overlay title={`Preview of ${name}`} onClose={onClose} size={size}>
      {/* No `onBack`: there is no tree behind this surface to go back to, and
          Escape or the close button is how a dialog is left. */}
      <FileViewer
        sessionId={sessionId}
        path={path}
        previewKind={previewKind}
        onClose={onClose}
        scale="full"
      />
    </Overlay>
  );
}
