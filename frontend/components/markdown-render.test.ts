/**
 * Markdown *rendering*, as opposed to parsing.
 *
 * `lib/markdown-parse.test.ts` covers the tokenizer, and it did not catch the
 * bug these tests exist for: rendering a GitHub badge line overflowed the stack,
 * because `renderLink` recursed into an autolink's label and the tokenizer
 * handed back an identical token every time. The parser was correct in isolation
 * — the non-terminating loop only closed once the renderer was in it.
 *
 * Rendered through `react-dom/server` rather than a DOM library: this suite runs
 * in Vitest's `node` environment, and static markup is enough to assert both
 * that recursion terminates and what the output says. No new dependency.
 */

import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Markdown } from "@/components/markdown";
import { parseInlineForTest } from "@/components/markdown";

function render(text: string): string {
  return renderToStaticMarkup(createElement(Markdown, { text }));
}

describe("inline rendering terminates", () => {
  /**
   * The shipped crash, at full fidelity: line 3 of a real README.
   *
   * `[![CI](img)](href)` puts a bare URL where a link label goes, and a bare URL
   * is the one token whose label re-parses to itself. Every emphasis token
   * shrinks — `**x**` yields `x` — so this is the only input that can loop.
   */
  it("renders a badge link without overflowing the stack", () => {
    const badge =
      "[![CI](https://github.com/o/r/actions/workflows/ci.yml/badge.svg)](https://github.com/o/r/actions/workflows/ci.yml)";
    expect(() => render(badge)).not.toThrow();
    expect(render(badge)).toContain("actions/workflows/ci.yml");
  });

  /** The minimal case, in case the badge idiom is ever special-cased away. */
  it("renders a bare URL used as a link label without recursing forever", () => {
    expect(() => render("[https://example.com/a](https://example.com/b)")).not.toThrow();
  });

  it("renders a bare autolink without recursing forever", () => {
    expect(() => render("see https://example.com/a/b for details")).not.toThrow();
  });

  /**
   * The whole README that crashed, in the shapes that matter: badges, a plain
   * image, and prose. A regression here is a blank Artifacts pane.
   */
  it("renders a README of badges and images", () => {
    const readme = [
      "# Dynamic Workflow",
      "",
      "[![CI](https://github.com/o/r/actions/workflows/ci.yml/badge.svg)](https://github.com/o/r/actions/workflows/ci.yml)",
      "[![npm](https://img.shields.io/npm/v/pkg)](https://www.npmjs.com/package/pkg)",
      "[![license](https://img.shields.io/npm/l/pkg)](./LICENSE)",
      "",
      "Prose with **bold**, *italic*, `code`, and a [link](https://example.com).",
      "",
      "![A TUI screenshot](docs/assets/shot.svg)",
    ].join("\n");
    expect(() => render(readme)).not.toThrow();
  });

  /** Deeply nested *valid* emphasis still renders: recursion is bounded by the
   *  input shrinking, not by a depth cap that would truncate this. */
  it("renders deeply nested emphasis rather than capping it", () => {
    const nested = "**" + "*a*".repeat(40) + "**";
    expect(() => render(nested)).not.toThrow();
    expect(render(nested)).toContain("<strong");
  });
});

describe("images", () => {
  /**
   * `![alt](src)` is an image, not a link labelled `![alt`.
   *
   * The link alternative's `[^\]]+` matches straight across the `!`, so ordering
   * the image pattern second produced a mislabelled link and left `](` as stray
   * text on every badge line.
   */
  it("parses an image as an image, not a link with a bracketed label", () => {
    expect(parseInlineForTest("![alt text](docs/a.svg)")).toEqual([
      { type: "image", value: "alt text", href: "docs/a.svg", literal: true },
    ]);
  });

  it("parses the badge idiom as an image inside a link", () => {
    const tokens = parseInlineForTest("[![CI](https://a/b.svg)](https://a/c)");
    expect(tokens).toEqual([
      { type: "link", value: "![CI](https://a/b.svg)", href: "https://a/c" },
    ]);
  });

  /**
   * A relative source cannot resolve: it is relative to the previewed file, and
   * an `<img>` would resolve it against the app's origin and draw a broken
   * glyph. Alt text in brackets says what the author meant instead.
   */
  it("renders a relative image as its alt text rather than a broken img", () => {
    const html = render("![A screenshot](docs/assets/shot.svg)");
    expect(html).not.toContain("<img");
    expect(html).toContain("A screenshot");
  });

  it("offers a remote image as a link, since that source does resolve", () => {
    const html = render("![badge](https://img.shields.io/npm/v/pkg)");
    expect(html).toContain("https://img.shields.io/npm/v/pkg");
    expect(html).not.toContain("<img");
  });

  /**
   * A badge is an image inside a link, and an image with a remote source is
   * offered as a link — so rendering one naively nests `<a>` inside `<a>`. That is
   * invalid HTML: browsers recover by unnesting it, leaving a DOM that does not
   * match the tree React rendered.
   */
  it("does not nest an anchor inside an anchor for a badge", () => {
    const html = render("[![CI](https://a/b.svg)](https://a/c)");
    expect(html.match(/<a\b/g)).toHaveLength(1);
    // The outer link is the one that survives, since it is the destination the
    // author actually pointed at.
    expect(html).toContain('href="https://a/c"');
    expect(html).toContain("CI");
  });

  it("names an image with no alt text instead of rendering an empty label", () => {
    expect(render("![](docs/a.svg)")).toContain("image");
  });

  /** An image source is an `href` too, so it takes the same allowlist. */
  it("refuses a javascript: image source, keeping the alt text", () => {
    const tokens = parseInlineForTest("![x](javascript:alert(1))");
    expect(tokens.some((t) => t.type === "image")).toBe(false);
    expect(render("![x](javascript:alert(1))")).not.toContain("javascript:");
  });
});

describe("autolink labels are literal", () => {
  /**
   * The marker that makes termination structural. Asserted on the token so the
   * invariant is pinned where it is established, not only where it is consumed.
   */
  it("marks an autolink's label as literal", () => {
    expect(parseInlineForTest("https://example.com/a_b_c")).toEqual([
      {
        type: "link",
        value: "https://example.com/a_b_c",
        href: "https://example.com/a_b_c",
        literal: true,
      },
    ]);
  });

  /** A URL with emphasis characters must not be re-parsed into <em> fragments. */
  it("does not italicize underscores inside an autolinked URL", () => {
    const html = render("https://en.wikipedia.org/wiki/Foo_bar_baz");
    expect(html).not.toContain("<em");
    expect(html).toContain("Foo_bar_baz");
  });

  /** An explicit label is still inline content: this must keep working. */
  it("still parses markup inside an explicit link label", () => {
    expect(render("[**bold** label](https://example.com)")).toContain("<strong");
  });
});
