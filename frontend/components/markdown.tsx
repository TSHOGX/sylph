"use client";

/**
 * Markdown rendering for assistant text and file previews.
 *
 * # Why not react-markdown
 *
 * `react-markdown` + `remark-gfm` is ~45 KB gzipped, and Phase 4 measured
 * first-load JS at 190.4 KB against a 200 KB budget. Adding it would blow the
 * budget on its own, before the command palette, file browser, and diff view
 * that Phase 6 also needs.
 *
 * What agent output actually uses is a small subset: fenced code, inline code,
 * headings, lists, bold/italic, links, blockquotes, and tables. That is what
 * this renders. It is a block-then-inline parser, not a CommonMark
 * implementation — reference links, HTML blocks, setext headings, and nested
 * block quotes are out of scope, and unsupported syntax degrades to the literal
 * text rather than to a parse error.
 *
 * # Safety
 *
 * No `dangerouslySetInnerHTML` anywhere. Every node is a React element, so
 * agent-authored text cannot inject markup — which is why this needs no
 * sanitizer, and why `rehype-sanitize` (another 20 KB) is not in the budget
 * either. Link `href`s are still scheme-checked, because a React-rendered
 * `<a href="javascript:...">` is a live XSS vector even without raw HTML.
 */

import { Fragment, memo, type ReactNode } from "react";
import { CodeBlock } from "@/components/code-block";

export const Markdown = memo(function Markdown({
  text,
  className = "",
}: {
  text: string;
  /** Applied to the wrapper, for density and colour context. */
  className?: string;
}) {
  return <div className={`space-y-2 ${className}`}>{renderBlocks(text)}</div>;
});

type Block =
  | { type: "code"; language: string; content: string }
  | { type: "heading"; level: number; content: string }
  | { type: "list"; ordered: boolean; items: string[] }
  | { type: "quote"; content: string }
  | { type: "table"; header: string[]; rows: string[][] }
  | { type: "rule" }
  | { type: "paragraph"; content: string };

/**
 * Split into blocks, then render each.
 *
 * Fenced code is consumed first and never re-scanned, so `#` or `|` inside a
 * code block is not mistaken for a heading or a table row.
 */
function parseBlocks(text: string): Block[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];

    // Fenced code. The closing fence must be at least as long as the opening
    // one, which is how CommonMark allows fences inside fences.
    const fence = /^(\s*)(`{3,}|~{3,})\s*([\w+-]*)\s*$/.exec(line);
    if (fence) {
      const [, , marker, language] = fence;
      const body: string[] = [];
      index += 1;
      while (index < lines.length) {
        const closing = new RegExp(`^\\s*${marker[0]}{${marker.length},}\\s*$`);
        if (closing.test(lines[index])) {
          index += 1;
          break;
        }
        body.push(lines[index]);
        index += 1;
      }
      blocks.push({ type: "code", language: language || "", content: body.join("\n") });
      continue;
    }

    if (!line.trim()) {
      index += 1;
      continue;
    }

    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      blocks.push({ type: "heading", level: heading[1].length, content: heading[2].trim() });
      index += 1;
      continue;
    }

    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      blocks.push({ type: "rule" });
      index += 1;
      continue;
    }

    // Table: a header row followed by a delimiter row of dashes and pipes.
    if (line.includes("|") && index + 1 < lines.length && isDelimiterRow(lines[index + 1])) {
      const header = splitRow(line);
      const rows: string[][] = [];
      index += 2;
      while (index < lines.length && lines[index].includes("|") && lines[index].trim()) {
        rows.push(splitRow(lines[index]));
        index += 1;
      }
      blocks.push({ type: "table", header, rows });
      continue;
    }

    const bullet = /^\s*([-*+]|\d+[.)])\s+/.exec(line);
    if (bullet) {
      const ordered = /\d/.test(bullet[1]);
      const items: string[] = [];
      while (index < lines.length) {
        const match = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(lines[index]);
        if (match && /\d/.test(match[1]) === ordered) {
          items.push(match[2]);
          index += 1;
        } else if (lines[index].trim() && /^\s{2,}/.test(lines[index]) && items.length > 0) {
          // A continuation line belongs to the previous item, which is how
          // wrapped list text arrives from an agent.
          items[items.length - 1] += ` ${lines[index].trim()}`;
          index += 1;
        } else {
          break;
        }
      }
      blocks.push({ type: "list", ordered, items });
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const body: string[] = [];
      while (index < lines.length && /^\s*>\s?/.test(lines[index])) {
        body.push(lines[index].replace(/^\s*>\s?/, ""));
        index += 1;
      }
      blocks.push({ type: "quote", content: body.join("\n") });
      continue;
    }

    // Paragraph: consume until a blank line or the start of another block.
    const paragraph: string[] = [];
    while (index < lines.length && lines[index].trim() && !startsBlock(lines[index])) {
      paragraph.push(lines[index]);
      index += 1;
    }
    // A line that starts a block while a paragraph is open must not be swallowed;
    // the loop above stops before it, so no `index` advance is needed here.
    if (paragraph.length === 0) {
      paragraph.push(lines[index]);
      index += 1;
    }
    blocks.push({ type: "paragraph", content: paragraph.join("\n") });
  }

  return blocks;
}

function startsBlock(line: string): boolean {
  return (
    /^(\s*)(`{3,}|~{3,})/.test(line) ||
    /^#{1,6}\s/.test(line) ||
    /^\s*([-*+]|\d+[.)])\s/.test(line) ||
    /^\s*>\s?/.test(line) ||
    /^\s*([-*_])(\s*\1){2,}\s*$/.test(line)
  );
}

