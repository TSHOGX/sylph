/**
 * Render every icon to a contact sheet, light and dark, for visual inspection.
 *
 * Run:  node scripts/icon-sheet.mjs  → writes /tmp/icon-sheet-{light,dark}.png
 *
 * Hand-authored SVG paths cannot be verified by a type checker or a unit test: a
 * transposed coordinate compiles, renders, and is simply wrong. Nor by E2E, which
 * asserts that an icon is *present*, not that it depicts anything. The only real
 * check is looking at them, and looking at 40 icons one screen at a time is how
 * three of them stayed subtly broken for a while.
 *
 * Not part of any suite: it produces an image, and the judgement is a human's. It
 * lives in the repo so the next person changing an icon can re-run it in one
 * command rather than reinventing this.
 */

import { chromium } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const source = await readFile(join(here, "..", "components", "icons.tsx"), "utf8");

/**
 * Extract each exported icon's name and body by brace-matching its component.
 *
 * A regex over the whole file cannot do this: the bodies contain nested JSX and
 * `{...rest}` spreads, so a non-greedy match to the next `}` truncates mid-path.
 * This walks depth from the opening `{` of `return (` instead.
 */
function extractIcons(src) {
  const icons = [];
  const re = /export function (\w+Icon)\(props: IconProps\) \{/g;
  let m;
  while ((m = re.exec(src)) !== null) {
    let depth = 1;
    let i = re.lastIndex;
    while (i < src.length && depth > 0) {
      if (src[i] === "{") depth += 1;
      else if (src[i] === "}") depth -= 1;
      i += 1;
    }
    const body = src.slice(re.lastIndex, i - 1);
    // Inline the one shared sub-component, so file icons render their sheet.
    const inner = body
      .replace(/<Icon \{\.\.\.props\}>/g, "")
      .replace(/<\/Icon>/g, "")
      .replace(/<FileFrame>/g, FILE_FRAME)
      .replace(/<\/FileFrame>/g, "")
      .replace(/<FileFrame \/>/g, FILE_FRAME)
      .replace(/return \(|\);\s*$/g, "")
      // JSX attribute syntax the browser will not parse.
      .replace(/(\w+)=\{([^}]*)\}/g, (_, key, value) => `${key}="${value.replace(/"/g, "")}"`)
      .replace(/fillOpacity/g, "fill-opacity")
      .replace(/strokeWidth/g, "stroke-width")
      .replace(/strokeLinecap/g, "stroke-linecap")
      .replace(/strokeLinejoin/g, "stroke-linejoin")
      .replace(/\{\/\*[\s\S]*?\*\/\}/g, "")
      .trim();
    icons.push({ name: m[1], inner });
  }
  return icons;
}

const FILE_FRAME = `
  <path d="M14 3.5H7A1.5 1.5 0 0 0 5.5 5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8Z" />
  <path d="M14 3.5V8h4.5" />
`;

const icons = extractIcons(source);
if (icons.length === 0) throw new Error("no icons extracted — did the signature change?");
console.log(`extracted ${icons.length} icons`);

const cells = icons
  .map(
    ({ name, inner }) => `
    <figure>
      <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor"
           stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">${inner}</svg>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
           stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">${inner}</svg>
      <figcaption>${name.replace(/Icon$/, "")}</figcaption>
    </figure>`,
  )
  .join("");

/** Both sizes per cell: a path can read correctly at 28px and mush at 16px. */
function page(theme) {
  const bg = theme === "dark" ? "#0e1016" : "#ffffff";
  const fg = theme === "dark" ? "#f1f2f4" : "#101828";
  const muted = theme === "dark" ? "#9b9ea6" : "#4a5565";
  return `<!doctype html><html><head><meta charset="utf-8"><style>
    body{margin:0;padding:20px;background:${bg};color:${fg};
      font:13px system-ui,-apple-system,sans-serif}
    h1{font-size:14px;margin:0 0 16px;color:${muted};font-weight:600}
    .grid{display:grid;grid-template-columns:repeat(8,1fr);gap:14px}
    figure{margin:0;display:flex;flex-direction:column;align-items:center;gap:4px}
    figcaption{font-size:10px;color:${muted};text-align:center;word-break:break-word}
  </style></head><body>
    <h1>${icons.length} icons — ${theme} — 28px and 16px</h1>
    <div class="grid">${cells}</div>
  </body></html>`;
}

const browser = await chromium.launch();
for (const theme of ["light", "dark"]) {
  const p = await browser.newPage({ viewport: { width: 900, height: 600 } });
  await p.setContent(page(theme));
  await p.screenshot({ path: `/tmp/icon-sheet-${theme}.png`, fullPage: true });
  await p.close();
  console.log(`wrote /tmp/icon-sheet-${theme}.png`);
}
await browser.close();
