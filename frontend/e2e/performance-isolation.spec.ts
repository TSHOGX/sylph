import { expect, test } from "@playwright/test";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { createSession } from "./fixtures/session";

/** A reproducible UI/transport measurement, also run against the previous commit. */
test("file browsing stays responsive with a large transcript and concurrent execution", async ({
  page,
}) => {
  test.slow();
  await page.addInitScript(() => {
    const stored = JSON.parse(
      localStorage.getItem("sylph-web.settings.v1") ?? "{}",
    );
    localStorage.setItem(
      "sylph-web.settings.v1",
      JSON.stringify({ ...stored, showRightSidebar: true }),
    );
    const watch = new MutationObserver(() => {
      const pane = document.querySelector('aside[aria-label="Work panel"]');
      if (!pane) return;
      watch.disconnect();
      const files = new MutationObserver(() => {
        if (pane.querySelector('[title="latency-000"]')) {
          files.disconnect();
          requestAnimationFrame(() => {
            (window as unknown as { firstFilePaint: number }).firstFilePaint =
              performance.now();
          });
        }
      });
      files.observe(pane, { childList: true, subtree: true });
    });
    watch.observe(document, { childList: true, subtree: true });
  });
  const directory = await mkdtemp(path.join(os.tmpdir(), "sylph-perf-"));
  try {
    for (let i = 0; i < 100; i++) {
      const name = `latency-${String(i).padStart(3, "0")}`;
      await mkdir(path.join(directory, name));
      await writeFile(
        path.join(directory, name, "visible.txt"),
        "File browsing measurement",
      );
    }
    const background = await createSession(page, { workdir: directory });
    const sessionId = await createSession(page, { workdir: directory });
    const post = await page.request.post(
      `/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`,
      { data: { text: "bulk render please" } },
    );
    expect(post.ok()).toBe(true);
    const turnId = (await post.json()).turn.id;
    await expect
      .poll(
        async () => {
          const response = await page.request.get(`/api/v1/turns/${turnId}`);
          return (await response.json()).turn.status;
        },
        { timeout: 30_000 },
      )
      .toBe("completed");
    await page.reload();
    await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
    const pane = page.getByRole("complementary", { name: "Work panel" });
    if (!(await pane.isVisible()))
      await page.keyboard.press("ControlOrMeta+\\");
    await expect(pane.getByTitle("latency-000", { exact: true })).toBeVisible();
    await page.waitForFunction(() =>
      Boolean((window as unknown as { firstFilePaint: number }).firstFilePaint),
    );
    const firstFile = await page.evaluate(() => {
      const resource = performance
        .getEntriesByType("resource")
        .find((entry) => {
          const url = new URL(entry.name);
          return (
            url.pathname.endsWith("/files") && !url.searchParams.get("path")
          );
        }) as PerformanceResourceTiming;
      return {
        paintedMs: (window as unknown as { firstFilePaint: number })
          .firstFilePaint,
        requestMs: resource.requestStart,
        responseMs: resource.responseEnd,
        bytes: resource.encodedBodySize,
        server: Object.fromEntries(
          resource.serverTiming.map((timing) => [timing.name, timing.duration]),
        ),
      };
    });
    const client = await page.context().newCDPSession(page);
    await client.send("Performance.enable");
    const metricsBefore = await client.send("Performance.getMetrics");
    const dom = await page.locator("[data-scroller]").evaluate((element) => ({
      elements: element.querySelectorAll("*").length,
      sections: element.querySelectorAll("section").length,
    }));
    const backgroundResponse = await page.request.post(
      `/api/v1/sessions/${encodeURIComponent(background)}/turns`,
      { data: { text: "bulk render please" } },
    );
    expect(backgroundResponse.ok()).toBe(true);
    const backgroundTurnId = (await backgroundResponse.json()).turn.id;
    const searches: Promise<unknown>[] = [];
    const samples: unknown[] = [];
    for (let i = 0; i < 14; i++) {
      const relative = `latency-${String(i).padStart(3, "0")}`;
      searches.push(page.request.get("/api/v1/search?q=Step&limit=20"));
      await pane.getByTitle(relative, { exact: true }).scrollIntoViewIfNeeded();
      await pane
        .getByTitle(relative, { exact: true })
        .evaluate((element, relative) => {
          const state = window as unknown as {
            fileMeasurement: { clicked: number; painted?: number };
          };
          performance.clearResourceTimings();
          state.fileMeasurement = { clicked: performance.now() };
          const observer = new MutationObserver(() => {
            if (document.querySelector(`[title="${relative}/visible.txt"]`)) {
              observer.disconnect();
              requestAnimationFrame(() => {
                state.fileMeasurement.painted = performance.now();
              });
            }
          });
          observer.observe(document.body, { childList: true, subtree: true });
          (element as HTMLElement).click();
        }, relative);
      await expect(
        pane.getByTitle(`${relative}/visible.txt`, { exact: true }),
      ).toBeVisible();
      await page.waitForFunction(() =>
        Boolean(
          (window as unknown as { fileMeasurement: { painted?: number } })
            .fileMeasurement.painted,
        ),
      );
      samples.push(
        await page.evaluate((relative) => {
          const state = (
            window as unknown as {
              fileMeasurement: { clicked: number; painted: number };
            }
          ).fileMeasurement;
          const resource = performance
            .getEntriesByType("resource")
            .find((entry) => {
              const url = new URL(entry.name);
              return (
                url.pathname.endsWith("/files") &&
                url.searchParams.get("path") === relative
              );
            }) as PerformanceResourceTiming | undefined;
          if (!resource)
            throw new Error("The directory expansion did not issue a request");
          return {
            relative,
            totalMs: state.painted - state.clicked,
            beforeRequestMs: resource.requestStart - state.clicked,
            requestToFirstByteMs:
              resource.responseStart - resource.requestStart,
            httpTransferMs: resource.responseEnd - resource.responseStart,
            afterResponseMs: state.painted - resource.responseEnd,
            bytes: resource.encodedBodySize,
            server: Object.fromEntries(
              resource.serverTiming.map((timing) => [
                timing.name,
                timing.duration,
              ]),
            ),
          };
        }, relative),
      );
    }
    await Promise.all(searches);
    await expect
      .poll(
        async () => {
          const response = await page.request.get(
            `/api/v1/turns/${backgroundTurnId}`,
          );
          return (await response.json()).turn.status;
        },
        { timeout: 30_000 },
      )
      .toBe("completed");
    const backgroundState = await (
      await page.request.get(`/api/v1/turns/${backgroundTurnId}`)
    ).json();
    const metricsAfter = await client.send("Performance.getMetrics");
    const heap = (metrics: { metrics: { name: string; value: number }[] }) =>
      metrics.metrics.find((item) => item.name === "JSHeapUsedSize")?.value;
    const report = {
      label: process.env.SYLPH_MEASURE_LABEL ?? "working-tree",
      createdAt: new Date().toISOString(),
      browser: page.context().browser()?.version(),
      directories: 100,
      backgroundExecution: {
        turnId: backgroundTurnId,
        startedAtMs: backgroundState.turn.startedAtMs,
        completedAtMs: backgroundState.turn.completedAtMs,
      },
      firstFile,
      samples,
      dom,
      heapBefore: heap(metricsBefore),
      heapAfter: heap(metricsAfter),
    };
    console.log(`file_tree_measurement ${JSON.stringify(report)}`);
    if (process.env.SYLPH_MEASURE_OUTPUT)
      await writeFile(
        process.env.SYLPH_MEASURE_OUTPUT,
        JSON.stringify(report, null, 2) + "\n",
      );
    expect(
      Math.max(
        ...samples.map((sample) => (sample as { totalMs: number }).totalMs),
      ),
    ).toBeLessThan(2000);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
