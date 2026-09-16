import { expect, test, type Locator, type Page } from "@playwright/test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";

/**
 * Creating, uploading, and deleting files from the Artifacts pane.
 *
 * # Why these are E2E rather than unit tests
 *
 * Every one of them spans three layers that unit tests cannot join: a menu
 * item opens a dialog, the dialog calls the API, and the tree refetches. The
 * failures worth catching all live in the seams — a dialog that renders inside a
 * `hidden` ancestor and cannot take focus, a mutation that succeeds without
 * invalidating the query, a confirm that can be bypassed by pressing Enter on
 * arrival. None of those are visible to a test that calls the hook directly.
 *
 * # Why each spec owns a `mkdtemp` directory
 *
 * These specs *mutate* the workspace, so sharing one would let an earlier spec's
 * leftovers decide a later spec's assertions. Own-directory also means asserting
 * against the real filesystem afterwards, which is the only way to tell a UI that
 * reports success from a backend that did the work.
 */

function workdir(files: Record<string, string> = {}): string {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sylph-e2e-ops-"));
  for (const [name, contents] of Object.entries(files)) {
    fs.writeFileSync(path.join(dir, name), contents);
  }
  return dir;
}

async function openPane(page: Page, dir: string) {
  await createSession(page, { workdir: dir });
  await page.keyboard.press("ControlOrMeta+Backslash");
  const pane = page.getByRole("complementary", { name: "Trace inspector" });
  await expect(pane).toBeVisible();
  return pane;
}

/** A row's activating button, matched by `title` — see `file-viewer.spec.ts`. */
function treeRow(pane: Locator, relative: string): Locator {
  return pane.getByTitle(relative, { exact: true });
}

/** Open one row's action menu through its overflow button, not right-click. */
async function openRowMenu(pane: Locator, name: string) {
  await pane.getByRole("button", { name: `${name} actions` }).click();
  return pane.page().getByRole("menu");
}

/**
 * The pane's hidden upload input.
 *
 * Scoped to the pane: the composer carries its own `input[type="file"]` for turn
 * attachments, so a page-wide selector matches two and fails strict mode. That the
 * two exist separately is deliberate — one adds bytes to a message, the other
 * writes a file to disk.
 */
function uploadInput(pane: Locator): Locator {
  return pane.locator('input[type="file"]');
}

test("the header creates a folder in the working directory", async ({ page }) => {
  const dir = workdir({ "existing.txt": "x\n" });
  const pane = await openPane(page, dir);

  await pane.getByRole("button", { name: "New folder", exact: true }).click();

  // A prompt, not a `window.confirm`: it is in the page's accessibility tree, so
  // it can be addressed by role. That is also the point of not using the native
  // dialog — Playwright cannot see into one.
  const dialog = page.getByRole("alertdialog", { name: "New folder" });
  await expect(dialog).toBeVisible();
  // The input holds focus on open, so the user can type immediately.
  await expect(dialog.getByRole("textbox")).toBeFocused();

  await dialog.getByRole("textbox").fill("drafts");
  await dialog.getByRole("button", { name: "Create" }).click();

  await expect(dialog).toBeHidden();
  // The tree refetched: this fails if the mutation forgets to invalidate.
  await expect(treeRow(pane, "drafts")).toBeVisible();
  expect(fs.statSync(path.join(dir, "drafts")).isDirectory()).toBe(true);
});

test("Enter submits the new-folder name", async ({ page }) => {
  const dir = workdir();
  const pane = await openPane(page, dir);

  await pane.getByRole("button", { name: "New folder", exact: true }).click();
  const dialog = page.getByRole("alertdialog", { name: "New folder" });
  await dialog.getByRole("textbox").fill("报告");
  // A prompt whose only commit path is a button means dismissing the on-screen
  // keyboard to find it. A non-ASCII name also pins that nothing strips it.
  await dialog.getByRole("textbox").press("Enter");

  await expect(dialog).toBeHidden();
  expect(fs.existsSync(path.join(dir, "报告"))).toBe(true);
});

test("a name the backend refuses is reported in place, keeping the dialog open", async ({
  page,
}) => {
  const dir = workdir();
  fs.mkdirSync(path.join(dir, "taken"));
  const pane = await openPane(page, dir);

  await pane.getByRole("button", { name: "New folder", exact: true }).click();
  const dialog = page.getByRole("alertdialog", { name: "New folder" });
  await dialog.getByRole("textbox").fill("taken");
  await dialog.getByRole("button", { name: "Create" }).click();

  // The dialog survives the failure with the typed name intact, so the fix is one
  // edit rather than a re-open. Closing on error would discard the input.
  await expect(dialog.getByRole("alert")).toContainText(/already exists/i);
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("textbox")).toHaveValue("taken");
});

