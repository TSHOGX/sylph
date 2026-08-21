import { expect, test, type Page } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";

/**
 * The Artifacts file viewer: the tree/viewer swap, fullscreen, and formats.
 *
 * # Why these specs write their own workdir
 *
 * The shared fixture defaults to `/tmp`, whose contents are arbitrary, so a spec
 * using it can only assert that a tree exists. Asserting on *file contents* needs
 * a directory the spec controls, so each test mkdtemps one and passes it to
 * `createSession`. Specs run in Node, so `fs` is available.
 *
 * The session itself is a precondition here, not the subject, so it is created
 * through the API via the shared fixture rather than by driving the picker — which
 * is what keeps a change to the create UI from failing these specs. The create
 * interaction is owned by `workspace-shell.spec.ts`.
 *
 * # What is deliberately not covered here
 *
 * Excel, Word, and PowerPoint fixtures are **not** written from these specs. All
 * three are zip containers, Node ships no zip writer, and adding a dependency to
 * produce one fixture is the wrong trade. They are covered instead by Rust
 * integration tests over synthesized packages — which also assert the parsed
 * payload, something a browser test cannot see — plus vitest over the pure
 * presentation logic. CSV and HTML are plain bytes, so they are written here and
 * exercise the same grid and sandbox code paths end to end.
 */

/** A directory this spec owns, so tree and viewer assertions are stable. */
function workdir(files: Record<string, string>): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sylph-e2e-files-"));
  for (const [name, contents] of Object.entries(files)) {
    fs.writeFileSync(path.join(dir, name), contents);
  }
  return dir;
}

/** Open a session on `dir` and reveal the Artifacts pane. */
async function openPane(page: Page, dir: string) {
  await createSession(page, { workdir: dir });
  await page.keyboard.press("ControlOrMeta+Backslash");
  const pane = page.getByRole("complementary", { name: "Artifacts" });
  await expect(pane).toBeVisible();
  return pane;
}

/**
 * The spec that pins the swap.
 *
 * It fails if anyone converts the `hidden` toggle to conditional rendering, which
 * is the whole reason the tree survives a round trip through the viewer.
 */
test("opening a file swaps the tree for the viewer, and Back restores both", async ({
  page,
}) => {
  const dir = workdir({ "notes.md": "# Title\n\nBody text.\n" });
  fs.mkdirSync(path.join(dir, "nested"));
  fs.writeFileSync(path.join(dir, "nested", "inner.txt"), "inner file\n");

  const pane = await openPane(page, dir);
  const tree = pane.getByRole("tree");
  await expect(tree).toBeVisible();

  // Expand a directory, so there is state that unmounting would destroy.
  await pane.getByRole("button", { name: /nested/ }).click();
  await expect(pane.getByRole("button", { name: /inner\.txt/ })).toBeVisible();

  await pane.getByRole("button", { name: /notes\.md/ }).click();

  // The viewer takes the pane and the tree is gone from view *and* from the
  // accessibility tree, so a keyboard user cannot tab into an invisible tree.
  await expect(pane.getByRole("region", { name: /preview of notes\.md/i })).toBeVisible();
  await expect(tree).toBeHidden();

  await pane.getByRole("button", { name: /Files/ }).click();
  await expect(tree).toBeVisible();
  // The expanded directory is still expanded: this is what `hidden` buys.
  await expect(pane.getByRole("button", { name: /inner\.txt/ })).toBeVisible();
});

test("the expand control opens a dialog that Escape closes, restoring focus", async ({
  page,
}) => {
  const dir = workdir({ "data.csv": "region,revenue\nAPAC,1200\n" });
  const pane = await openPane(page, dir);

  await pane.getByRole("button", { name: /data\.csv/ }).click();
  const expand = pane.getByRole("button", { name: "Open full screen" });
  await expand.click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  // Near-full-bleed, which is the point of the viewer size variant.
  const box = await dialog.boundingBox();
  const viewport = page.viewportSize();
  expect(box!.width).toBeGreaterThan(viewport!.width * 0.8);

  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  // Focus returns to the control that opened it, not to the top of the document.
  await expect(expand).toBeFocused();
});

