/**
 * Rasterise the app icons from their SVG sources.
 *
 * Run after editing `public/icons/*.svg`:
 *
 *   node scripts/generate-icons.mjs
 *
 * # Why the PNGs are committed rather than built
 *
 * They change roughly never, and a build step that needs a browser to produce a
 * favicon would put Playwright on the critical path of `next build` — including in
 * E2E and in `scripts/install.sh`. Committed output keeps the install path to
 * `npm ci && next build`.
 *
 * # Why PNG at all, when the manifest also lists the SVG
 *
 * iOS ignores the manifest for `apple-touch-icon` and requires a raster, and
 * Android's maskable path is only reliable with one. The SVG entry is a bonus for
 * browsers that prefer vector, not a replacement.
 *
 * # Why Playwright
 *
 * It is already a devDependency with Chromium installed, so this needs no new tool.
 * `sips` cannot read SVG, and ImageMagick and rsvg are not present. Chromium is
 * also the renderer that will actually display these, so what is rasterised here is
 * what a user sees.
 */

import { chromium } from "@playwright/test";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const iconsDir = join(dirname(fileURLToPath(import.meta.url)), "..", "public", "icons");

/**
 * What to emit.
 *
 * 192 and 512 are the two sizes the manifest spec calls for; 512 is also what
 * Android upscales for a splash screen. 180 is `apple-touch-icon`, which iOS reads
 * at exactly that size. The maskable source is separate — see its own comment.
 */
const TARGETS = [
  { source: "icon.svg", size: 192, out: "icon-192.png" },
  { source: "icon.svg", size: 512, out: "icon-512.png" },
  { source: "icon.svg", size: 180, out: "apple-touch-icon.png" },
  { source: "icon-maskable.svg", size: 512, out: "icon-maskable-512.png" },
];

const browser = await chromium.launch();
try {
  for (const { source, size, out } of TARGETS) {
    const svg = await readFile(join(iconsDir, source), "utf8");
    // `deviceScaleFactor: 1` with an exact viewport, so the screenshot is the
    // requested pixel size rather than a scaled approximation of it.
    const page = await browser.newPage({ viewport: { width: size, height: size } });
    // A data URL rather than a file path: it needs no server, and it keeps the page
    // origin opaque so nothing in the SVG could reach the filesystem.
    await page.goto(`data:text/html,${encodeURIComponent(
      // `margin: 0` and a block-level SVG, or the body's default margin and the
      // inline baseline gap both shift the artwork by a few pixels.
      `<!doctype html><style>
         html,body{margin:0;padding:0}
         svg{display:block;width:${size}px;height:${size}px}
       </style>${svg}`,
    )}`);
    // `omitBackground` keeps the corners the SVG rounds off transparent, rather
    // than filling them white — which would show as white notches on any launcher
    // whose background is not white.
    const png = await page.screenshot({ omitBackground: true });
    await writeFile(join(iconsDir, out), png);
    await page.close();
    console.log(`wrote ${out} (${size}x${size}, ${png.length} bytes)`);
  }
} finally {
  await browser.close();
}
