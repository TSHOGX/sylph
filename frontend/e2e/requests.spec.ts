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
  await page.getByLabel("Message", { exact: true }).fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  const panel = page.getByTestId("approval-dialog");
  await expect(panel).toBeVisible({ timeout: 15000 });
  return panel;
}

/** Read the echoed answer through the same group/operation disclosures as a user. */
async function answerReceived(page: Page) {
  const sessionId=decodeURIComponent(new URL(page.url()).pathname.split("/").at(-1)!);
  const {openExecutionGroups}=await import("./fixtures/execution");
  await expect.poll(async()=>{
    const response=await page.request.get(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turn-state`);
    return (await response.json()).turns[0]?.turn.status;
  },{timeout:15000}).toMatch(/completed|failed/);
  const response=await page.request.get(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turn-state`);
  const turnId=(await response.json()).turns[0].turn.id;
  await openExecutionGroups(page,sessionId,turnId);
  const call=page.getByRole("region",{name:/tool call/}).last();
  await expect(call).toBeVisible();
  await call.locator("button[aria-expanded]").click();
  const result=page.getByRole("region",{name:"Tool result",exact:true}).last();
  await expect(result).toBeVisible();
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

  await expect(await answerReceived(page)).toContainText("Declined by the user.");
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

/**
 * Answering on one device retires the panel on another, with no reload.
 *
 * The reported bug, in the setup that produced it: a question answered on a phone
 * left the dialog standing on the desktop until the page was refreshed. The cause was
 * an omission — the server announced a request's *arrival* on the session stream and
 * announced its *departure* nowhere — so a second client only learned of the answer
 * when its 20-second backstop poll happened to fire. And that poll does not run in a
 * backgrounded tab (`refetchIntervalInBackground` is false by default) while
 * `refetchOnWindowFocus` is off, so returning to the tab did not refetch either:
 * reloading really was the only reliable way.
 *
 * Two browser *contexts*, not two tabs, because that is the honest model of a phone
 * and a laptop — separate cookie jars, separate streams, each bootstrapping its own
 * auth. Two tabs in one context would share a jar and could pass while a genuinely
 * separate client still hung.
 *
 * This is the assertion no unit test can make. The backend test proves the event is
 * published and the frontend test proves the merge rule retires the row; only a
 * second real browser proves the two are connected end to end.
 */
test("answering on one client retires the panel on another", async ({ browser }) => {
  const phone = await browser.newContext();
  const laptop = await browser.newContext();
  try {
    const phonePage = await phone.newPage();
    const sessionId = await createSession(phonePage);

    // The laptop opens the same session and is left sitting on it, exactly as a user
    // leaves a tab open before picking up their phone.
    const laptopPage = await laptop.newPage();
    await laptopPage.request.get("/api/v1/auth/token");
    await laptopPage.goto(`/sessions/${sessionId}`);
    await expect(laptopPage.getByLabel("Message", { exact: true })).toBeVisible();

    // Raised from the phone. Both clients are attached, so both must show it — the
    // arrival half, which already worked and is asserted so a failure below is
    // unambiguously about departure.
    const phonePanel = await ask(phonePage, "approval please");
    const laptopPanel = laptopPage.getByTestId("approval-dialog");
    await expect(laptopPanel).toBeVisible({ timeout: 15000 });

    await phonePanel.getByRole("button", { name: "Allow once", exact: true }).click();
    await expect(phonePanel).toBeHidden({ timeout: 15000 });

    // The assertion the bug fails. A generous timeout that is still far under the
    // 20-second poll: passing here means the stream delivered it, not that a
    // backstop refetch eventually did. Were this allowed 20s+ it would pass against
    // the bug on a foregrounded tab, which is the version of this test worth not
    // writing.
    await expect(laptopPanel).toBeHidden({ timeout: 10000 });

    // And the laptop is not left claiming the agent is still waiting, which is the
    // composer's own summary of the same state — one surface retiring while the other
    // insists would be a half fix.
    await expect(
      laptopPage.getByText("The agent is waiting on you."),
    ).toBeHidden({ timeout: 10000 });
  } finally {
    await phone.close();
    await laptop.close();
  }
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
