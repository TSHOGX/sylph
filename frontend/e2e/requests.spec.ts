import { expect, test, type Page } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * Answering what an agent is blocked on.
 *
 * This file exists because the whole request surface shipped with no end-to-end
 * coverage at all, and a real bug reached a user through that gap: an agent's
 * question rendered its options correctly, the panel closed on a click, the turn
 * resumed — and the answer had been dropped on the way to the agent. Every layer
 * looked like it was working.
 *
 * So the assertions here are about what the *agent received*, not about the panel
 * closing. The fixture echoes the resolve payload into its tool result for exactly
 * that reason: "the dialog went away" and "the answer arrived" are different claims,
 * and only the second one is the feature.
 */

/** Raise a request by prompt substring, and wait for its panel. */
async function ask(page: Page, prompt: string) {
  await page.getByLabel("Message").fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const panel = page.getByTestId("approval-dialog");
  await expect(panel).toBeVisible({ timeout: 15000 });
  return panel;
}

/**
 * The tool result the fixture echoed the answer into, expanded.
 *
 * A result is collapsed to one line by default, so its body sits behind a `more`
 * toggle. Expanding is part of reading it rather than a workaround: what these specs
 * assert is the payload the agent received, and that only exists in the expanded body.
 */
async function answerReceived(page: Page) {
  const result = page.getByRole("region", { name: "Tool result" });
  await expect(result).toBeVisible({ timeout: 15000 });
  const more = result.getByRole("button", { name: "more" });
  if (await more.isVisible().catch(() => false)) await more.click();
  return result;
}

test("a pick, a note, and a multi-select all reach the agent", async ({ page }) => {
  await createSession(page);
  const panel = await ask(page, "question please");

  // Both questions are on one panel. Forwarding only the first is what makes the
  // model report a partial answer, so the second field existing is the assertion.
  await expect(panel.getByRole("group", { name: "Indentation?" })).toBeVisible();
  await expect(panel.getByRole("group", { name: "Linters?" })).toBeVisible();

  // The agent's own descriptions are what make its options readable; a picker that
  // shows only labels loses most of the question.
  await expect(panel.getByText("Indent with tabs.")).toBeVisible();

  // `Linters?` is required, so sending is withheld until it is answered — and said
  // rather than left to be inferred from a greyed button.
  const send = panel.getByRole("button", { name: "Send answer" });
  await expect(send).toBeDisabled();
  await expect(panel.getByText("Answer the required questions to send.")).toBeVisible();

  await panel.getByRole("radio", { name: "Tabs" }).check();
  await panel.getByRole("checkbox", { name: "ESLint" }).check();
  await panel.getByRole("checkbox", { name: "Clippy" }).check();
  await panel.getByLabel("Notes (optional)").fill("only for generated files");

  await expect(send).toBeEnabled();
  await send.click();

  // The payoff. A single-select answers with a string and a multi-select with an
  // array — the array is not flattened on the wire, because joining is a vendor
  // detail that belongs in an adapter.
  const result = await answerReceived(page);
  await expect(result).toContainText('"q:0": "Tabs"');
  await expect(result).toContainText('"q:1": ["ESLint", "Clippy"]');
  await expect(result).toContainText('"notes": {"q:0": "only for generated files"}');
  await expect(page.getByTestId("approval-dialog")).toBeHidden();
});

test("an answer the agent never offered is sent as the answer, not as a note", async ({
  page,
}) => {
  await createSession(page);
  const panel = await ask(page, "question please");

  // The terminal's "Other" row. Its text replaces the pick rather than riding
  // alongside it: conflating the two loses which the user meant.
  await panel.getByRole("radio", { name: "Something else" }).check();
  await panel.getByLabel("Type your answer").fill("Tabs in Makefiles only");
  await panel.getByRole("checkbox", { name: "Prettier" }).check();
  await panel.getByRole("button", { name: "Send answer" }).click();

  const result = await answerReceived(page);
  await expect(result).toContainText('"q:0": "Tabs in Makefiles only"');
  await expect(result).not.toContainText("notes");
});

test("declining sends no answers, even while a required field is empty", async ({
  page,
}) => {
  await createSession(page);
  const panel = await ask(page, "question please");

  // A required field must not become a trap: saying "I would rather not answer" has
  // to stay available regardless of what is filled in.
  const decline = panel.getByRole("button", { name: "Don't answer" });
  await expect(decline).toBeEnabled();
  await decline.click();

  await expect(page.getByText("Declined by the user.")).toBeVisible({ timeout: 15000 });
});

test("a text request carries its prefill and submits what was typed", async ({
  page,
}) => {
  await createSession(page);
  const panel = await ask(page, "elicit please");

  // An `elicitation` is free text by definition. Before a request could declare a
  // form this rendered as a read-only dump of its own placeholder — there was
  // literally nowhere to type.
  const field = panel.getByLabel("Summary");
  await expect(field).toHaveValue("Refactor the ");
  await field.fill("Refactor the parser into two passes.");
  await panel.getByRole("button", { name: "Submit" }).click();

  await expect(await answerReceived(page)).toContainText(
    "Refactor the parser into two passes.",
  );
});

test("dismissing a request says the agent is still waiting", async ({ page }) => {
  await createSession(page);
  const panel = await ask(page, "question please");

  // Escape hides the panel without answering, so the request stays open and the
  // agent stays blocked. With one request pending, nothing else on screen used to
  // say so — a blocked session that merely looks idle leaves the user waiting for an
  // agent that is waiting for them.
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();

  await expect(page.getByText("The agent is waiting on you.")).toBeVisible();
  await page.getByRole("button", { name: "Show request" }).click();

  // Re-opening restores the panel, and the answer still gets through.
  const reopened = page.getByTestId("approval-dialog");
  await expect(reopened).toBeVisible();
  await reopened.getByRole("radio", { name: "Tabs" }).check();
  await reopened.getByRole("checkbox", { name: "ESLint" }).check();
  await reopened.getByRole("button", { name: "Send answer" }).click();

  await expect(await answerReceived(page)).toContainText('"q:0": "Tabs"');
  await expect(page.getByText("The agent is waiting on you.")).toBeHidden();
});

test("a form is answerable by keyboard alone", async ({ page }) => {
  await createSession(page);
  const panel = await ask(page, "question please");

  // The first control takes focus, not the deny row: there is nothing to grant by
  // pressing Enter on an unanswered form, and starting on deny would cost a keystroke
  // on the common path. A native radio in a named group also gets arrow-key
  // navigation from the platform, which is the reason these are real inputs rather
  // than divs wearing ARIA.
  await expect(panel.getByRole("radio", { name: "Tabs" })).toBeFocused();
  await page.keyboard.press("Space");
  await expect(panel.getByRole("radio", { name: "Tabs" })).toBeChecked();

  await page.keyboard.press("ArrowDown");
  await expect(panel.getByRole("radio", { name: "Spaces" })).toBeChecked();
});
