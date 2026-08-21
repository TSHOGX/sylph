import { describe, expect, it } from "vitest";
import { parseInlineForTest, parseBlocksForTest } from "@/components/markdown";

/**
 * Markdown parsing, tested through the pure parser rather than the renderer.
 *
 * Two shipped bugs motivate this file, and both are asserted below:
 *
 * 1. A module-level `/g` regex whose `lastIndex` was rewound by the recursive
 *    call that renders the inside of a bold span, so the outer loop re-matched
 *    the same span forever and grew the node list until the tab crashed.
 * 2. `\1` inside a combined alternation resolving to the pattern's *first* group
 *    rather than the alternative's own, which is undefined and matches the empty
 *    string in JavaScript — so `**real**` parsed as a bold "r" followed by "eal".
 *
 * Both took a crashed browser to find and are one assertion each here.
 */

describe("inline parsing", () => {
  it("parses bold with both delimiters, keeping the whole word", () => {
    expect(parseInlineForTest("with **real** logging")).toEqual([
      { type: "text", value: "with " },
      { type: "bold", value: "real" },
      { type: "text", value: " logging" },
    ]);
    expect(parseInlineForTest("__also bold__")).toEqual([{ type: "bold", value: "also bold" }]);
  });

  it("parses italic without swallowing bold", () => {
    expect(parseInlineForTest("*em*")).toEqual([{ type: "italic", value: "em" }]);
    // The regression: bold must not be consumed as `*` + `*x*` + `*`.
    expect(parseInlineForTest("**b**")).toEqual([{ type: "bold", value: "b" }]);
    // A spaced `*` pair still reads as emphasis here; that is a deliberate
    // simplification, and pinning it stops the behaviour drifting silently.
    expect(parseInlineForTest("a * b * c")).toEqual([
      { type: "text", value: "a " },
      { type: "italic", value: " b " },
      { type: "text", value: " c" },
    ]);
  });

  it("terminates on nested and adjacent emphasis", () => {
    // The infinite-loop regression: any recursion into emphasis content must not
    // rewind the outer scan. A parse that returns at all proves it terminates.
    expect(parseInlineForTest("**a _b_ c**")).toEqual([
      { type: "bold", value: "a _b_ c" },
    ]);
    expect(parseInlineForTest("**one** and **two**")).toEqual([
      { type: "bold", value: "one" },
      { type: "text", value: " and " },
      { type: "bold", value: "two" },
    ]);
  });

  it("does not re-scan inline code", () => {
    // What lets documentation about Markdown show its own syntax.
    expect(parseInlineForTest("`**not bold**`")).toEqual([
      { type: "code", value: "**not bold**" },
    ]);
  });

  it("terminates on an empty inline-code fence", () => {
    // ``​`` matches with empty content, which without a zero-length guard would
    // leave `lastIndex` parked and spin.
    expect(() => parseInlineForTest("a `` b")).not.toThrow();
    expect(parseInlineForTest("a `` b").length).toBeGreaterThan(0);
  });

  it("parses links and autolinks", () => {
    expect(parseInlineForTest("see [docs](https://example.com/x)")).toEqual([
      { type: "text", value: "see " },
      { type: "link", value: "docs", href: "https://example.com/x" },
    ]);
    // `literal` marks a label that must not be re-parsed. An autolink's label is
    // the URL itself, so re-tokenizing it yields this same token — the renderer
    // then recursed forever. `components/markdown-render.test.ts` covers the
    // crash; this pins the flag at the point it is set.
    expect(parseInlineForTest("https://example.com/y")).toEqual([
      {
        type: "link",
        value: "https://example.com/y",
        href: "https://example.com/y",
        literal: true,
      },
    ]);
  });

  it("refuses a javascript: link, keeping its text", () => {
    // The whole XSS surface of this renderer: nothing else can inject markup, and
    // a React-rendered `href="javascript:..."` still executes on click.
    // The property that matters is that no `link` token is produced, so no
    // `href` is ever emitted. Exact token counts are not asserted: the href
    // pattern stops at the first `)`, so `alert(1)` also leaves a stray `)` as
    // text — harmless, and not worth complicating the pattern for.
    for (const hostile of [
      "[click](javascript:alert(1))",
      "[x](data:text/html;base64,PHNjcmlwdD4=)",
      "[y](JaVaScRiPt:alert(1))",
      "[z](  javascript:alert(1))",
      "[w](vbscript:msgbox)",
    ]) {
      const tokens = parseInlineForTest(hostile);
      expect(tokens.some((token) => token.type === "link"), hostile).toBe(false);
      expect(tokens.every((token) => token.href === undefined), hostile).toBe(true);
    }
    // Relative, anchor, and mailto links stay usable.
    expect(parseInlineForTest("[a](/local)")[0]).toMatchObject({ type: "link" });
    expect(parseInlineForTest("[b](#frag)")[0]).toMatchObject({ type: "link" });
    expect(parseInlineForTest("[c](mailto:x@y.z)")[0]).toMatchObject({ type: "link" });
  });

  it("parses strikethrough", () => {
    expect(parseInlineForTest("~~gone~~")).toEqual([{ type: "strike", value: "gone" }]);
  });

  it("leaves unmatched delimiters as literal text", () => {
    expect(parseInlineForTest("2 * 3 = 6")).toEqual([{ type: "text", value: "2 * 3 = 6" }]);
    // Intra-word underscores do emphasize here, unlike GFM. Documented rather
    // than fixed: agent prose rarely contains snake_case outside inline code,
    // and the fix needs word-boundary tracking this parser does not do.
    expect(parseInlineForTest("a_b_c")).toEqual([
      { type: "text", value: "a" },
      { type: "italic", value: "b" },
      { type: "text", value: "c" },
    ]);
  });

  it("finishes promptly on pathological emphasis input", () => {
    // Guards against catastrophic backtracking as well as the loop bug: a wall of
    // delimiters must not take measurable time.
    const nasty = "*".repeat(400) + "text" + "_".repeat(400);
    const started = Date.now();
    parseInlineForTest(nasty);
    expect(Date.now() - started).toBeLessThan(300);
  });
});