test("a folder's menu creates a folder inside it", async ({ page }) => {
  const dir = workdir();
  fs.mkdirSync(path.join(dir, "src"));
  const pane = await openPane(page, dir);

  const menu = await openRowMenu(pane, "src");
  await menu.getByRole("menuitem", { name: "New folder" }).click();

  const dialog = page.getByRole("alertdialog", { name: "New folder" });
  // The destination is stated, because "New folder" from a row menu is otherwise
  // ambiguous about which folder it lands in.
  await expect(dialog).toContainText("src");
  await dialog.getByRole("textbox").fill("nested");
  await dialog.getByRole("button", { name: "Create" }).click();

  await expect(dialog).toBeHidden();
  expect(fs.statSync(path.join(dir, "src/nested")).isDirectory()).toBe(true);
});

/**
 * The confirmation gate. This is the spec that matters most in this file: the
 * backend has no trash, so a delete that escapes its confirm is unrecoverable.
 */
test("deleting a file asks first, and Cancel leaves it on disk", async ({ page }) => {
  const dir = workdir({ "keep.txt": "important\n" });
  const pane = await openPane(page, dir);

  const menu = await openRowMenu(pane, "keep.txt");
  await menu.getByRole("menuitem", { name: "Delete" }).click();

  const dialog = page.getByRole("alertdialog", { name: "Delete this file?" });
  await expect(dialog).toContainText("keep.txt");
  await expect(dialog).toContainText(/cannot be undone/i);

  // Focus is on Cancel, not on Delete. A keystroke already in flight when the
  // dialog opens must not destroy a file.
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeFocused();

  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  expect(fs.existsSync(path.join(dir, "keep.txt"))).toBe(true);

  // Escape is the other dismissal, and must be equally harmless.
  await openRowMenu(pane, "keep.txt");
  await page.getByRole("menu").getByRole("menuitem", { name: "Delete" }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("alertdialog")).toBeHidden();
  expect(fs.existsSync(path.join(dir, "keep.txt"))).toBe(true);
});

test("confirming a delete removes the file and the row", async ({ page }) => {
  const dir = workdir({ "stale.txt": "x\n", "keep.txt": "y\n" });
  const pane = await openPane(page, dir);

  const menu = await openRowMenu(pane, "stale.txt");
  await menu.getByRole("menuitem", { name: "Delete" }).click();
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Delete" })
    .click();

  await expect(treeRow(pane, "stale.txt")).toBeHidden();
  expect(fs.existsSync(path.join(dir, "stale.txt"))).toBe(false);
  // The sibling is untouched, which distinguishes a delete from a failed refetch
  // that emptied the listing.
  await expect(treeRow(pane, "keep.txt")).toBeVisible();
});

/**
 * A folder's contents go with it, in one question.
 *
 * The API refuses a non-empty directory without `recursive`, which is right for an
 * API and wrong as a UI step: confirm, fail, confirm again is two dialogs for one
 * decision. So the dialog says what will happen and sends the flag.
 */
test("deleting a folder says its contents go too, and they do", async ({ page }) => {
  const dir = workdir();
  // Not `build` or `dist`: those are on the backend's `SKIPPED_DIRS` list and are
  // never listed, so a spec naming one waits forever for a row that will not come.
  fs.mkdirSync(path.join(dir, "reports/assets"), { recursive: true });
  fs.writeFileSync(path.join(dir, "reports/assets/app.js"), "console.log(1)\n");
  const pane = await openPane(page, dir);

  const menu = await openRowMenu(pane, "reports");
  await menu.getByRole("menuitem", { name: "Delete" }).click();

  const dialog = page.getByRole("alertdialog", { name: "Delete this folder?" });
  await expect(dialog).toContainText(/everything inside it/i);
  await dialog.getByRole("button", { name: "Delete" }).click();

  await expect(treeRow(pane, "reports")).toBeHidden();
  expect(fs.existsSync(path.join(dir, "reports"))).toBe(false);
});

