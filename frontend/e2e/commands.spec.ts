/**
 * The session's command surface: the menu, and the two kinds of command.
 *
 * Both specs here exist because of a bug that reached real use, and neither symptom
 * was observable to the suite as it stood.
 *
 * **The menu stopped opening after the first exchange.** The cause was three layers
 * from the symptom: `claude` emits its `system/init` once per *turn*, the adapter
 * turned that into a capability update carrying only the permission mode, and the
 * server applied it by replacing the whole surface — so the first turn of every
 * session erased the project's 46 slash commands. Every existing spec typed `/` on a
 * fresh session, so none of them could see it.
 *
 * **`/model` answered with a paragraph.** Sent as prompt text to a headless CLI, a
 * request to pick a model can only come back as prose, because there is no picker on
 * that side of the wire. So a control command must resolve against typed session
 * config instead of becoming a turn.
 */

import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

test("the slash menu still opens after a completed turn", async ({ page }) => {
  await createSession(page);

  const composer = page.getByLabel("Message");
  await composer.fill("/");
  // The fixture reports four commands, so the listbox is the agent's own answer to
  // "what can I be asked to do" rather than anything this UI holds.
  await expect(page.getByRole("listbox", { name: "Agent commands" })).toBeVisible();
  await expect(page.getByRole("option", { name: /verify/ })).toBeVisible();

  await composer.fill("hello");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();

  // The assertion the reported bug would have failed. The fixture emits a per-turn
  // capability update naming only `permissionMode`, exactly as `claude` does, so
  // this drives the real path rather than a hypothetical one.
  await composer.fill("/");
  await expect(page.getByRole("listbox", { name: "Agent commands" })).toBeVisible();
  await expect(page.getByRole("option", { name: /verify/ })).toBeVisible();
});

test("a control command opens a picker instead of sending a message", async ({ page }) => {
  await createSession(page);

  const composer = page.getByLabel("Message");
  await composer.fill("/model");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // A dialog, not a turn. The agent's own options, with the current value checked —
  // nothing here is named by the frontend.
  const picker = page.getByRole("dialog", { name: "Model" });
  await expect(picker).toBeVisible();
  await expect(picker.getByRole("menuitemradio", { name: "Fast" })).toBeChecked();

  await picker.getByRole("menuitemradio", { name: "Thorough" }).click();

  // The row reflects what the agent reported back, so the picker is not merely
  // cosmetic: the response *is* the refreshed surface.
  await expect(page.getByRole("button", { name: /^Model:/ })).toHaveText(/Thorough/);

  // And nothing was sent. This is the whole point of the distinction: `/model` as
  // prompt text is what produced a paragraph about models in the transcript.
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();
});