describe("block parsing", () => {
  it("parses fenced code and does not treat its contents as markup", () => {
    const blocks = parseBlocksForTest("before\n\n```rust\n# not a heading\n| not | a table |\n```\n");
    expect(blocks[0]).toMatchObject({ type: "paragraph" });
    expect(blocks[1]).toMatchObject({
      type: "code",
      language: "rust",
      content: "# not a heading\n| not | a table |",
    });
  });

  it("closes a fence only on a marker at least as long as the opener", () => {
    const blocks = parseBlocksForTest("````\n```\ninner\n```\n````\n");
    expect(blocks).toHaveLength(1);
    expect(blocks[0]).toMatchObject({ type: "code" });
    expect((blocks[0] as { content: string }).content).toContain("inner");
  });

  it("parses headings, lists, quotes, rules, and tables", () => {
    const blocks = parseBlocksForTest(
      [
        "## Heading",
        "",
        "- one",
        "- two",
        "",
        "1. first",
        "2. second",
        "",
        "> quoted",
        "",
        "---",
        "",
        "| a | b |",
        "| --- | --- |",
        "| 1 | 2 |",
      ].join("\n"),
    );
    const types = blocks.map((block) => block.type);
    expect(types).toEqual([
      "heading",
      "list",
      "list",
      "quote",
      "rule",
      "table",
    ]);
    expect(blocks[0]).toMatchObject({ level: 2, content: "Heading" });
    expect(blocks[1]).toMatchObject({ ordered: false, items: ["one", "two"] });
    expect(blocks[2]).toMatchObject({ ordered: true, items: ["first", "second"] });
    expect(blocks[5]).toMatchObject({ header: ["a", "b"], rows: [["1", "2"]] });
  });

  it("attaches a wrapped continuation line to its list item", () => {
    const blocks = parseBlocksForTest("- first item\n  continues here\n- second");
    expect(blocks[0]).toMatchObject({
      items: ["first item continues here", "second"],
    });
  });

  it("does not run a paragraph into a following block", () => {
    const blocks = parseBlocksForTest("text\n## Heading");
    expect(blocks.map((block) => block.type)).toEqual(["paragraph", "heading"]);
  });

  it("terminates on input with no trailing newline and on empty input", () => {
    expect(parseBlocksForTest("")).toEqual([]);
    expect(parseBlocksForTest("no newline").length).toBe(1);
    // An unclosed fence must consume to EOF rather than loop.
    expect(parseBlocksForTest("```\nunclosed").length).toBe(1);
  });
});
