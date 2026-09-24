import { expect, test } from "./fixtures/test";
import { createHash } from "node:crypto";
import {
  installTraceFixture,
  message,
  openTrace,
  traceView,
} from "./fixtures/trace";

// Exercise the coarse-pointer size contract at both reading widths.
test.use({ hasTouch: true });

function recording(texts: string[], gap = 0, corrupt = false) {
  const frames: Record<string, unknown>[] = [
    {
      kind: "header",
      format: "sylph-evidence-jsonl",
      scope: "environment",
      pacing: { clock: "server_admission", speed: 1, maxGapMs: 2000 },
      evidenceCut: traceView().evidenceCut,
    },
  ];
  for (let index = 0; index < texts.length; index++) {
    const payload = `payload-${index}`,
      bytes = Buffer.from(texts[index]);
    const capture = {
      payloadRef: payload,
      observationRef: {
        producerId: "retained-source",
        producerSeq: index + 1,
      },
      sourcePosition: { generation: 0, locator: { ordinal: index } },
      transportKind: "native_file",
      capturedAt: {
        domain: "capture",
        value: index,
        unit: "milliseconds",
        basis: "source_wall",
      },
      sourceClock: null,
    };
    frames.push(
      {
        kind: "timing",
        admissionSeq: index + 1,
        recordedDeltaMs: index ? 200000 : null,
        delayMs: index === 1 ? gap : 0,
        adjustment:
          index === 1 && gap ? "gap_clamped" : index ? "recorded" : "first",
      },
      {
        kind: "capture",
        metadataText: {
          observationRef: JSON.stringify(capture.observationRef),
          sourcePosition: JSON.stringify(capture.sourcePosition),
          capturedAt: JSON.stringify(capture.capturedAt),
          sourceClock: JSON.stringify(capture.sourceClock),
        },
        record: {
          admissionSeq: index + 1,
          admittedAtMs: 1780000000000 + index * 200000,
          capture,
        },
      },
      { kind: "bytes", payload, offset: 0, data: bytes.toString("base64") },
      { kind: "payload_end", payload, observedBytes: bytes.length },
    );
  }
  const prefix = frames.map((frame) => JSON.stringify(frame) + "\n").join("");
  return (
    prefix +
    JSON.stringify({
      kind: "complete",
      observations: texts.length,
      frames: frames.length,
      prefixBytes: Buffer.byteLength(prefix),
      sha256: corrupt
        ? "bad"
        : createHash("sha256").update(prefix).digest("hex"),
    }) +
    "\n"
  );
}
const headers = {
  "content-type": "application/x-ndjson",
  "x-sylph-export-limit": "4194304",
  "x-sylph-export-frame-limit": "2097152",
  "x-sylph-export-duration-ms": "300000",
};
for (const width of [1280, 390]) {
  test(`recorded observations pause without losing their order or redirecting control at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    const fixture = await installTraceFixture(page, [
      message(0, "Current conversation"),
    ]);
    let reads = 0;
    await page.route("**/api/v1/trace/environments/*/playback?*", (route) => {
      reads++;
      const query = new URL(route.request().url()).searchParams;
      expect(query.get("at_snapshot")).toBe(traceView().token);
      expect(query.get("speed")).toBe("1");
      return route.fulfill({
        headers,
        body: recording(["A", "A", "B", "BC"], 2000),
      });
    });
    await openTrace(page);
    await page.getByRole("button", { name: "Details", exact: true }).click();
    const panel = page.getByRole("region", {
      name: "Recorded observation playback",
      exact: true,
    });
    await expect(
      panel.getByText(
        /Playback covers every retained observation in Fixture machine/,
      ),
    ).toBeVisible();
    const speedBox = await panel
      .getByRole("button", { name: "Playback speed", exact: true })
      .boundingBox();
    expect(speedBox?.width).toBeGreaterThanOrEqual(44);
    expect(speedBox?.height).toBeGreaterThanOrEqual(44);
    expect(reads).toBe(0);
    await panel
      .getByRole("button", { name: "Play recorded observations", exact: true })
      .click();
    const rows = panel
      .getByRole("list", { name: "Recorded observations", exact: true })
      .getByRole("listitem");
    await expect(rows).toHaveCount(1);
    await panel
      .getByRole("button", { name: "Pause playback", exact: true })
      .click();
    await expect(panel.getByRole("status")).toContainText("Paused");
    // Open the bad window deliberately: a paused player must outlast its gap.
    await page.waitForTimeout(2200);
    await expect(rows).toHaveCount(1);
    await panel
      .getByRole("button", { name: "Resume playback", exact: true })
      .click();
    await expect(panel.getByRole("status")).toContainText(
      "Playback verified and complete · 4 observations",
    );
    await expect(rows.locator("pre")).toHaveText(["A", "A", "B", "BC"]);
    await expect(
      panel.getByText("Long receipt gap shortened for playback."),
    ).toBeVisible();
    await expect(
      panel.getByRole("button", {
        name: "Play recorded observations",
        exact: true,
      }),
    ).toBeFocused();
    const history = panel.getByRole("list", {
      name: "Recorded observations",
      exact: true,
    });
    await history.focus();
    await expect(history).toBeFocused();
    expect(fixture.commands).toHaveLength(0);
    expect(reads).toBe(1);
    await testInfo.attach(`observation-playback-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
  });
}
test("playback bounds its displayed history and clears provisional content after corruption", async ({
  page,
}) => {
  await installTraceFixture(page, [message(0)]);
  let reads = 0;
  await page.route("**/api/v1/trace/environments/*/playback?*", (route) =>
    route.fulfill({
      headers,
      body:
        ++reads === 1
          ? recording(
              Array.from({ length: 600 }, (_, i) => `Raw occurrence ${i}`),
            )
          : recording(["Unverified private bytes"], 0, true),
    }),
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  const panel = page.getByRole("region", {
    name: "Recorded observation playback",
    exact: true,
  });
  await panel
    .getByRole("button", { name: "Play recorded observations", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText(
    "complete · 600 observations",
  );
  const windowText = await panel
    .getByText(/display retains at most/)
    .textContent();
  const maximum = Number(windowText?.match(/at most (\d+)/)?.[1]);
  expect(maximum).toBeGreaterThan(0);
  expect(maximum).toBeLessThan(600);
  await expect(panel.getByRole("listitem")).toHaveCount(maximum);
  await expect(panel.locator("pre").first()).toHaveText(
    `Raw occurrence ${600 - maximum}`,
  );
  await expect(panel.locator("pre").last()).toHaveText("Raw occurrence 599");
  await panel
    .getByRole("button", { name: "Play recorded observations", exact: true })
    .click();
  await expect(panel.getByRole("alert")).toContainText(
    "completion does not match",
  );
  await expect(panel.getByRole("listitem")).toHaveCount(0);
  await expect(panel.getByRole("status")).toContainText("Playback failed");
});
test("stopping a paused playback wakes its consumer and leaves agent commands untouched", async ({
  page,
}) => {
  const fixture = await installTraceFixture(page, [message(0)]);
  await page.route("**/api/v1/trace/environments/*/playback?*", (route) =>
    route.fulfill({
      headers,
      body: recording(["First", "Must not display"], 2000),
    }),
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  const panel = page.getByRole("region", {
    name: "Recorded observation playback",
    exact: true,
  });
  await panel
    .getByRole("button", { name: "Play recorded observations", exact: true })
    .click();
  await expect(panel.locator("pre")).toHaveText(["First"]);
  await panel
    .getByRole("button", { name: "Pause playback", exact: true })
    .click();
  await panel
    .getByRole("button", { name: "Stop playback", exact: true })
    .click();
  await expect(panel.getByRole("status")).toContainText("Stopped");
  await expect(panel.locator("pre")).toHaveText(["First"]);
  await expect(
    panel.getByRole("button", {
      name: "Play recorded observations",
      exact: true,
    }),
  ).toBeEnabled();
  expect(fixture.commands).toHaveLength(0);
});

test("access loss clears a paused observation preview without waiting for its playback clock", async ({
  page,
}) => {
  const fixture = await installTraceFixture(page, [message(0)]);
  let revoked = false;
  await page.route("**/api/v1/trace/changes?*", (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: revoked
        ? `data: ${JSON.stringify({ kind: "access_changed", reason: "Access revoked" })}\n\n`
        : ": heartbeat\n\n",
    }),
  );
  await page.route("**/api/v1/trace/environments/*/playback?*", (route) =>
    route.fulfill({
      headers,
      body: recording(["Private paused observation", "Must stay hidden"], 2000),
    }),
  );
  await openTrace(page);
  await page.getByRole("button", { name: "Details", exact: true }).click();
  const panel = page.getByRole("region", {
    name: "Recorded observation playback",
    exact: true,
  });
  await panel
    .getByRole("button", { name: "Play recorded observations", exact: true })
    .click();
  await expect(panel.locator("pre")).toHaveText(["Private paused observation"]);
  await panel
    .getByRole("button", { name: "Pause playback", exact: true })
    .click();
  revoked = true;
  await expect(
    page.getByText("Private paused observation", { exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByText(/Access changed. Open this conversation again/).first(),
  ).toBeVisible();
  expect(fixture.commands).toHaveLength(0);
});
