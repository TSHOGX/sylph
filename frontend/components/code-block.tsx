"use client";

/**
 * Code display with line numbers, copy, and truncation.
 *
 * # No syntax highlighter
 *
 * Shiki ships a WASM regex engine plus per-language grammars (~200 KB before grammars),
 * and `react-syntax-highlighter` with Prism is ~35 KB gzipped for a useful language set.
 * Neither fits the first-load budget, and the bytes are better spent on the file browser,
 * palette, and diff view.
 *
 * What is given up is colour. What is kept is everything that makes code
 * readable in a transcript: monospace, preserved whitespace, line numbers,
 * horizontal scrolling instead of wrapping, and a copy button. If highlighting
 * becomes the top request, the way in is a dynamic `import()` behind a settings
 * toggle so it never lands in the first-load bundle — that is the change to make,
 * not a static dependency.
 */

import { memo, useState } from "react";
import { useT } from "@/lib/i18n";

/**
 * Lines rendered before truncating.
 *
 * A tool result can be thousands of lines. Rendering them all costs DOM nodes on
 * a page that may hold hundreds of such blocks, and nobody reads line 4000 of a
 * build log inline. The full text stays available through "Show all" and Copy.
 */
const PREVIEW_LINES = 60;

export const CodeBlock = memo(function CodeBlock({
  code,
  language,
  /** Start line, when this block is an excerpt of a larger file. */
  startLine = 1,
  showLineNumbers = true,
  maxLines = PREVIEW_LINES,
}: {
  code: string;
  language?: string;
  startLine?: number;
  showLineNumbers?: boolean;
  maxLines?: number;
}) {
  const [expanded, setExpanded] = useState(false);
  const t = useT();
  const [copied, setCopied] = useState(false);

  const lines = code.replace(/\n$/, "").split("\n");
  const overflowing = lines.length > maxLines;
  const visible = expanded || !overflowing ? lines : lines.slice(0, maxLines);

  async function copy() {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard access is refused in insecure contexts and by permission
      // policy. Leaving the label unchanged is the honest signal.
      setCopied(false);
    }
  }

  return (
    <figure className="group relative overflow-hidden rounded border border-line bg-sunken">
      <div className="flex items-center justify-between gap-2 border-b border-line bg-hover px-2 py-0.5">
        <figcaption className="font-mono text-3xs uppercase tracking-wide text-ink-muted">
          {language || "text"}
          <span className="ml-1.5 normal-case tracking-normal">
            {lines.length} {lines.length === 1 ? "line" : "lines"}
          </span>
        </figcaption>
        <button
          type="button"
          onClick={() => void copy()}
          className="rounded px-1.5 py-0.5 text-3xs text-ink-muted hover:bg-raised hover:text-ink focus-ring"
        >
          {copied ? t("action.copied") : t("action.copy")}
        </button>
      </div>

      <div className="overflow-x-auto">
        <pre className="min-w-full py-1 font-mono text-2xs leading-[1.55] text-ink">
          {visible.map((line, index) => (
            <span key={index} className="flex">
              {showLineNumbers && (
                <span
                  aria-hidden="true"
                  className="sticky left-0 shrink-0 select-none border-r border-line bg-sunken px-2 text-right text-ink-muted"
                  style={{ minWidth: `${String(startLine + lines.length).length + 1.5}ch` }}
                >
                  {startLine + index}
                </span>
              )}
              <span className="px-2 whitespace-pre">{line || " "}</span>
            </span>
          ))}
        </pre>
      </div>

      {overflowing && (
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="w-full border-t border-line bg-hover px-2 py-1 text-2xs font-medium text-ink-muted hover:bg-active focus-ring"
        >
          {expanded
            ? `Show first ${maxLines} lines`
            : `Show all ${lines.length} lines (${lines.length - maxLines} hidden)`}
        </button>
      )}
    </figure>
  );
});

/** Plain preformatted output, for tool results that are not source code. */
export const OutputBlock = memo(function OutputBlock({
  text,
  maxLines = 40,
  tone = "neutral",
}: {
  text: string;
  maxLines?: number;
  tone?: "neutral" | "error";
}) {
  const [expanded, setExpanded] = useState(false);
  const lines = text.replace(/\n$/, "").split("\n");
  const overflowing = lines.length > maxLines;
  const visible = expanded || !overflowing ? lines : lines.slice(0, maxLines);

  return (
    <div
      className={`overflow-hidden rounded border ${
        tone === "error" ? "border-danger-line bg-danger-bg" : "border-line bg-sunken"
      }`}
    >
      <div className="overflow-x-auto">
        <pre
          className={`whitespace-pre px-2 py-1.5 font-mono text-2xs leading-[1.55] ${
            tone === "error" ? "text-danger" : "text-ink-soft"
          }`}
        >
          {visible.join("\n") || " "}
        </pre>
      </div>
      {overflowing && (
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="w-full border-t border-line px-2 py-0.5 text-3xs font-medium text-ink-muted hover:bg-hover focus-ring"
        >
          {expanded ? "Show less" : `Show all ${lines.length} lines`}
        </button>
      )}
    </div>
  );
});
