import { describe, expect, it } from "vitest";
import { buildSandboxDocument, describeBlocked } from "@/lib/sandbox-html";
import { translate, type Translate } from "@/lib/i18n/locale";

/** Where the policy landed, as an index, so ordering can be asserted. */
function policyAt(html: string): number {
  return html.indexOf("Content-Security-Policy");
}

describe("buildSandboxDocument: policy placement", () => {
  it("puts the policy inside head, before anything that could fetch", () => {
    const { html } = buildSandboxDocument(
      `<!doctype html><html><head><title>t</title></head><body><img src="x.png"></body></html>`,
    );
    expect(policyAt(html)).toBeGreaterThan(-1);
    // Before the title, so it precedes every fetchable element in the head.
    expect(policyAt(html)).toBeLessThan(html.indexOf("<title>"));
    expect(policyAt(html)).toBeLessThan(html.indexOf("<img"));
  });

  it("falls back to just inside html when there is no head", () => {
    const { html } = buildSandboxDocument(`<html><body><p>hi</p></body></html>`);
    expect(policyAt(html)).toBeGreaterThan(html.indexOf("<html>"));
    expect(policyAt(html)).toBeLessThan(html.indexOf("<body>"));
  });

  /** A stray element ahead of the doctype triggers quirks mode, which changes the
   *  box model and visibly breaks layouts that were otherwise fine. */
  it("never precedes the doctype", () => {
    const { html } = buildSandboxDocument(`<!doctype html><body><p>hi</p></body>`);
    expect(html.toLowerCase().indexOf("<!doctype")).toBe(0);
    expect(policyAt(html)).toBeGreaterThan(0);
  });

  it("keeps the doctype first even with leading whitespace", () => {
    const { html } = buildSandboxDocument(`\n  <!DOCTYPE html>\n<body>x</body>`);
    expect(policyAt(html)).toBeGreaterThan(html.toLowerCase().indexOf("<!doctype"));
  });

  it("handles a bare fragment, which generated snippets often are", () => {
    const { html } = buildSandboxDocument(`<p>just a fragment</p>`);
    expect(policyAt(html)).toBeLessThan(html.indexOf("<p>"));
  });

  it("handles head with attributes", () => {
    const { html } = buildSandboxDocument(
      `<html><head lang="en" data-x="1"><meta charset="utf-8"></head><body>x</body></html>`,
    );
    expect(policyAt(html)).toBeGreaterThan(-1);
    expect(policyAt(html)).toBeLessThan(html.indexOf("<meta charset"));
  });

  it("blocks network by default and permits only inline style and data images", () => {
    const { html } = buildSandboxDocument("<p>x</p>");
    expect(html).toContain("default-src 'none'");
    expect(html).toContain("style-src 'unsafe-inline'");
    expect(html).toContain("img-src data:");
    // Scripts are absent from the policy, so default-src denies them. The
    // sandbox already does, and neither layer relies on the other.
    expect(html).not.toContain("script-src");
    expect(html).not.toContain("'unsafe-eval'");
  });

  it("leaves the original markup intact", () => {
    // Deliberately no sanitizing: the sandbox removes script execution wholesale,
    // and a hand-written sanitizer would be a permanent source of bypasses.
    const raw = `<p>kept &amp; intact <b>bold</b></p><script>ignored()</script>`;
    const { html } = buildSandboxDocument(raw);
    expect(html).toContain("kept &amp; intact <b>bold</b>");
    expect(html).toContain("<script>ignored()</script>");
  });
});

describe("buildSandboxDocument: reference counting", () => {
  it("counts external references, which will not load", () => {
    const { blockedExternal, blockedRelative } = buildSandboxDocument(
      `<img src="https://tracker/x.gif"><link href="//cdn/app.css"><script src="http://e/x.js"></script>`,
    );
    expect(blockedExternal).toBe(3);
    expect(blockedRelative).toBe(0);
  });

  it("counts workspace-relative references separately", () => {
    // These break structurally: srcdoc inherits the parent's base URL, so
    // `./chart.png` resolves against the app origin where nothing answers.
    const { blockedRelative, blockedExternal } = buildSandboxDocument(
      `<img src="./chart.png"><img src="assets/logo.svg"><link href="../style.css">`,
    );
    expect(blockedRelative).toBe(3);
    expect(blockedExternal).toBe(0);
  });

  it("ignores references that need no fetch", () => {
    const { blockedExternal, blockedRelative } = buildSandboxDocument(
      `<img src="data:image/png;base64,AAA">
       <a href="#section">jump</a>
       <a href="mailto:x@example.com">mail</a>
       <a href="tel:+100">call</a>
       <img src="">`,
    );
    expect(blockedExternal).toBe(0);
    expect(blockedRelative).toBe(0);
  });

  it("reads single-quoted and spaced attributes", () => {
    const { blockedExternal } = buildSandboxDocument(
      `<img src = 'https://a/b.png'><img SRC="https://c/d.png">`,
    );
    expect(blockedExternal).toBe(2);
  });
});

/**
 * The English translator, so these assertions stay readable strings.
 *
 * `describeWindow` and `describeBlocked` take `t` because the plural rules they
 * apply belong in the catalogue rather than in this module. Binding English here
 * keeps the test asserting on the sentence a reader sees, and it also checks the
 * keys resolve — a renamed key fails these tests, not only the UI.
 */
const t: Translate = (key, params) => translate("en", key, params);

describe("describeBlocked", () => {
  it("says nothing for a self-contained page", () => {
    // The common case for generated reports: inline CSS, inline SVG, no banner.
    expect(describeBlocked(buildSandboxDocument("<p>inline only</p>"), t)).toBeNull();
  });

  it("names both causes when both are present", () => {
    const message = describeBlocked(
      buildSandboxDocument(`<img src="./a.png"><img src="https://b/c.png">`),
      t,
    );
    expect(message).toBe(
      "Not loaded in this preview: 1 workspace file and 1 external resource.",
    );
  });

  it("pluralizes on count", () => {
    const message = describeBlocked(
      buildSandboxDocument(`<img src="./a.png"><img src="./b.png">`),
      t,
    );
    expect(message).toBe("Not loaded in this preview: 2 workspace files.");
  });
});
