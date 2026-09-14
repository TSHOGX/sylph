import { expect, test } from "@playwright/test";
import { gotoReady } from "./fixtures/session";

/**
 * Choosing *where* work happens: the workspace list and the folder browser.
 *
 * # Why this file exists
 *
 * `select-list.spec.ts` covers a long choice list in the composer — the model
 * catalogue. This covers the same engine on the surfaces that answer "which
 * directory", which is a different question with two failure modes of its own.
 *
 * Both were invisible to the suite before. `Mod+N` opened a raw filesystem browse
 * with no filter and no keyboard path at all, so every spec that used it clicked a
 * confirm button at the landing directory and never navigated — a picker that could
 * not be searched or arrowed through passed everything. And the filterable
 * workspace list existed only inside the history browser, so no spec could observe
 * that the two most common ways of naming a directory did not reach it.
 *
 * # What each spec pins
 *
 * The invariants, not the mechanisms: a directory is reachable by typing part of
 * its path, the keyboard can navigate and commit without the mouse, and the two
 * surfaces are one modal rather than two stacked focus traps. Asserting an exact
 * panel height or a row count would fail on the next padding change while saying
 * nothing about whether the picker is usable.
 *
 * The exception is the *last* spec here, and the distinction is worth stating
 * because it looks like the thing this comment forbids. It names no height at all;
 * it compares the surface against *itself* across a content change, which is what
 * makes it survive a padding edit while still pinning the defect. A panel sized by
 * its content is the bug, so "the same dialog before and after the list empties" is
 * the invariant, and any particular number would be the incidental part.
 *
 * It needed that shape to fail at all. Asserting the list is merely tall enough for
 * one row passed against the broken code, because the e2e browse root is small
 * enough that a content-sized panel is still taller than a row — a reminder that a
 * fixture's size is part of what an assertion can observe. Every other spec in this
 * file also passed while the list was 8px tall holding 2248px of rows, and
 * `toBeVisible` did not catch it either: the rows were in the DOM with non-zero
 * boxes of their own, overflowing a container that had collapsed.
 *
 * Deliberately no assertion on *which* workspaces are listed. `frontend/.e2e-data`
 * accumulates every session from every run and is never wiped, so any count is a
 * time bomb — the documented pitfall that has already bitten twice. These assert on
 * identity and on behaviour that holds at any list length, including zero.
 */

/** Opens the workspace picker. `Mod+Shift+O` is the `newSession` binding. */
async function openNewSession(page: import("@playwright/test").Page) {
  // `gotoReady`, not `goto`: a shortcut pressed before the shell's keydown effect
  // has run is discarded, which is how a sibling spec failed 1 run in 4.
  await gotoReady(page);
  await page.keyboard.press("ControlOrMeta+Shift+o");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  return dialog;
}

test("the new-session picker opens on workspaces with its filter focused", async ({
  page,
}) => {
  const dialog = await openNewSession(page);

  // A combobox, not a menu: the list is filterable, and `role="menu"` may only
  // contain menu items — an `<input>` inside one is invalid ARIA that axe reports.
  const filter = dialog.getByRole("combobox", { name: /filter workspaces/i });

  // Focused on open, so the first keystroke filters rather than being swallowed.
  // This is the failure the model catalogue shipped with: `autoFocus` fires at
  // mount, while an anchored surface is still unfocusable.
  await expect(filter).toBeFocused();

  // The placeholder names the *path*, because the visible column is a leaf name and
  // nothing else tells the user a middle segment will match.
  await expect(filter).toHaveAttribute("placeholder", /path/i);
});

