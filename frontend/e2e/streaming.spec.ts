import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * Incremental streaming: does the transcript paint *during* a turn?
 *
 * This file exists because every other spec passed while the app streamed
 * nothing at all. They assert on final state, and the mock runner finishes in
 * well under a second, so "rendered progressively" and "appeared all at once at
 * the end" are indistinguishable to them. A client that dropped every mid-turn
 * event shipped through a green suite.
 *
 * The `paced` scenario spaces its stages a second apart, which makes the
 * distinction observable: each assertion below runs while the runner is still
 * working, and the turn cannot have completed yet because later stages have not
 * been emitted. Asserting an *earlier* stage is visible while a *later* one is
 * not is the part that fails on a non-streaming client.
 */
test("the transcript paints thinking, tools, and prose while the turn is still running", async ({
  page,
}) => {
  await createSession(page);

  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("paced please");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  // Stage 1: the first reasoning delta. It arrives immediately, ~4s before the
  // turn ends, so seeing it at all proves mid-turn delivery. Thinking is
  // expanded while streaming, which is why its text is visible rather than
  // merely present.
  await expect(page.getByText("Reasoning step one.")).toBeVisible({ timeout: 8000 });

  // The reply is the last thing the runner emits. Its absence here is what makes
  // the assertion above meaningful: the turn is genuinely still in flight.
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();

  // Stage 2: the tool call, still ahead of the reply.
  const read = page.getByRole("region", { name: "Read tool call" });
  await expect(read).toBeVisible({ timeout: 8000 });
  await expect(read).toContainText("src/paced.rs");
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();

  // Stage 3: a second reasoning block. The user's ask was explicitly that
  // multi-round reasoning stream, not only the first block. This one arrives
  // already `completed`, and finished thinking collapses by design — so the
  // assertion is that its row exists mid-turn, not that its text is on screen.
  // Asserting visibility here would be asserting the wrong product behaviour.
  const thinkingRows = page.getByText("Thinking", { exact: false });
  await expect(thinkingRows).toHaveCount(3, { timeout: 8000 });
  await expect(page.getByText("Second round of reasoning.")).toBeAttached();
  await expect(page.getByText("Hello from the E2E runner.")).toBeHidden();

  // Stage 4: the turn settles, and the streamed blocks survive into history
  // rather than being replaced by a journal refetch that drops them.
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible({ timeout: 8000 });
  await expect(read).toBeVisible();
});

/**
 * The live indicator must reflect events actually arriving.
 *
 * "Streaming" is shown only once at least one event has been received, whereas
 * "Waiting for the agent" is what an open-but-silent connection shows. Asserting
 * the former specifically is what makes this test meaningful: a client that
 * connects successfully and then drops every frame sits on "Waiting" forever, so
 * accepting either label would pass against the very bug this covers.
 */
test("the live turn reports Streaming once events arrive", async ({ page }) => {
  await createSession(page);

  const composer = page.getByLabel("Message", { exact: true });
  await composer.fill("paced please");
  await page.getByRole("button", { name: "Send", exact: true }).click();

  const live = page.getByRole("region", { name: "Current turn" });
  await expect(live).toContainText("Streaming", { timeout: 8000 });

  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible({ timeout: 10000 });
});
