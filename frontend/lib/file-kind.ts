/**
 * Which viewer a file gets, and where its bytes come from.
 *
 * The server already classifies each tree entry, so this exists for the cases the
 * hint does not cover: a file opened from a tool call in the conversation, where
 * no listing was involved, and the text/Markdown split that the server reports
 * through `mimeType` instead. Kept in one table so the tree, the viewer, and the
 * context menu cannot disagree about what a file is.
 */

import type { FileEntry, PreviewKind } from "@/lib/api";

/**
 * A viewer family. Wider than `PreviewKind`, which only names the families the
 * server has to classify: `text` and `markdown` are decided from the bytes.
 */
export type ViewerKind = PreviewKind | "text" | "markdown";

/** Which endpoint supplies a viewer's data. */
export type Source = "content" | "document" | "raw";

const GRID_EXTENSIONS = ["xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv", "tab"];
const BLOCK_EXTENSIONS = ["docx", "pptx"];
const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "ico", "avif"];
const MARKDOWN_EXTENSIONS = ["md", "markdown"];

/** Lowercase extension without the dot, or `""` when there is none. */
export function extensionOf(path: string): string {
  const name = path.split("/").pop() ?? path;
  // A dotfile with no second dot (`.gitignore`) has no extension: the leading
  // dot is part of the name. Treating it as one would call it a "gitignore file".
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return "";
  return name.slice(dot + 1).toLowerCase();
}

/**
 * Classify by extension.
 *
 * Falls back to `text`, matching the server's own default: a file with no
 * extension is usually readable source, and refusing to preview it would be
 * worse than showing bytes that turn out to be binary — which `content` reports
 * accurately anyway.
 */
export function viewerKindFor(path: string): ViewerKind {
  const extension = extensionOf(path);
  if (GRID_EXTENSIONS.includes(extension)) return "grid";
  if (BLOCK_EXTENSIONS.includes(extension)) return "blocks";
  if (IMAGE_EXTENSIONS.includes(extension)) return "image";
  if (MARKDOWN_EXTENSIONS.includes(extension)) return "markdown";
  if (extension === "html" || extension === "htm") return "html";
  if (extension === "pdf") return "pdf";
  return "text";
}

/**
 * Prefer the server's classification, falling back to the extension.
 *
 * The server saw the file; this side only sees a string. But a file opened from a
 * tool call has no tree entry, so the fallback has to exist.
 */
export function viewerKindOf(path: string, hint?: PreviewKind): ViewerKind {
  return hint ?? viewerKindFor(path);
}

/** Which endpoint a viewer reads from. */
export function sourceFor(kind: ViewerKind): Source {
  switch (kind) {
    case "grid":
    case "blocks":
      return "document";
    case "html":
    case "pdf":
      return "raw";
    default:
      // Images included: an SVG must not be served as a renderable type from our
      // own origin, where it could execute script, so every image travels the
      // base64 path inside `content`.
      return "content";
  }
}

/**
 * Whether a raw-byte URL is safe for this file.
 *
 * SVG is the exception that makes this function necessary. The backend refuses to
 * serve it as `image/svg+xml`, so asking for its bytes yields an attachment
 * rather than an image; it has to go through the base64 path instead.
 */
export function servesRawBytes(path: string): boolean {
  const extension = extensionOf(path);
  if (extension === "svg") return false;
  const kind = viewerKindFor(path);
  return kind === "pdf" || kind === "html" || kind === "image";
}

/** Glyph for a tree row, so a spreadsheet is recognizable before opening it. */
export function glyphFor(entry: Pick<FileEntry, "isDir" | "path" | "previewKind">): string {
  if (entry.isDir) return "▮";
  switch (viewerKindOf(entry.path, entry.previewKind)) {
    case "grid":
      return "▦";
    case "blocks":
      return "▤";
    case "image":
      return "▩";
    case "pdf":
      return "▧";
    case "html":
      return "◈";
    case "markdown":
      return "▪";
    default:
      return "▫";
  }
}

/** Human-readable family name, for the viewer header and the context menu. */
export function describeKind(kind: ViewerKind): string {
  switch (kind) {
    case "grid":
      return "Table";
    case "blocks":
      return "Document";
    case "image":
      return "Image";
    case "pdf":
      return "PDF";
    case "html":
      return "Web page";
    case "markdown":
      return "Markdown";
    default:
      return "Text";
  }
}