function isDelimiterRow(line: string): boolean {
  return /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line);
}

function splitRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\||\|$/g, "")
    .split("|")
    .map((cell) => cell.trim());
}

function renderBlocks(text: string): ReactNode {
  return parseBlocks(text).map((block, index) => {
    switch (block.type) {
      case "code":
        return <CodeBlock key={index} code={block.content} language={block.language} />;
      case "heading": {
        // Headings inside agent prose start at h4: the page already owns h1-h3,
        // and emitting an h1 mid-transcript breaks the document outline that
        // screen-reader navigation depends on.
        const level = Math.min(block.level + 3, 6);
        const Tag = `h${level}` as "h4" | "h5" | "h6";
        const size = block.level <= 1 ? "text-base" : block.level === 2 ? "text-sm" : "text-sm";
        return (
          <Tag key={index} className={`${size} font-semibold text-gray-900`}>
            {renderInline(block.content)}
          </Tag>
        );
      }
      case "list":
        return block.ordered ? (
          <ol key={index} className="list-decimal space-y-0.5 pl-5">
            {block.items.map((item, at) => (
              <li key={at}>{renderInline(item)}</li>
            ))}
          </ol>
        ) : (
          <ul key={index} className="list-disc space-y-0.5 pl-5">
            {block.items.map((item, at) => (
              <li key={at}>{renderInline(item)}</li>
            ))}
          </ul>
        );
      case "quote":
        return (
          <blockquote
            key={index}
            className="border-l-2 border-gray-300 pl-3 text-gray-600 italic"
          >
            {renderInline(block.content)}
          </blockquote>
        );
      case "table":
        return (
          <div key={index} className="overflow-x-auto">
            <table className="w-full border-collapse text-left text-xs">
              <thead>
                <tr>
                  {block.header.map((cell, at) => (
                    <th key={at} className="border border-gray-200 bg-gray-50 px-2 py-1 font-semibold">
                      {renderInline(cell)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {block.rows.map((row, rowAt) => (
                  <tr key={rowAt}>
                    {row.map((cell, cellAt) => (
                      <td key={cellAt} className="border border-gray-200 px-2 py-1 align-top">
                        {renderInline(cell)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
      case "rule":
        return <hr key={index} className="border-gray-200" />;
      default:
        return (
          <p key={index} className="whitespace-pre-wrap break-words">
            {renderInline(block.content)}
          </p>
        );
    }
  });
}

/**
 * Inline spans: code, bold, italic, strikethrough, links, autolinks.
 *
 * Inline code is matched first and its content is never re-scanned, so
 * `` `**not bold**` `` renders literally — the rule that makes documentation
 * about Markdown readable.
 */
/**
 * Inline constructs, as one alternation with named groups.
 *
 * Named groups, not numbered ones, and each delimiter spelled out on both sides
 * rather than closed with a backreference. Backreferences in a combined
 * alternation are numbered across the *whole* pattern, not within the alternative
 * that contains them: `(\*\*|__)(.+?)\1` inside a larger alternation resolves
 * `\1` to the first group of the pattern — the inline-code fence — which is
 * undefined at that point and, in JavaScript, matches the empty string. The
 * result was `**real**` rendering as a bold "r" followed by plain "eal".
 *
 * Writing `\*\*(...)\*\*` and `__(...)__` as separate branches removes the
 * backreference entirely, so each delimiter is matched literally.
 */
const INLINE_SOURCE = [
  "(?<codeFence>`+)(?<code>[\\s\\S]*?)\\k<codeFence>", // inline code
  "\\[(?<linkText>[^\\]]+)\\]\\((?<linkHref>[^)\\s]+)(?:\\s+\"[^\"]*\")?\\)",
  "\\*\\*(?<boldStar>[\\s\\S]+?)\\*\\*",
  "__(?<boldUnderscore>[\\s\\S]+?)__",
  "~~(?<strike>[\\s\\S]+?)~~",
  // Italic runs after bold, and excludes its own delimiter from the content, so
  // `**x**` can never be consumed here as `*` + `*x*` + `*`.
  "\\*(?<italicStar>[^*\\n]+?)\\*",
  "_(?<italicUnderscore>[^_\\n]+?)_",
  "(?<autolink>https?://[^\\s<>()]+)",
].join("|");

/** One inline span. `href` is set only for links. */
export interface InlineToken {
  type: "text" | "code" | "bold" | "italic" | "strike" | "link";
  value: string;
  href?: string;
}

/**
 * Tokenize one line's inline markup.
 *
 * Separated from rendering so it can be tested as a pure function. The parsing
 * here has had two subtle bugs that only showed up as a crashed tab or as visibly
 * wrong output, and neither is economical to catch through the DOM.
 */
export function parseInline(text: string): InlineToken[] {
  const tokens: InlineToken[] = [];
  let cursor = 0;

  /**
   * A fresh regex per call, not a shared module-level one.
   *
   * `lastIndex` is mutable state on the RegExp object, and emphasis content is
   * parsed by recursion. With one shared instance the inner call rewinds
   * `lastIndex` to 0, so the outer loop re-matches the span it just consumed and
   * never terminates — it appends tokens until memory runs out. Constructing per
   * call costs a compile that V8 caches by source string, which is nothing next
   * to that failure.
   */
  const pattern = new RegExp(INLINE_SOURCE, "g");
  let match: RegExpExecArray | null;

  while ((match = pattern.exec(text)) !== null) {
    if (match.index > cursor) {
      tokens.push({ type: "text", value: text.slice(cursor, match.index) });
    }

    const groups = match.groups ?? {};
    const bold = groups.boldStar ?? groups.boldUnderscore;
    const italic = groups.italicStar ?? groups.italicUnderscore;

    if (groups.code !== undefined) {
      // Code content is never re-scanned, which is the rule that lets
      // documentation about Markdown show its own syntax literally.
      tokens.push({ type: "code", value: groups.code });
    } else if (groups.linkText !== undefined) {
      const href = groups.linkHref ?? "";
      // An unsafe scheme degrades to plain text, so the label survives but the
      // link does not. See `isSafeHref`.
      tokens.push(
        isSafeHref(href)
          ? { type: "link", value: groups.linkText, href }
          : { type: "text", value: groups.linkText },
      );
    } else if (bold !== undefined) {
      tokens.push({ type: "bold", value: bold });
    } else if (groups.strike !== undefined) {
      tokens.push({ type: "strike", value: groups.strike });
    } else if (italic !== undefined) {
      tokens.push({ type: "italic", value: italic });
    } else if (groups.autolink !== undefined) {
      tokens.push({ type: "link", value: groups.autolink, href: groups.autolink });
    }

    cursor = match.index + match[0].length;
    // A zero-length match would leave `lastIndex` parked and spin forever. The
    // inline-code branch can match empty content (`` `` ``), so this is not
    // hypothetical.
    if (match[0].length === 0) pattern.lastIndex += 1;
  }

  if (cursor < text.length) tokens.push({ type: "text", value: text.slice(cursor) });
  return tokens;
}

/**
 * Whether a link target is safe to render as an `href`.
 *
 * `javascript:` and `data:` URLs execute or render attacker-controlled content on
 * click. Nothing else in this component can inject markup, so this allowlist is
 * the entire XSS surface of Markdown rendering — which is why it is an allowlist
 * and not a blocklist.
 */
function isSafeHref(href: string): boolean {
  return /^(https?:|mailto:|#|\/)/i.test(href.trim());
}

function renderInline(text: string): ReactNode {
  return (
    <Fragment>
      {parseInline(text).map((token, index) => {
        switch (token.type) {
          case "code":
            return (
              <code
                key={index}
                className="rounded bg-gray-100 px-1 py-px font-mono text-[0.9em] text-gray-800"
              >
                {token.value}
              </code>
            );
          case "link":
            return renderLink(token.value, token.href ?? "", index);
          case "bold":
            return (
              <strong key={index} className="font-semibold">
                {renderInline(token.value)}
              </strong>
            );
          case "italic":
            return <em key={index}>{renderInline(token.value)}</em>;
          case "strike":
            return (
              <del key={index} className="text-gray-500">
                {renderInline(token.value)}
              </del>
            );
          default:
            return token.value;
        }
      })}
    </Fragment>
  );
}

/** Test seams. Exported so the parser can be exercised without a DOM. */
export const parseInlineForTest = parseInline;
export const parseBlocksForTest = parseBlocks;

/**
 * Render a link. The scheme was already checked in `parseInline`, and is checked
 * again here because this is the function that produces the `href` attribute —
 * a future caller reaching it by another path must not bypass the allowlist.
 */
function renderLink(label: string, href: string, key: number): ReactNode {
  if (!isSafeHref(href)) {
    return <span key={key}>{label}</span>;
  }
  const external = /^https?:/i.test(href);
  return (
    <a
      key={key}
      href={href}
      // `noreferrer` implies `noopener`, but both are named because the pairing
      // is what prevents reverse tabnabbing on older engines.
      {...(external ? { target: "_blank", rel: "noopener noreferrer" } : {})}
      className="text-blue-700 underline decoration-blue-300 hover:decoration-blue-700"
    >
      {renderInline(label)}
    </a>
  );
}