test("the folder browser filters the current level and keeps one focus trap", async ({
  page,
}) => {
  const dialog = await openNewSession(page);

  // Replaces the workspace list rather than stacking on it. Two `Overlay`s on one
  // DOM path put two focus traps there, and Tab at the boundary becomes
  // unpredictable — so "browse" is a mode swap, and Back is a state change.
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  await expect(dialog.getByRole("listbox", { name: "Folders" })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(1);

  const filter = dialog.getByRole("combobox", { name: /filter folders/i });
  await expect(filter).toBeFocused();

  // `SYLPH_WEB_BROWSE_ROOTS` is `/tmp`, so the browser lands there. A filter that
  // matches nothing must empty the list rather than fall back to showing all of
  // it — a filter that silently ignores its query is worse than none.
  await filter.fill("zzz-no-such-folder-zzz");
  await expect(dialog.getByRole("option")).toHaveCount(0);

  // And clearing it restores the level, so the field is non-destructive.
  await filter.clear();
  await expect(dialog.getByRole("option").first()).toBeVisible();

  // One trap throughout: Escape closes the whole thing, not just this layer.
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
});

test("typing a path navigates there, which is the only way to reach an unlisted folder", async ({
  page,
}) => {
  const dialog = await openNewSession(page);
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  await expect(dialog.getByRole("listbox", { name: "Folders" })).toBeVisible();

  // A path-shaped query is a navigation target rather than a filter. Offered beside
  // the field rather than as a row, because a row would be ranked against children
  // it has nothing to do with — and dropped by the same query that produced it
  // whenever no child happened to match.
  const filter = dialog.getByRole("combobox", { name: /filter folders/i });
  await filter.fill("/tmp");
  const jump = dialog.getByRole("button", { name: /^Go to/ });
  await expect(jump).toBeVisible();

  // Enter commits it, so a pasted path costs no mouse. The crumb trail is the proof
  // it landed: it is rebuilt from the directory the server answered with.
  await filter.press("Enter");
  await expect(dialog.getByRole("navigation", { name: "Path" })).toContainText("tmp");

  // And the query is cleared, or the new listing would be filtered by the path that
  // reached it — an empty list at a directory full of children.
  await expect(filter).toHaveValue("");
});

test("the keyboard can open a folder and choose one without the mouse", async ({
  page,
}) => {
  const dialog = await openNewSession(page);
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  const list = dialog.getByRole("listbox", { name: "Folders" });
  await expect(list).toBeVisible();

  const filter = dialog.getByRole("combobox", { name: /filter folders/i });

  // The cursor rides `aria-activedescendant` while focus stays on the field, which
  // is the one arrangement where typing and navigating do not fight over focus.
  await filter.press("ArrowDown");
  await expect(filter).toHaveAttribute("aria-activedescendant", /.+/);

  // Enter *opens* the highlighted folder rather than choosing it: a click has always
  // navigated, and one gesture meaning both is the ambiguity that makes
  // double-click pickers feel unpredictable. The crumb trail gains a level.
  const crumbs = dialog.getByRole("navigation", { name: "Path" });
  const before = await crumbs.getByRole("button").count();
  await filter.press("Enter");
  await expect(crumbs.getByRole("button")).toHaveCount(before + 1);

  // Which left the keyboard no way to *choose*, so the modifier does it outright.
  // Creation navigates to the session, so the composer is the signal it worked.
  await filter.press("ControlOrMeta+Enter");
  await expect(page.getByLabel("Message", { exact: true })).toBeVisible();
});

test("the folder browser's height is the surface's, not its current directory's", async ({
  page,
}) => {
  const dialog = await openNewSession(page);
  await dialog.getByRole("button", { name: "Browse folders..." }).click();
  const list = dialog.getByRole("listbox", { name: "Folders" });
  await expect(list).toBeVisible();
  await expect(dialog.getByRole("option").first()).toBeVisible();

  const filter = dialog.getByRole("combobox", { name: /filter folders/i });

  // Located by id, not by role: `SelectScroll` claims `listbox` only while there are
  // options for it to contain, because a `listbox` around a "no matches" line is the
  // `aria-required-children` failure that pattern exists to avoid. The empty state
  // below is exactly that case, so a role locator would stop resolving mid-test —
  // which is a different failure from the one this spec is about.
  const listId = await filter.getAttribute("aria-controls");
  const scroller = dialog.locator(`#${listId}`);

  const heights = async () => ({
    dialog: (await dialog.boundingBox())!.height,
    list: await scroller.evaluate((el) => el.clientHeight),
  });

  const listed = await heights();

  // A query matching nothing empties the list, which is the largest content change
  // this surface has: every row goes away and one line of text replaces them.
  await filter.fill("zzz-no-such-folder-zzz");
  await expect(dialog.getByRole("option")).toHaveCount(0);
  const empty = await heights();

  await filter.clear();
  await expect(dialog.getByRole("option").first()).toBeVisible();
  const restored = await heights();

  // The dialog must not have resized. This is the invariant the height chain broke:
  // the panel was `max-h-` only, so it was content-sized and its height was a
  // function of how many directories the current level happened to hold — measured
  // swinging between 211px and 756px on one viewport as the user navigated and
  // filtered. A picker that changes size under the pointer is the visible symptom;
  // the invisible one is that `flex-1` on the list then has no free space to
  // distribute, so on a short viewport the list resolved to 8px while holding
  // 2248px of rows.
  //
  // `Overlay` now owns a determinate height for the `picker` variant. This assertion
  // goes red if that reverts to a cap, or if a child re-declares a height of its own.
  expect(empty.dialog).toBe(listed.dialog);
  expect(restored.dialog).toBe(listed.dialog);

  // And the list keeps its box, so the rows scroll inside a stable frame rather than
  // the frame growing to fit them.
  expect(empty.list).toBe(listed.list);
  expect(restored.list).toBe(listed.list);

  // The scroll container is the list itself. Asserted because `fill` mode takes its
  // height from the surface and owns only the scrolling — if that moved to an
  // ancestor, a long level would scroll the whole dialog including its footer.
  expect(await scroller.evaluate((el) => getComputedStyle(el).overflowY)).toBe("auto");
  expect(listed.list).toBeGreaterThan(0);
});