/** The one-modal-at-a-time invariant. */
test("opening a picker dismisses the fullscreen viewer", async ({ page }) => {
  const dir = workdir({ "data.csv": "a,b\n1,2\n" });
  const pane = await openPane(page, dir);

  await pane.getByRole("button", { name: /data\.csv/ }).click();
  await pane.getByRole("button", { name: "Open full screen" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();

  // Opening the palette must leave exactly one modal surface up. Without this,
  // Escape and focus restoration would have to reason about which of two stacked
  // dialogs owns them.
  await page.keyboard.press("ControlOrMeta+p");
  await expect(dialog).toBeHidden();
  await expect(page.getByPlaceholder(/command/i)).toBeVisible();

  // The pane viewer is still there behind it — closing the dialog returns to the
  // file rather than to the tree, since the file is still selected.
  await page.keyboard.press("Escape");
  await expect(
    pane.getByRole("region", { name: /preview of data\.csv/i }),
  ).toBeVisible();
});

test("a CSV renders as a table with real column letters and typed alignment", async ({
  page,
}) => {
  const dir = workdir({
    // A quoted comma and an embedded newline, which naive splitting would break.
    "data.csv": 'region,revenue\n"Smith, Alice",1200\nBob,980\n',
  });
  const pane = await openPane(page, dir);
  await pane.getByRole("button", { name: /data\.csv/ }).click();

  const table = pane.getByRole("table");
  await expect(table).toBeVisible();
  // Spreadsheet column letters, so a cell reference read off the screen is real.
  await expect(table.getByRole("columnheader", { name: "A", exact: true })).toBeVisible();
  await expect(table.getByRole("cell", { name: "Smith, Alice" })).toBeVisible();
  await expect(table.getByRole("cell", { name: "1200" })).toBeVisible();
});

/**
 * The security spec.
 *
 * A workspace HTML file may be agent-generated. Rendering it with our privileges
 * would let it read the DOM and issue same-origin fetches carrying the session
 * cookie, so the frame must render the markup while running none of its script.
 */
test("HTML renders in a frame that cannot run scripts or reach the page", async ({
  page,
}) => {
  const dir = workdir({
    "page.html":
      "<h1>Generated report</h1>" +
      "<style>h1 { color: rgb(10, 20, 30) }</style>" +
      "<script>document.title = 'pwned'; parent.document.body.innerHTML = 'breached';</script>",
  });
  const pane = await openPane(page, dir);
  const titleBefore = await page.title();

  await pane.getByRole("button", { name: /page\.html/ }).click();

  const frame = pane.locator("iframe");
  await expect(frame).toBeVisible();
  // Present and empty: an empty sandbox withholds every permission, including
  // allow-scripts and allow-same-origin. Anything else here is a regression.
  await expect(frame).toHaveAttribute("sandbox", "");

  const inner = page.frameLocator("iframe");
  // The markup renders and inline CSS applies, so the preview is useful...
  await expect(inner.locator("h1")).toHaveText("Generated report");
  await expect(inner.locator("h1")).toHaveCSS("color", "rgb(10, 20, 30)");
  // ...while the script did not run: the title it tried to set is unchanged, and
  // the parent document it tried to rewrite is intact.
  expect(await page.title()).toBe(titleBefore);
  await expect(pane.getByRole("button", { name: /Files/ })).toBeVisible();

  // The frame cannot read the session cookie, which is the actual escalation
  // this closes: an opaque origin has no access to it at all.
  const cookieAccess = await page
    .frames()
    .find((f) => f !== page.mainFrame())!
    .evaluate(() => {
      try {
        return document.cookie;
      } catch (error) {
        return `THREW ${(error as Error).name}`;
      }
    });
  expect(cookieAccess).toContain("THREW");

  // Source is one click away, because reading the markup is a real need.
  await pane.getByRole("button", { name: "View source" }).click();
  await expect(pane.getByText("Generated report", { exact: false })).toBeVisible();
});

test("an image opens in a viewer offering fit and zoom", async ({ page }) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sylph-e2e-img-"));
  // A 1x1 transparent GIF, small enough to write as bytes from a spec.
  fs.writeFileSync(
    path.join(dir, "pixel.gif"),
    Buffer.from("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7", "base64"),
  );

  const pane = await openPane(page, dir);
  await pane.getByRole("button", { name: /pixel\.gif/ }).click();

  await expect(pane.getByRole("img", { name: /pixel\.gif/ })).toBeVisible();
  await expect(pane.getByRole("button", { name: "Zoom in" })).toBeVisible();
  // Fit is the default in a narrow pane; clicking it switches to 100%.
  await pane.getByRole("button", { name: "Fit" }).click();
  await expect(pane.getByRole("button", { name: "100%" })).toBeVisible();
});

test("the viewer and the fullscreen dialog have no accessibility violations", async ({
  page,
}) => {
  const dir = workdir({ "data.csv": "region,revenue\nAPAC,1200\nEMEA,980\n" });
  const pane = await openPane(page, dir);
  await pane.getByRole("button", { name: /data\.csv/ }).click();
  await expect(pane.getByRole("table")).toBeVisible();

  const { default: AxeBuilder } = await import("@axe-core/playwright");
  const audit = async (context: string) => {
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
      .analyze();
    const violations = results.violations.map((v) => ({
      id: v.id,
      impact: v.impact,
      nodes: v.nodes.slice(0, 3).map((n) => n.target.join(" ")),
    }));
    expect(violations, `axe violations on ${context}`).toEqual([]);
  };

  // A data table and an iframe are both new surface, so both get audited.
  await audit("the file viewer in the pane");

  await pane.getByRole("button", { name: "Open full screen" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await audit("the fullscreen file viewer");
});
