import { expect, test } from "./fixtures/test";
import { installTraceFixture, MAIN_AGENT, openTrace } from "./fixtures/trace";

function available(
  actions: Array<"send" | "stop" | "steer" | "queue">,
  lease = "lease-one",
  execution: string | null = "execution-one",
) {
  return {
    kind: "available" as const,
    target: {
      kind: "session" as const,
      agent: MAIN_AGENT,
      sessionId: "fixture-session",
    },
    actions,
    lease,
    affectedExecution: execution,
  };
}

test("a stop cannot silently move from the displayed execution to its successor", async ({
  page,
}) => {
  const state = await installTraceFixture(page);
  state.control = available(["stop"]);
  await openTrace(page);
  const stop = page.getByRole("button", { name: /^Stop/ });
  await expect(stop).toBeVisible();
  state.control = available(["stop"], "lease-two", "execution-two");
  await stop.click();
  await expect(page.getByText(/The targeted execution changed/)).toBeVisible();
  expect(state.commands).toHaveLength(0);
});

test("durable acceptance clears only the submitted draft revision", async ({
  page,
}) => {
  const state = await installTraceFixture(page);
  state.control = available(["send"], "lease", null);
  await openTrace(page);
  let submitted: { text: string } | null = null;
  let accept!: () => void;
  const accepted = new Promise<void>((resolve) => {
    accept = resolve;
  });
  await page.route("**/api/v1/trace/commands", async (route) => {
    submitted = route.request().postDataJSON();
    await accepted;
    return route.fulfill({
      status: 202,
      json: {
        commandId: "accepted-one",
        observationRef: {},
        state: "accepted",
        revision: 1,
        text: submitted?.text,
        error: null,
        editable: false,
        cancellable: false,
      },
    });
  });
  const editor = page.getByLabel("Message", { exact: true });
  await editor.fill("The dispatched revision");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect.poll(() => submitted?.text).toBe("The dispatched revision");
  await editor.fill("New work typed while acceptance is pending");
  accept();
  await expect(page.getByText(/Command status/)).toBeVisible();
  await expect(editor).toHaveValue(
    "New work typed while acceptance is pending",
  );
});

test("an expired observation view preserves the unsent draft and restores editor focus", async ({
  page,
}) => {
  const state = await installTraceFixture(page);
  state.control = available(["send"], "lease", null);
  await openTrace(page);
  const editor = page.getByLabel("Message", { exact: true });
  await editor.fill("Unsent during snapshot expiry");
  await editor.focus();
  let expired = false;
  await page.route("**/api/v1/trace/changes?*", (route) => {
    if (!expired) {
      expired = true;
      return route.fulfill({
        status: 410,
        json: { error: { code: "snapshot_expired", message: "View expired" } },
      });
    }
    return route.fulfill({
      contentType: "text/event-stream",
      body: ": heartbeat\n\n",
    });
  });
  await expect.poll(() => expired).toBe(true);
  await expect(editor).toHaveValue("Unsent during snapshot expiry");
  await expect(editor).toBeFocused();
  expect(state.commands).toHaveLength(0);
});