/**
 * Deleting the file the viewer is showing must not leave an error on screen.
 *
 * The viewer would otherwise refetch a path that is now a 404 and report "this
 * file no longer exists" — technically true, and reads as a fault rather than as
 * the result of what the user just did.
 */
test("deleting the previewed file returns to the tree instead of erroring", async ({
  page,
}) => {
  const dir = workdir({ "doomed.md": "# Doomed\n" });
  const pane = await openPane(page, dir);

  await treeRow(pane, "doomed.md").click();
  await expect(pane.getByRole("region", { name: /preview of doomed\.md/i })).toBeVisible();

  // Back to the tree to reach the row menu, with the file still selected.
  await pane.getByRole("button", { name: "Files", exact: true }).click();
  const menu = await openRowMenu(pane, "doomed.md");
  await menu.getByRole("menuitem", { name: "Delete" }).click();
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Delete" })
    .click();

  await expect(pane.getByRole("tree")).toBeVisible();
  await expect(pane.getByRole("alert")).toBeHidden();
  await expect(treeRow(pane, "doomed.md")).toBeHidden();
});

/**
 * The upload path, driven through the real `<input type="file">`.
 *
 * `setInputFiles` on a hidden input is the supported way to do this and exercises
 * the same change handler a user's picker would, including the base64 encode.
 */
test("uploading writes the chosen file into the target folder", async ({ page }) => {
  const dir = workdir();
  fs.mkdirSync(path.join(dir, "inbox"));
  const pane = await openPane(page, dir);

  // The input is `hidden`, so it is addressed directly rather than clicked.
  await uploadInput(pane).setInputFiles({
    name: "brief.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("uploaded body\n"),
  });

  // With no row menu opened first, the destination is the header's default: the
  // working directory root.
  await expect(treeRow(pane, "brief.txt")).toBeVisible();
  expect(fs.readFileSync(path.join(dir, "brief.txt"), "utf8")).toBe("uploaded body\n");
});

test("an upload that would overwrite asks before replacing", async ({ page }) => {
  const dir = workdir({ "notes.txt": "original\n" });
  const pane = await openPane(page, dir);

  await uploadInput(pane).setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("replacement\n"),
  });

  const dialog = page.getByRole("alertdialog", { name: "Replace the existing file?" });
  await expect(dialog).toBeVisible();
  // Cancelling leaves the original bytes, which is the whole reason the backend
  // refuses to overwrite by default.
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect(fs.readFileSync(path.join(dir, "notes.txt"), "utf8")).toBe("original\n");

  await uploadInput(pane).setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("replacement\n"),
  });
  await page
    .getByRole("alertdialog")
    .getByRole("button", { name: "Replace" })
    .click();
  await expect(page.getByRole("alertdialog")).toBeHidden();
  expect(fs.readFileSync(path.join(dir, "notes.txt"), "utf8")).toBe("replacement\n");
});

/**
 * "Open in new tab" reaches the raw route, and the browser renders it.
 *
 * This is the feature's whole point, so the assertion is on what the *new tab*
 * received rather than on the click: a wrong URL, a missing cookie, or an
 * `attachment` disposition would all leave the click looking fine.
 */
test("Open in new tab serves the file's full contents from the raw route", async ({
  page,
  context,
}) => {
  // Longer than the pane's 256 KB text preview, so this file is exactly the case
  // the feature exists for: the tab must show what the preview cannot.
  const body = `HEAD\n${"line of content\n".repeat(20_000)}TAIL\n`;
  const dir = workdir({ "big.log": body });
  const pane = await openPane(page, dir);

  const menu = await openRowMenu(pane, "big.log");
  const [tab] = await Promise.all([
    context.waitForEvent("page"),
    menu.getByRole("menuitem", { name: "Open in new tab" }).click(),
  ]);

  await tab.waitForLoadState("domcontentloaded");
  expect(tab.url()).toContain("files/raw");
  // `TAIL` is past the preview limit, so its presence is what proves the route
  // serves the file whole.
  const text = await tab.locator("body").innerText();
  expect(text).toContain("TAIL");
  await tab.close();
});

