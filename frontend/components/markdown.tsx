"use client";

/**
 * Markdown rendering for assistant text and file previews.
 *
 * # Why not react-markdown
 *
 * `react-markdown` + `remark-gfm` is ~45 KB gzipped, which is a substantial fraction of
 * the whole first-load budget on its own — and the budget exists to protect first paint on
 * a phone over a slow link (`docs/frontend-architecture.md` § Bundle size management).
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
          <Tag key={index} className={`${size} font-semibold text-ink`}>
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
            className="border-l-2 border-line-strong pl-3 text-ink-muted italic"
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
                    <th key={at} className="border border-line bg-sunken px-2 py-1 font-semibold">
                      {renderInline(cell)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {block.rows.map((row, rowAt) => (
                  <tr key={rowAt}>
                    {row.map((cell, cellAt) => (
                      <td key={cellAt} className="border border-line px-2 py-1 align-top">
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
        return <hr key={index} className="border-line" />;
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
  // Images before links so `![alt](src)` reads as an image. Alternation order is
  // not the whole story — JavaScript takes the *leftmost* match, so an earlier
  // alternative only wins among those matching at the same index — but it is
  // what settles `!` where both could start.
  "!\\[(?<imageAlt>[^\\]]*)\\]\\((?<imageSrc>[^)\\s]+)(?:\\s+\"[^\"]*\")?\\)",
  // A link label admits one level of nested brackets, which is what makes the
  // badge idiom `[![alt](img)](href)` parse. Under a flat `[^\]]+` the label
  // could not span the nested `]`, so the line matched as a link labelled
  // `![alt` carrying the *image's* URL, and the real href was left behind to be
  // picked up as an autolink — the bare-URL-in-a-label shape that then overflowed
  // the stack. One level is enough for every idiom this renderer targets;
  // arbitrary nesting needs a real parser, not a regex.
  "\\[(?<linkText>(?:[^\\[\\]]|!?\\[[^\\]]*\\])+)\\]\\((?<linkHref>[^)\\s]+)(?:\\s+\"[^\"]*\")?\\)",
  "\\*\\*(?<boldStar>[\\s\\S]+?)\\*\\*",
  "__(?<boldUnderscore>[\\s\\S]+?)__",
  "~~(?<strike>[\\s\\S]+?)~~",
  // Italic runs after bold, and excludes its own delimiter from the content, so
  // `**x**` can never be consumed here as `*` + `*x*` + `*`.
  "\\*(?<italicStar>[^*\\n]+?)\\*",
  "_(?<italicUnderscore>[^_\\n]+?)_",
  "(?<autolink>https?://[^\\s<>()]+)",
].join("|");

/** One inline span. `href` is set only for links and images. */
export interface InlineToken {
  type: "text" | "code" | "bold" | "italic" | "strike" | "link" | "image";
  value: string;
  href?: string;
  /**
   * The label is literal text, not inline markup, and must not be re-parsed.
   *
   * Set on autolinks, where `value` *is* the matched URL. Recursion through
   * inline content terminates because every nested call gets a strictly shorter
   * string — `**x**` yields `x`, four characters shorter. An autolink is the one
   * token that breaks that: re-tokenizing its label reproduces the same token
   * with the same value, so `renderLink` -> `renderInline` -> `renderLink` never
   * shrinks and overflows the stack. A GitHub README of badge links is enough to
   * do it, which is how this shipped: it needs a bare URL in a label, and only
   * autolinks make one.
   *
   * Recorded on the token rather than re-derived in the renderer, because the
   * tokenizer is the only place that knows the label came from the URL itself.
   */
  literal?: boolean;
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
    } else if (groups.imageAlt !== undefined) {
      const src = groups.imageSrc ?? "";
      // Alt text is literal by definition, so it is never re-parsed. A refused
      // source degrades to the alt text, which is exactly what alt text is for.
      tokens.push(
        isDescribableImageSource(src)
          ? { type: "image", value: groups.imageAlt, href: src, literal: true }
          : { type: "text", value: groups.imageAlt },
      );
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
      // `literal`: the label is the URL, so re-parsing it would reproduce this
      // very token and recurse forever. See `InlineToken.literal`.
      tokens.push({
        type: "link",
        value: groups.autolink,
        href: groups.autolink,
        literal: true,
      });
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

/**
 * Whether an image source is safe to *describe*.
 *
 * Deliberately not `isSafeHref`. That predicate guards the `href` attribute, and
 * requires a leading `/` — so it rejects `docs/a.svg`, the ordinary way a README
 * refers to a picture beside it. An image here never becomes an `src` attribute
 * (see `renderImage`), so the question is not "may the browser fetch this" but
 * "may this string be shown to the reader".
 *
 * The answer is still no for `javascript:` and `data:`. Neither can execute from
 * the text of a `title`, but an image is offered as a *link* when its source is
 * remote, and a scheme that reaches `renderImage` unchecked would be one refactor
 * away from reaching an `href`. Rejecting every scheme except http(s) leaves
 * relative paths — which have no scheme at all — as the only other thing that
 * passes.
 */
function isDescribableImageSource(src: string): boolean {
  const trimmed = src.trim();
  if (/^https?:/i.test(trimmed)) return true;
  // Anything with a scheme other than http(s) is refused. A bare `foo:bar` is a
  // valid relative path on disk but indistinguishable from a scheme here, and
  // refusing it costs an alt-text fallback rather than an unchecked URL.
  return !/^[a-z][a-z0-9+.-]*:/i.test(trimmed);
}

/**
 * Render inline markup.
 *
 * `inLink` is threaded through so a nested image does not emit an `<a>` inside an
 * `<a>`. The badge idiom `[![alt](img)](href)` puts an image inside a link, and
 * `renderImage` offers a remote source as a link of its own — valid alone, but
 * nested anchors are invalid HTML that browsers silently unnest, producing markup
 * that does not match the tree React thinks it rendered.
 */
function renderInline(text: string, inLink = false): ReactNode {
  return (
    <Fragment>
      {parseInline(text).map((token, index) => {
        switch (token.type) {
          case "code":
            return (
              <code
                key={index}
                className="rounded bg-hover px-1 py-px font-mono text-[0.9em] text-ink"
              >
                {token.value}
              </code>
            );
          case "link":
            return renderLink(token.value, token.href ?? "", index, token.literal);
          case "image":
            return renderImage(token.value, token.href ?? "", index, inLink);
          case "bold":
            return (
              <strong key={index} className="font-semibold">
                {renderInline(token.value, inLink)}
              </strong>
            );
          case "italic":
            return <em key={index}>{renderInline(token.value, inLink)}</em>;
          case "strike":
            return (
              <del key={index} className="text-ink-muted">
                {renderInline(token.value, inLink)}
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
 *
 * `literal` labels are rendered as text instead of being re-parsed. That is a
 * termination requirement, not a preference: see `InlineToken.literal`. Rather
 * than defend it with a recursion-depth cap, the label of an autolink is simply
 * not inline content — a cap would silently truncate deep-but-valid nesting and
 * still leave the non-decreasing recursion in place.
 */
function renderLink(
  label: string,
  href: string,
  key: number,
  literal = false,
): ReactNode {
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
      className="text-info underline decoration-link/40 hover:decoration-link"
    >
      {literal ? label : renderInline(label, true)}
    </a>
  );
}

/**
 * Render an image as its alt text, not as an `<img>`.
 *
 * A Markdown image source is relative to the *file being previewed*, and this
 * renderer has no idea where that is — it receives a string, not a path. There
 * is also no endpoint that serves workspace bytes to an `<img>`: `files/content`
 * returns authenticated JSON, deliberately, so that no unauthenticated route to
 * the filesystem exists (see `file-preview.tsx`). Emitting `<img src="docs/a.svg">`
 * would therefore resolve against the *app's* origin and load nothing, drawing a
 * broken-image glyph where the document meant to show a picture.
 *
 * Alt text in brackets is honest about what it is: the description the author
 * wrote, marked as standing in for an image. A remote `https:` source is still
 * offered as a link, since that one does resolve.
 */
function renderImage(
  alt: string,
  src: string,
  key: number,
  inLink = false,
): ReactNode {
  const label = alt.trim() === "" ? "image" : alt;
  // Inside a link the enclosing anchor already carries a destination, and nesting
  // another would be invalid HTML.
  if (!inLink && /^https?:/i.test(src)) {
    return (
      <a
        key={key}
        href={src}
        target="_blank"
        rel="noopener noreferrer"
        title={`Image: ${src}`}
        className="text-info underline decoration-dotted decoration-link/40 hover:decoration-link"
      >
        [{label}]
      </a>
    );
  }
  return (
    <span key={key} title={`Image: ${src}`} className="text-ink-muted">
      [{label}]
    </span>
  );
}
