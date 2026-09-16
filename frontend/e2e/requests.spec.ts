import { expect, test } from "./fixtures/test";
import {
  openRequest,
  questionRequest,
  requestFixture,
} from "./fixtures/trace-requests";
import { openTrace } from "./fixtures/trace";

test("a pick, note, and multi-select keep their typed values in the approval command", async ({
  page,
}) => {
  const state = await requestFixture(page);
  const panel = await openRequest(page);
  await expect(panel.getByText("Indent with tabs.")).toBeVisible();
  await expect(
    panel.getByRole("button", { name: "Send answer" }),
  ).toBeDisabled();
  await panel.getByRole("radio", { name: "Tabs" }).check();
  await panel.getByRole("checkbox", { name: "ESLint" }).check();
  await panel.getByRole("checkbox", { name: "Clippy" }).check();
  await panel.getByLabel("Notes (optional)").fill("only generated files");
  await panel.getByRole("button", { name: "Send answer" }).click();
  await expect
    .poll(() => state.commands[0]?.answer)
    .toEqual({
      requestId: "question-request",
      optionId: "submit",
      input: {
        values: { "q:0": "Tabs", "q:1": ["ESLint", "Clippy"] },
        notes: { "q:0": "only generated files" },
      },
    });
  await expect(panel).toHaveCount(0);
});

test("an explicitly offered other answer is the value rather than a note", async ({
  page,
}) => {
  const state = await requestFixture(page);
  const panel = await openRequest(page);
  await panel.getByRole("radio", { name: "Something else" }).check();
  await panel.getByLabel("Type your answer").fill("Tabs in Makefiles only");
  await panel.getByRole("checkbox", { name: "Prettier" }).check();
  await panel.getByRole("button", { name: "Send answer" }).click();
  await expect
    .poll(() => state.commands[0]?.answer?.input)
    .toMatchObject({
      values: { "q:0": "Tabs in Makefiles only", "q:1": ["Prettier"] },
    });
});

test("declining forwards no unanswered required fields", async ({ page }) => {
  const state = await requestFixture(page);
  const panel = await openRequest(page);
  await panel.getByRole("button", { name: "Don't answer" }).click();
  await expect
    .poll(() => state.commands[0]?.answer)
    .toEqual({ requestId: "question-request", optionId: "deny", input: null });
});

test("a text request keeps its prefill and submits the edited text", async ({
  page,
}) => {
  const request = questionRequest();
  request.title = "Describe the change";
  request.kind = "elicitation";
  request.form = {
    fields: [
      {
        type: "text",
        id: "text",
        label: "Change",
        prefill: "Keep this draft",
        required: true,
      },
    ],
  };
  const state = await requestFixture(page, request);
  const panel = await openRequest(page);
  await expect(panel.getByLabel("Change", { exact: true })).toHaveValue(
    "Keep this draft",
  );
  await panel.getByLabel("Change", { exact: true }).fill("The edited draft");
  await panel.getByRole("button", { name: "Send answer" }).click();
  await expect
    .poll(() => state.commands[0]?.answer?.input)
    .toEqual({ values: { text: "The edited draft" } });
});

test("a background question waits for explicit review and dismissal does not answer it", async ({
  page,
}) => {
  const state = await requestFixture(page);
  await openTrace(page);
  const editor = page.getByLabel("Message", { exact: true });
  await editor.fill("My separate draft");
  await editor.focus();
  await expect(page.getByTestId("approval-dialog")).toHaveCount(0);
  await expect(editor).toBeFocused();
  await page.getByRole("button", { name: "Show request", exact: true }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("approval-dialog")).toHaveCount(0);
  expect(state.commands).toHaveLength(0);
  await expect(editor).toHaveValue("My separate draft");
  await expect(
    page.getByRole("button", { name: "Show request", exact: true }),
  ).toBeVisible();
});

test("one client's resolution retires another observer's answer controls", async ({
  browser,
}) => {
  const first = await browser.newPage(),
    second = await browser.newPage();
  try {
    const state = await requestFixture(first),
      other = await requestFixture(second);
    other.requests = state.requests;
    const firstPanel = await openRequest(first),
      secondPanel = await openRequest(second);
    await firstPanel.getByRole("button", { name: "Don't answer" }).click();
    await second
      .getByRole("button", { name: "Refresh controls", exact: true })
      .click({ force: true });
    await expect(secondPanel).toHaveCount(0);
    expect(other.commands).toHaveLength(0);
  } finally {
    await first.close();
    await second.close();
  }
});

test("a declared choice form is operable through the keyboard", async ({
  page,
}) => {
  await requestFixture(page);
  const panel = await openRequest(page);
  await expect(panel.getByRole("radio", { name: "Tabs" })).toBeFocused();
  await page.keyboard.press("Space");
  await expect(panel.getByRole("radio", { name: "Tabs" })).toBeChecked();
  await page.keyboard.press("ArrowDown");
  await expect(panel.getByRole("radio", { name: "Spaces" })).toBeChecked();
});
