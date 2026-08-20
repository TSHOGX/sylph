"use client";

/**
 * Code display with line numbers, copy, and truncation.
 *
 * # No syntax highlighter
 *
 * Shiki ships a WASM regex engine plus per-language grammars (~200 KB before
 * grammars), and `react-syntax-highlighter` with Prism is ~35 KB gzipped for a
 * useful language set. Phase 4 left about 9.6 KB of first-load budget, so
 * neither fits, and Phase 6 needs those bytes for the file browser, palette, and
 * diff view instead.
 *
 * What is given up is colour. What is kept is everything that makes code
 * readable in a transcript: monospace, preserved whitespace, line numbers,
 * horizontal scrolling instead of wrapping, and a copy button. If highlighting
 * becomes the top request, the way in is a dynamic `import()` behind a settings
 * toggle so it never lands in the first-load bundle — that is the change to make,
 * not a static dependency.
 */

import { memo, useState } from "react";

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
    <figure className="group relative overflow-hidden rounded border border-gray-200 bg-gray-50">
      <div className="flex items-center justify-between gap-2 border-b border-gray-200 bg-gray-100/70 px-2 py-0.5">
        <figcaption className="font-mono text-[10px] uppercase tracking-wide text-gray-500">
          {language || "text"}
          <span className="ml-1.5 normal-case tracking-normal">
            {lines.length} {lines.length === 1 ? "line" : "lines"}
          </span>
        </figcaption>
        <button
          type="button"
          onClick={() => void copy()}
          className="rounded px-1.5 py-0.5 text-[10px] text-gray-500 hover:bg-white hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>

      <div className="overflow-x-auto">
        <pre className="min-w-full py-1 font-mono text-[11.5px] leading-[1.55] text-gray-800">
          {visible.map((line, index) => (
            <span key={index} className="flex">
              {showLineNumbers && (
                <span
                  aria-hidden="true"
                  className="sticky left-0 shrink-0 select-none border-r border-gray-200 bg-gray-50 px-2 text-right text-gray-500"
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
          className="w-full border-t border-gray-200 bg-gray-100/70 px-2 py-1 text-[11px] font-medium text-gray-600 hover:bg-gray-200 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
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
        tone === "error" ? "border-red-200 bg-red-50/60" : "border-gray-200 bg-gray-50"
      }`}
    >
      <div className="overflow-x-auto">
        <pre
          className={`whitespace-pre px-2 py-1.5 font-mono text-[11.5px] leading-[1.55] ${
            tone === "error" ? "text-red-900" : "text-gray-700"
          }`}
        >
          {visible.join("\n") || " "}
        </pre>
      </div>
      {overflowing && (
        <button
          type="button"
          onClick={() => setExpanded(!expanded)}
          className="w-full border-t border-gray-200 px-2 py-0.5 text-[10px] font-medium text-gray-600 hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          {expanded ? "Show less" : `Show all ${lines.length} lines`}
        </button>
      )}
    </div>
  );
});
