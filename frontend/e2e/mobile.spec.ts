import { expect, test, type Page } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * The phone layout.
 *
 * # Why this file exists at all
 *
 * Three reported failures — the composer running off the right edge, the sidebars
 * leaving ~150px for the conversation, and scrolling dragging the whole shell into
 * blank space — were all invisible to the existing suite. Not because it was
 * careless, but because every spec in it runs at 1280px, where the same code is at
 * worst a little cramped. A layout bug that only manifests below a breakpoint can
 * only be observed below that breakpoint.
 *
 * # What each spec pins, and why that assertion
 *
 * The temptation is to assert pixel values. These deliberately do not: a spec that
 * knows the composer is 358px wide fails on the next padding change and says
 * nothing about whether it is usable. Each one instead asserts the *invariant* the
 * fix established — nothing extends past the viewport, the document does not scroll,
 * the drawer covers rather than displaces — so it keeps holding as the design moves.
 *
 * # The limit of this file
 *
 * `devices["Pixel 7"]` is Chromium, so `pointer: coarse` and a 412px viewport are
 * real, but iOS Safari is not covered: neither its collapsing-toolbar `dvh`
 * behaviour nor its zoom-on-focus. Both are mechanisms behind the scroll bug, so
 * they are asserted here through consequences a Chromium phone *can* observe, and
 * still want a pass on a real device.
 */

/**
 * Wait for an edge-anchored surface to finish sliding in.
 *
 * `toBeVisible` resolves as soon as the element is painted, which for a drawer or a
 * sheet is the first frame of a 180–240ms transform. Geometry read at that moment
 * is mid-animation: a sheet measured then reports a bottom edge ~100px past the
 * viewport, which is indistinguishable from a genuinely clipped panel. Awaiting the
 * element's own animations is exact, where a fixed sleep is a guess that gets
 * slower or flakier as the easing changes.
 */
async function settled(page: Page, locator: ReturnType<Page["getByRole"]>) {
  await locator.evaluate((node: HTMLElement) =>
    Promise.all(
      [node, node.parentElement]
        .filter((n): n is HTMLElement => n !== null)
        .flatMap((n) => n.getAnimations())
        .map((a) => a.finished),
    ),
  );
}

/** Width of the layout viewport, as the page sees it. */
async function viewportWidth(page: Page): Promise<number> {
  return page.evaluate(() => document.documentElement.clientWidth);
}

/**
 * Anything whose box extends past the right edge of the viewport.
 *
 * Reported per element rather than as a boolean, because "something overflows" is
 * not actionable and this is the assertion most likely to catch a future
 * regression. A 1px tolerance absorbs subpixel rounding at a fractional device
 * pixel ratio.
 */
async function overflowingElements(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const limit = document.documentElement.clientWidth + 1;
    const offenders: string[] = [];
    for (const node of Array.from(document.querySelectorAll("body *"))) {
      const box = node.getBoundingClientRect();
      // Zero-sized nodes are `sr-only` text and unmounted branches.
      if (box.width === 0 && box.height === 0) continue;
      if (box.right > limit) {
        const el = node as HTMLElement;
        offenders.push(
          `${el.tagName.toLowerCase()}${el.className ? "." + String(el.className).split(/\s+/)[0] : ""} right=${Math.round(box.right)}`,
        );
      }
    }
    return offenders;
  });
}

