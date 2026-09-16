import { expect, test } from "./fixtures/test";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";
import {
  installTraceFixture,
  message,
  MAIN_AGENT,
  openTrace,
} from "./fixtures/trace";

/** A large synthetic layout scope, with real authorized filesystem requests. */
test("filesystem interaction remains bounded beside a large updating trace", async ({
  page,
}) => {
  test.slow();
  const directory = await mkdtemp(path.join(os.tmpdir(), "sylph-perf-"));
  try {
    for (let index = 0; index < 20; index++) {
      const child = `directory-${index}`;
      await mkdir(path.join(directory, child));
      await writeFile(
        path.join(directory, child, "visible.txt"),
        "Filesystem responsiveness",
      );
    }
    const sessionId = await createSession(page, { workdir: directory });
    const metadata = (
      await (
        await page.request.get(
          `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
        )
      ).json()
    ).session;
    const rows = Array.from({ length: 10000 }, (_, index) => message(index));
    const last = rows.at(-1)!;
    if (last.content.kind !== "message") throw new Error("message fixture");
    last.content.parts[0].previewTruncated = true;
    last.content.parts[0].payload.totalBytes = 6 * 1024 ** 3;
    const state = await installTraceFixture(page, rows);
    state.mainHeader.fileContext = {
      sessionId,
      environment: metadata.environment,
      workdir: metadata.workdir,
    };
    const response = page.waitForResponse(
      (response) =>
        new URL(response.url()).pathname ===
        `/api/v1/trace/agents/${MAIN_AGENT}`,
    );
    await openTrace(page);
    const served = await (await response).json();
    expect(served.window.items.length).toBeLessThanOrEqual(
      served.window.limits.maxItems,
    );
    expect(
      await page.locator("[data-trace-address]").count(),
    ).toBeLessThanOrEqual(served.window.limits.maxMountedRows);
    await page.getByRole("button", { name: "Details", exact: true }).click();
    await page.getByRole("tab", { name: "Files", exact: true }).click();
    const inspector = page.getByLabel("Trace inspector", { exact: true });
    const samples: number[] = [];
    for (let index = 0; index < 8; index++) {
      state.revision++;
      const folder = inspector.getByTitle(`directory-${index}`, {
        exact: true,
      });
      await folder.scrollIntoViewIfNeeded();
      const started = await page.evaluate(() => performance.now());
      await folder.click();
      await expect(
        inspector.getByTitle(`directory-${index}/visible.txt`, { exact: true }),
      ).toBeVisible();
      samples.push((await page.evaluate(() => performance.now())) - started);
    }
    expect(
      state.reads.filter((read) => read.includes("/payloads/")),
    ).toHaveLength(0);
    expect(Math.max(...samples)).toBeLessThan(2000);
    console.log(
      `trace_filesystem_measurement ${JSON.stringify({ scopeItems: served.window.total.value, servedItems: served.window.returnedItems, servedBytes: served.window.returnedBytes, mountedRows: await page.locator("[data-trace-address]").count(), interactionMs: samples })}`,
    );
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
