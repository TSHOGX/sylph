import { expect, test } from "./fixtures/test";
import {
  agentLink,
  CHILD_AGENT,
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
} from "./fixtures/trace";

test("a fork origin opens an independent reader and Back preserves the fork draft", async ({
  page,
}) => {
  const state = await installTraceFixture(page, [
    message(0, "Fork-owned conversation"),
  ]);
  state.mainHeader.agent = agentLink(MAIN_AGENT, "Fork reader");
  state.mainHeader.origin = agentLink(CHILD_AGENT, "Original reader");
  state.mainHeader.parent = null;
  state.childHeader.agent = agentLink(CHILD_AGENT, "Original reader");
  state.childHeader.parent = null;
  state.childRows = [message(1, "Original retained conversation", CHILD_AGENT)];
  await openTrace(page);
  await page
    .getByLabel("Unsent draft", { exact: true })
    .fill("Draft belongs to the fork");
  await expect(page.getByRole("link", { name: /^Parent:/ })).toHaveCount(0);
  await page
    .getByRole("link", { name: "Origin: Original reader", exact: true })
    .click();
  await expect(
    page.getByRole("heading", { name: "Original reader", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Original retained conversation", { exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
    "",
  );
  await page.goBack();
  await expect(
    page.getByRole("heading", { name: "Fork reader", exact: true }),
  ).toBeVisible();
  await expect(page.getByLabel("Unsent draft", { exact: true })).toHaveValue(
    "Draft belongs to the fork",
  );
  expect(state.commands).toHaveLength(0);
});