test("the composer fits the viewport, with a flag selected and with a long message", async ({
  page,
}) => {
  await createSession(page);

  const composer = page.getByLabel("Message");
  await expect(composer).toBeVisible();

  // The reported bug, directly: nothing on the page may extend past the right edge.
  // The cause was a `<select>` sized by its longest *option* rather than by its
  // value, so no `min-w-0` could recover the width — the intrinsic minimum was
  // already too large.
  expect(await overflowingElements(page)).toEqual([]);

  // Selecting a flag is what widened the row, so the invariant is re-checked after.
  await page.getByRole("button", { name: "Model" }).click();
  // On a phone the menu is a bottom sheet rather than a popover: a 200px panel
  // pinned to a 32px button is both hard to hit and prone to landing under the
  // on-screen keyboard.
  const sheet = page.getByRole("dialog", { name: "Model" });
  await expect(sheet).toBeVisible();
  await sheet.getByRole("menuitemradio", { name: "thorough" }).click();
  await expect(page.getByRole("button", { name: /thorough/ })).toBeVisible();
  expect(await overflowingElements(page)).toEqual([]);

  // And with content in it, since the textarea grows.
  await composer.fill("A ".repeat(400));
  expect(await overflowingElements(page)).toEqual([]);

  // The textarea is bounded rather than free-growing: an unbounded one eats the
  // transcript, and a fixed two-row box hides the start of a long message.
  const box = await composer.boundingBox();
  const height = await page.evaluate(() => window.innerHeight);
  expect(box!.height).toBeLessThanOrEqual(height * 0.45);
});

test("the conversation owns the full width; sidebars are not in flow", async ({ page }) => {
  await createSession(page);

  // `showLeftSidebar` defaults to true, and that default is exactly what crushed the
  // conversation: a 240px in-flow pane on a 412px viewport. Below the breakpoint the
  // pane must not be laid out at all, regardless of the stored preference.
  await expect(page.getByRole("complementary", { name: "Workspaces" })).toBeHidden();
  await expect(page.getByRole("complementary", { name: "Artifacts" })).toBeHidden();

  const width = await viewportWidth(page);
  const main = await page.locator("#main").boundingBox();
  expect(main!.width).toBe(width);
});

test("the sessions drawer overlays the conversation and dismisses on navigation", async ({
  page,
}) => {
  await createSession(page);

  // A hamburger, not a pane toggle: on a phone there is no column beside the
  // content for a pane glyph to depict.
  await page.getByRole("button", { name: "Open workspaces" }).click();

  const drawer = page.getByRole("dialog", { name: "Workspaces" });
  await expect(drawer).toBeVisible();
  await settled(page, drawer);
  // It overlays rather than displaces: `#main` keeps the full width behind it, which
  // is what makes the sliver of visible conversation a dismiss affordance.
  const width = await viewportWidth(page);
  expect((await page.locator("#main").boundingBox())!.width).toBe(width);
  // And leaves that sliver, rather than covering everything — a drawer that covers
  // the screen is indistinguishable from a navigation.
  expect((await drawer.boundingBox())!.width).toBeLessThan(width);

  // Escape closes it. The drawer stops the key itself, so the shell's document-level
  // handler does not also fire and close something behind it.
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  // Navigating dismisses it. Tapping a session *is* a navigation, and leaving the
  // drawer over the conversation the user just chose costs a second tap every time.
  await page.getByRole("button", { name: "Open workspaces" }).click();
  await expect(drawer).toBeVisible();
  await drawer.getByRole("link").first().click();
  await expect(drawer).toBeHidden();
  await expect(page.getByLabel("Message")).toBeVisible();
});

test("files open as a bottom sheet, and a file opens full screen from it", async ({ page }) => {
  await createSession(page, { workdir: "/tmp" });

  await page.getByRole("button", { name: "Open files" }).click();
  const sheet = page.getByRole("dialog", { name: "Files" });
  await expect(sheet).toBeVisible();

  // Settled, not mid-flight. The panel slides up over 240ms, so a measurement taken
  // on `toBeVisible` catches it partway and reports a box extending past the bottom
  // edge — which reads exactly like a clipped sheet and is not one. Awaiting the
  // animation is also more honest than sleeping a guessed interval.
  await settled(page, sheet);

  // A sheet, not a drawer: files are inspection *of the thing on screen*, so the
  // conversation stays visible above it. It therefore must not reach the top edge,
  // and must sit entirely within the viewport once it has arrived.
  const box = await sheet.boundingBox();
  const height = await page.evaluate(() => window.innerHeight);
  expect(box!.y).toBeGreaterThan(0);
  expect(box!.y + box!.height).toBeLessThanOrEqual(height + 1);

  await expect(sheet.getByRole("tree")).toBeVisible();
  expect(await overflowingElements(page)).toEqual([]);
});

