import { expect, test } from "./fixtures/test";
import {
  installTraceFixture,
  MAIN_AGENT,
  message,
  openTrace,
} from "./fixtures/trace";

for (const width of [1280, 390]) {
  test(`inherited-only context remains readable and uses live controls independently at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const copied = message(0, "Retained inherited answer");
    copied.entryContext = { kind: "inherited", originalOwner: null };
    const fixture = await installTraceFixture(page, [copied]);
    fixture.mainHeader.inheritedContextOnly = true;
    fixture.control = {
      kind: "available",
      target: {
        kind: "session",
        agent: MAIN_AGENT,
        sessionId: "fixture-session",
      },
      actions: ["send"],
      lease: "lease",
      affectedExecution: null,
    };
    await openTrace(page);
    await expect(
      page.getByText(/Only inherited context observed/),
    ).toBeVisible();
    const status = page.getByText(/Only inherited context observed/);
    await expect(
      status.evaluate((node) => node.scrollWidth <= node.clientWidth),
    ).resolves.toBe(true);
    await expect(
      page.getByText("Retained inherited answer", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Inherited context · Origin unavailable", { exact: true }),
    ).toBeVisible();
    await page.screenshot({
      path: `/tmp/sylph-inherited-summary-${width}.png`,
    });
    await page.getByLabel("Message", { exact: true }).fill("Start local work");
    await page.getByRole("button", { name: "Send", exact: true }).click();
    await expect.poll(() => fixture.commands.length).toBe(1);
    expect(fixture.commands[0]).toMatchObject({
      action: "send",
      text: "Start local work",
      target: fixture.control.target,
    });
    fixture.mainHeader.inheritedContextOnly = false;
    fixture.rows.push(message(1, "First local response"));
    fixture.revision++;
    await page.reload();
    await expect(
      page.getByText("First local response", { exact: true }),
    ).toBeVisible();
    await expect(page.getByText(/Only inherited context observed/)).toHaveCount(
      0,
    );
    await expect(
      page.getByText("Retained inherited answer", { exact: true }),
    ).toBeVisible();
  });
}
