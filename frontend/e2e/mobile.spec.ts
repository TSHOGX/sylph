import { expect, test, type Page } from "@playwright/test";
import { createSession, gotoReady } from "./fixtures/session";

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

  const composer = page.getByLabel("Message", { exact: true });
  await expect(composer).toBeVisible();

  // The reported bug, directly: nothing on the page may extend past the right edge.
  // The cause was a `<select>` sized by its longest *option* rather than by its
  // value, so no `min-w-0` could recover the width — the intrinsic minimum was
  // already too large.
  expect(await overflowingElements(page)).toEqual([]);

  // Selecting a value is what widened the row, so the invariant is re-checked after.
  await page.getByRole("button", { name: /^Model:/ }).click();
  // On a phone the menu is a bottom sheet rather than a popover: a 200px panel
  // pinned to a 32px button is both hard to hit and prone to landing under the
  // on-screen keyboard.
  const sheet = page.getByRole("dialog", { name: "Model" });
  await expect(sheet).toBeVisible();
  await sheet.getByRole("menuitemradio", { name: "Thorough" }).click();
  await expect(page.getByRole("button", { name: /^Model:/ })).toHaveText(/Thorough/);
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

  // Tapping a row dismisses it. Leaving the drawer over the conversation the user
  // just chose costs a second tap on every visit.
  //
  // Specifically the row for the session *already open*, which is the case that was
  // broken: the drawer's dismissal was derived from the pathname changing, and a
  // same-route navigation never changes it, so the drawer stayed put and the row
  // looked dead. Addressed by `aria-current` rather than by position, so this keeps
  // testing the same-route case even if sidebar ordering changes — `.first()` only
  // happened to be the current session on a clean journal.
  await page.getByRole("button", { name: "Open workspaces" }).click();
  await expect(drawer).toBeVisible();
  const current = drawer.locator('a[aria-current="page"]');
  await expect(current).toHaveCount(1);
  await current.click();
  await expect(drawer).toBeHidden();
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
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
  const composer = page.getByLabel("Message", { exact: true });
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
  // `gotoReady` rather than `goto`, because a shortcut is pressed further down and is
  // silently dropped if it arrives before the shell's keydown effect has run.
  await gotoReady(page);

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
  //
  // Note that neither assertion above proves hydration, which is why the gate is on
  // the navigation rather than here: `data-theme` is set by the blocking inline
  // script *before* React runs, and `getComputedStyle` reads CSS. Both pass on a
  // page that has not hydrated at all.
  await page.keyboard.press("ControlOrMeta+,");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();
  // "Light", capitalized: the label is now real text rather than lowercase DOM
  // text with a CSS `capitalize` on top, so the accessible name matches what is
  // on screen. That is the point of the change — a screen reader was previously
  // announcing "light" while the button read "Light".
  await settings.getByRole("button", { name: "Light", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

/**
 * A long catalogue on a phone: a sheet, filterable, and not behind the keyboard.
 *
 * The desktop popover focuses its filter field on open. This one deliberately does
 * not, and that difference is the spec: focusing raises the on-screen keyboard, and
 * iOS Safari does not resize the layout viewport when it appears — so a sheet sized
 * in `dvh` keeps its full height and its rows end up underneath the keyboard, which
 * is the exact failure the sheet form exists to avoid. One tap on the field, which
 * sits at the top of the sheet, is what every native picker asks for.
 *
 * Chromium cannot raise a keyboard, so this asserts the decision that avoids it
 * (focus is not taken) plus the consequences it can observe: the field works when
 * tapped, and nothing overflows.
 */
test("a long catalogue is a filterable sheet that does not grab focus", async ({ page }) => {
  await createSession(page);

  await page.getByRole("button", { name: /^Provider model:/ }).click();
  const sheet = page.getByRole("dialog", { name: "Provider model" });
  await expect(sheet).toBeVisible();
  await settled(page, sheet);

  const filter = sheet.getByRole("combobox", { name: /filter provider model/i });
  await expect(filter).toBeVisible();
  await expect(filter).not.toBeFocused();

  // The sheet is a scroll container of its own, so a fourteen-item catalogue is
  // reachable rather than clipped — and the sheet itself must not overflow.
  expect(await overflowingElements(page)).toEqual([]);

  await filter.tap();
  await filter.fill("qwen");
  const rows = sheet.getByRole("option");
  await expect(rows).toHaveCount(1);
  await rows.first().tap();

  await expect(sheet).toBeHidden();
  await expect(page.getByRole("button", { name: /^Provider model:/ })).toHaveText(/Qwen 3 Max/);
  expect(await overflowingElements(page)).toEqual([]);
});

/**
 * An agent's question, on a phone.
 *
 * The densest surface in the app: two grouped choice lists, a note field, and a verb
 * row, all inside a panel above the composer. Every part of it is new, and a layout
 * bug here is invisible to the desktop suite — at 1280px the same markup is merely
 * roomy.
 */
test("a request form fits the viewport and is answerable by tapping", async ({ page }) => {
  await createSession(page);

  await page.getByLabel("Message", { exact: true }).fill("question please");
  await page.getByRole("button", { name: "Send", exact: true }).tap();

  const panel = page.getByTestId("approval-dialog");
  await expect(panel).toBeVisible({ timeout: 15000 });
  expect(await overflowingElements(page)).toEqual([]);

  // Tapped rather than checked: the whole row is the label, which is what makes a
  // 16px control usable in a 44px target.
  await panel.getByRole("radio", { name: "Tabs" }).tap();
  await panel.getByRole("checkbox", { name: "ESLint" }).tap();
  await panel.getByLabel("Notes (optional)").fill("only generated files");
  expect(await overflowingElements(page)).toEqual([]);

  await panel.getByRole("button", { name: "Send answer" }).tap();

  // The answer the agent received, which the fixture echoes into its result. A
  // result is collapsed to one line by default, so its body is behind `more`.
  const result = page.getByRole("region", { name: "Tool result" });
  await expect(result).toBeVisible({ timeout: 15000 });
  await result.getByRole("button", { name: "more" }).tap();
  await expect(result).toContainText('"q:0": "Tabs"');
  expect(await overflowingElements(page)).toEqual([]);
});

/**
 * A nested delegation tree on a phone.
 *
 * This is where an indented tree fails, and the failure is invisible at 1280px
 * where the same markup is merely cramped. Each level of nesting spends horizontal
 * space that a 412px viewport does not have, so a depth-3 tree either overflows or
 * truncates a tool call's path to nothing — which is why indentation stops at two
 * levels and focus mode carries the rest. The assertion is the same overflow probe
 * the composer uses, because "does it fit" is the whole question.
 */
test("a delegation tree three levels deep still fits the viewport", async ({ page }) => {
  await createSession(page);

  await page.getByLabel("Message", { exact: true }).fill("delegate this please");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // The failed branch expands itself to the tool that failed, so this is already
  // the deepest state the tree reaches without any interaction.
  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toBeVisible({ timeout: 15000 });
  await expect(verify.getByRole("region", { name: "Worker delegation" })).toBeVisible();

  expect(await overflowingElements(page)).toEqual([]);

  // Open the sibling too: two expanded branches plus the stranded card is the
  // widest and tallest the timeline gets on this fixture.
  await page
    .getByRole("region", { name: "Explore delegation" })
    .getByRole("button", { name: "Explore", exact: true })
    .tap();
  expect(await overflowingElements(page)).toEqual([]);

  // Focus mode re-roots and resets the indent budget, which is the point of it.
  await verify.getByRole("button", { name: "Focus Verify" }).tap();
  await expect(page.getByRole("navigation", { name: "Delegation path" })).toBeVisible();
  expect(await overflowingElements(page)).toEqual([]);
});

/**
 * The three shortcut overlays, on a phone.
 *
 * They were the last surfaces still rendering their own field, rows, and cursor, and
 * they migrated onto the shared choice list — which changed the markup of every row on
 * a viewport no spec exercised them at. Two of the pitfalls this repo records about
 * lists are only observable below the breakpoint: a focused field under 16px zooms
 * mobile Safari and does not zoom back, and a row wide enough to overflow reads as a
 * broken app rather than a cramped one.
 *
 * The history browser earns the widest coverage here, because its paging control moved
 * out of the list and into the footer — which is where the safe-area inset lives, and
 * where a control placed wrongly is simply unreachable on a notched device.
 */
test("the shortcut overlays fit the viewport and their fields do not trigger zoom", async ({
  page,
}) => {
  await createSession(page);

  for (const [combo, name] of [
    ["ControlOrMeta+p", /command query/i],
    ["ControlOrMeta+k", /search query/i],
    ["ControlOrMeta+Shift+k", /filter this workspace/i],
  ] as const) {
    await page.keyboard.press(combo);
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();

    const field = dialog.getByRole("combobox", { name });
    await expect(field).toBeVisible();

    // 16px is the threshold: mobile Safari zooms a focused field below it and does not
    // zoom back out, which presents as "the whole screen moved". Read as a computed
    // value rather than asserted as a class — the class is an implementation detail
    // and the pixel size is the contract.
    const size = await field.evaluate((node) =>
      Number.parseFloat(window.getComputedStyle(node).fontSize),
    );
    expect(size, `filter font size with ${combo} open`).toBeGreaterThanOrEqual(16);

    expect(await overflowingElements(page), `overflow with ${combo} open`).toEqual([]);

    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  }
});
