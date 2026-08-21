import { expect, test, type Page } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * The agent-first timeline: tool calls as primary content, inline diffs, and
 * expansion policy that follows what a reader needs.
 *
 * These assertions encode design decisions from `docs/agent-ui-design-analysis.md`
 * rather than incidental markup, so a failure means the canvas stopped behaving
 * like an agent workspace — not that a class name changed.
 */

async function runPrompt(page: Page, prompt: string) {
  await createSession(page);
  await page.getByLabel("Message").fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
}

test("tool calls render as work, with target, status, and no bubbles", async ({ page }) => {
  await runPrompt(page, "exercise tools please");

  // Each call is a labelled region, which is what lets a screen reader jump
  // between operations rather than reading one undifferentiated stream.
  await expect(page.getByRole("region", { name: "Read tool call" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Edit tool call" })).toBeVisible();
  const bash = page.getByRole("region", { name: "Bash tool call" });
  await expect(bash).toBeVisible();

  // The target is shown, not just the tool name: "Read" alone says nothing about
  // what was read.
  await expect(page.getByRole("region", { name: "Read tool call" })).toContainText("src/lib.rs");
  await expect(bash).toContainText("cargo test --lib");

  // A failure is announced in text, not by colour alone (WCAG 1.4.1), and carries
  // the exit code because that is the first thing anyone wants from a failed run.
  await expect(bash).toContainText("failed");
  await expect(bash).toContainText("exit 101");
});

test("an edit renders as an inline diff, and a failed command auto-expands", async ({
  page,
}) => {
  await runPrompt(page, "exercise tools please");

  // The diff is computed in the browser from the edit's before/after strings, so
  // both sides must appear with their change markers.
  const edit = page.getByRole("region", { name: "Edit tool call" });
  await expect(edit).toContainText('println!("before");');
  await expect(edit).toContainText('tracing::info!("after");');
  // One line replaced: one addition, one removal.
  await expect(edit).toContainText("+1");
  await expect(edit).toContainText("−1");
  // The unchanged surrounding lines are context, so they are shown too.
  await expect(edit).toContainText("fn main() {");

  // A failing command's output is the answer to "what went wrong", so it is
  // visible without interaction.
  const bash = page.getByRole("region", { name: "Bash tool call" });
  await expect(bash).toContainText("cannot find value `after`");

  // A successful read is context-gathering, so its output stays collapsed.
  const read = page.getByRole("region", { name: "Read tool call" });
  await expect(read).not.toContainText('println!("before")');
  await read.getByRole("button", { name: "more" }).click();
  await expect(read).toContainText('println!("before")');
});

test("assistant prose renders Markdown without chat bubbles", async ({ page }) => {
  await runPrompt(page, "exercise tools please");

  // `**real**` in the runner's reply must render as emphasis, not literal
  // asterisks — the Markdown path is what makes agent prose readable.
  await expect(page.locator("strong", { hasText: "real" })).toBeVisible();
});

/**
 * Render cost for a long conversation.
 *
 * The Phase 6 exit criterion is a 1000-message conversation rendering in under
 * two seconds. The bulk prompt journals 1000 events (250 iterations × 4), and the
 * measurement is taken on a reload so it covers a cold render of durable history
 * rather than incremental streaming — the slower and more meaningful direction.
 *
 * The budget here is deliberately the spec's 2s and not a tighter number: this
 * runs on whatever CI machine is available, and a threshold tuned to a fast
 * laptop would fail for reasons unrelated to the code.
 */
test("a 1000-event conversation renders within the performance budget", async ({ page }) => {
  test.slow(); // Journalling 1000 events through the supervisor is not instant.

  await runPrompt(page, "bulk render please");

  // Confirm the fixture really produced the volume the budget is about, so the
  // measurement cannot silently pass against a short transcript.
  //
  // Polled rather than counted once: the transcript now paints incrementally, so
  // the reply block that `runPrompt` waits for appears while rows are still
  // arriving. A single count here races the stream and reads a partial log.
  await expect
    .poll(
      () => page.getByRole("region", { name: /tool call/ }).count(),
      { message: "bulk fixture should emit 250 tool calls", timeout: 30_000 },
    )
    .toBeGreaterThanOrEqual(250);

  const started = Date.now();
  await page.reload();
  // The last emitted block is the reply, so seeing it means the whole transcript
  // has been laid out.
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  await expect(page.getByText("Step 250:")).toBeVisible();
  const elapsed = Date.now() - started;

  expect(
    elapsed,
    `cold render of a 1000-event conversation took ${elapsed}ms`,
  ).toBeLessThan(2000);
});