test("the document does not scroll, so the shell cannot be dragged off screen", async ({
  page,
}) => {
  await createSession(page);
  const composer = page.getByLabel("Message");
  await composer.fill("Say something");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // The reported bug: "scrolling drags the whole screen up and reveals blank space".
  // Three causes, all closed together — `100vh` measuring as if the browser's
  // toolbars were hidden, an unpinned document, and overscroll chaining upward. The
  // observable invariant is that the document has nothing to scroll: the shell is
  // exactly the viewport, and every scrolling region is inside it.
  const doc = await page.evaluate(() => ({
    scrollHeight: document.documentElement.scrollHeight,
    clientHeight: document.documentElement.clientHeight,
    bodyOverflow: getComputedStyle(document.body).overflowY,
  }));
  expect(doc.bodyOverflow).toBe("hidden");
  expect(doc.scrollHeight).toBeLessThanOrEqual(doc.clientHeight);

  // A wheel gesture past the end of the transcript must not move the document.
  await page.mouse.move(200, 300);
  await page.mouse.wheel(0, 4000);
  expect(await page.evaluate(() => window.scrollY)).toBe(0);

  // The composer stays reachable at the bottom throughout, which is the thing the
  // drag was taking away.
  await expect(composer).toBeInViewport();
});

test("the app is installable: manifest, icons, and a standalone display mode", async ({
  page,
  request,
}) => {
  await page.goto("/");

  // Linked from the document, or a browser never looks for it.
  await expect(page.locator('link[rel="manifest"]')).toHaveCount(1);

  const response = await request.get("/manifest.webmanifest");
  expect(response.ok()).toBe(true);
  const manifest = await response.json();

  // `standalone`, not `minimal-ui`: a URL bar for an app that navigates entirely
  // client-side is a row of pixels showing a path the user cannot use, and it costs
  // exactly the height the composer needs.
  expect(manifest.display).toBe("standalone");
  expect(manifest.start_url).toBe("/");

  // Android requires a 192 and a 512, and a maskable entry separate from the
  // full-bleed one — a launcher crops a maskable icon to its own shape, so serving
  // one file as both either crops the mark or floats it small in a large box.
  const sizes = manifest.icons.map((icon: { sizes: string }) => icon.sizes);
  expect(sizes).toContain("192x192");
  expect(sizes).toContain("512x512");
  expect(
    manifest.icons.some((icon: { purpose?: string }) => icon.purpose === "maskable"),
  ).toBe(true);

  // Every declared icon must actually exist. `output: "standalone"` omits `public/`
  // from its bundle, so a missing copy step shows up as icons that 404 while the
  // manifest still parses — an install that silently produces a blank tile.
  for (const icon of manifest.icons as { src: string }[]) {
    const asset = await request.get(icon.src);
    expect(asset.ok(), `${icon.src} is declared but not served`).toBe(true);
  }

  // iOS ignores the manifest for both of these. Without `apple-touch-icon` it
  // screenshots the page and uses that as the home-screen icon; without
  // `mobile-web-app-capable` the install is a bookmark that opens in Safari chrome.
  await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveCount(1);
  const capable = page.locator('meta[name="mobile-web-app-capable"]');
  await expect(capable).toHaveAttribute("content", "yes");
});

test("dark mode applies before paint and follows an explicit choice", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");

  // Resolved by the inline bootstrap script, not by a React effect: an effect runs
  // after paint by definition, so a dark-mode user would see a white page for the
  // length of hydration.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  // The canvas actually follows the token, rather than the attribute being set and
  // ignored — which is the failure mode of a half-migrated palette.
  const canvas = await page.evaluate(() =>
    getComputedStyle(document.body).backgroundColor,
  );
  expect(canvas).not.toBe("rgb(255, 255, 255)");

  // An explicit choice overrides the OS and keeps overriding it.
  await page.keyboard.press("ControlOrMeta+,");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "light", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});