test("the truncation notice offers the way to the whole file", async ({ page, context }) => {
  // Over 1 MB, not merely over the 256 KB cut. Truncation is decided by
  // `MAX_PREVIEW_BYTES` (1 MB) — a 400 KB file is returned whole and shows no
  // notice, which is what an earlier version of this fixture got wrong.
  const dir = workdir({ "huge.txt": "A".repeat(1_500_000) });
  const pane = await openPane(page, dir);
  await treeRow(pane, "huge.txt").click();

  // The notice names both numbers, so the reader can tell how much is missing.
  const notice = pane.getByText(/Showing the first .* of a .* file/);
  await expect(notice).toBeVisible();

  const [tab] = await Promise.all([
    context.waitForEvent("page"),
    pane.getByRole("button", { name: /Open the whole file in a new tab/ }).click(),
  ]);
  await tab.waitForLoadState("domcontentloaded");
  expect(tab.url()).toContain("files/raw");
  await tab.close();
});

/**
 * Right-clicking the tree's empty space acts on the working directory.
 *
 * The root is the one directory the tree never draws a row for, so without this it
 * is reachable only through the header buttons. A file browser lets you right-click
 * the background of a folder to act on that folder, and this pane is one.
 */
test("right-clicking empty space offers the working directory's actions", async ({
  page,
}) => {
  const dir = workdir({ "only.txt": "x\n" });
  const pane = await openPane(page, dir);

  // Low in the pane, below the single row: clicking the tree's centre would land
  // *on* a row, whose handler deliberately claims the gesture. The empty region
  // below the rows is the part that has to belong to the root, and it only does
  // because the tree container is `min-h-full`.
  const box = (await pane.boundingBox())!;
  await page.mouse.click(box.x + 24, box.y + box.height - 24, { button: "right" });
  const menu = page.getByRole("menu", { name: "Working directory actions" });
  await expect(menu).toBeVisible();

  await menu.getByRole("menuitem", { name: "New folder" }).click();
  const dialog = page.getByRole("alertdialog", { name: "New folder" });
  await dialog.getByRole("textbox").fill("from-background");
  await dialog.getByRole("button", { name: "Create" }).click();

  await expect(treeRow(pane, "from-background")).toBeVisible();
  expect(fs.statSync(path.join(dir, "from-background")).isDirectory()).toBe(true);
});

/**
 * A right-click on a row is that row's, not the root's.
 *
 * Both handlers are on nested elements, so without `stopPropagation` one gesture
 * opens two menus — and the one the user sees is whichever renders on top, acting
 * on the wrong directory.
 */
test("a right-click on a row opens only that row's menu", async ({ page }) => {
  const dir = workdir({ "target.txt": "x\n" });
  const pane = await openPane(page, dir);

  await treeRow(pane, "target.txt").click({ button: "right" });

  await expect(page.getByRole("menu", { name: "target.txt actions" })).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(1);
});

test("dropping files on a folder row uploads into that folder", async ({ page }) => {
  const dir = workdir();
  fs.mkdirSync(path.join(dir, "dropzone"));
  const pane = await openPane(page, dir);

  // A synthetic `DataTransfer`, which is the only way to drive a file drop from a
  // browser test: `dragAndDrop` moves elements, it cannot carry files.
  const handle = await page.evaluateHandle(() => {
    const transfer = new DataTransfer();
    transfer.items.add(new File(["dropped body\n"], "dropped.txt", { type: "text/plain" }));
    return transfer;
  });

  const row = treeRow(pane, "dropzone");
  await row.dispatchEvent("dragover", { dataTransfer: handle });
  await row.dispatchEvent("drop", { dataTransfer: handle });

  await expect(treeRow(pane, "dropzone/dropped.txt")).toBeVisible();
  expect(fs.readFileSync(path.join(dir, "dropzone/dropped.txt"), "utf8")).toBe(
    "dropped body\n",
  );
});

test("the delete and new-folder dialogs have no accessibility violations", async ({
  page,
}) => {
  const dir = workdir({ "audited.txt": "x\n" });
  const pane = await openPane(page, dir);

  const { default: AxeBuilder } = await import("@axe-core/playwright");
  const audit = async (context: string) => {
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
      .analyze();
    expect(
      results.violations.map((v) => ({ id: v.id, impact: v.impact })),
      `axe violations on ${context}`,
    ).toEqual([]);
  };

  const menu = await openRowMenu(pane, "audited.txt");
  await menu.getByRole("menuitem", { name: "Delete" }).click();
  await expect(page.getByRole("alertdialog")).toBeVisible();
  await audit("the delete confirmation");
  await page.keyboard.press("Escape");

  await pane.getByRole("button", { name: "New folder", exact: true }).click();
  await expect(page.getByRole("alertdialog")).toBeVisible();
  await audit("the new-folder prompt");
});
