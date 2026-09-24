import { expect, test } from "./fixtures/test";
import { createHash } from "node:crypto";
import {
  installTraceFixture,
  message,
  openTrace,
  traceView,
} from "./fixtures/trace";
function archive() {
  const prefix = Buffer.from(
    [
      JSON.stringify({
        kind: "header",
        format: "sylph-evidence-jsonl",
        scope: "environment",
        environment: "fixture-environment",
        evidenceCut: traceView().evidenceCut,
      }),
      JSON.stringify({
        kind: "capture",
        record: { exact: "中文", source: "native" },
      }),
    ].join("\n") + "\n",
  );
  return Buffer.concat([
    prefix,
    Buffer.from(
      JSON.stringify({
        kind: "complete",
        frames: 2,
        observations: 1,
        prefixBytes: prefix.length,
        sha256: createHash("sha256").update(prefix).digest("hex"),
      }) + "\n",
    ),
  ]);
}
for (const width of [1280, 390]) {
  test(`evidence export names its environment and saves only a verified archive at ${width}px`, async ({
    page,
  }, testInfo) => {
    await page.setViewportSize({ width, height: 844 });
    await page.addInitScript(() =>
      Object.defineProperty(window, "showSaveFilePicker", {
        configurable: true,
        value: undefined,
      }),
    );
    const fixture = await installTraceFixture(page, [
      message(0, "Selected conversation"),
    ]);
    let reads = 0;
    const bytes = archive();
    await page.route("**/api/v1/trace/environments/*/export?*", (route) => {
      reads++;
      const url = new URL(route.request().url());
      expect(url.searchParams.get("at_snapshot")).toBe(traceView().token);
      expect(url.searchParams.get("buffered")).toBe("true");
      return route.fulfill({
        headers: {
          "content-type": "application/x-ndjson",
          "x-sylph-export-limit": "1048576",
          "x-sylph-export-frame-limit": "2097152",
        },
        body: bytes,
      });
    });
    await openTrace(page);
    await page.getByRole("button", { name: "Details", exact: true }).click();
    expect(reads).toBe(0);
    await expect(
      page.getByText(
        /All retained observations in Fixture machine through evidence cut/,
      ),
    ).toBeVisible();
    const download = page.waitForEvent("download");
    await page
      .getByRole("button", { name: "Download evidence archive", exact: true })
      .click();
    const saved = await download;
    expect(saved.suggestedFilename()).toBe("sylph-evidence.jsonl");
    const stream = await saved.createReadStream();
    const chunks = [];
    for await (const part of stream!) chunks.push(part);
    expect(Buffer.concat(chunks)).toEqual(bytes);
    await expect(
      page.getByText("Evidence archive verified and saved.", { exact: true }),
    ).toBeVisible();
    expect(reads).toBe(1);
    expect(fixture.commands).toHaveLength(0);
    await testInfo.attach(`environment-export-${width}`, {
      body: await page.screenshot(),
      contentType: "image/png",
    });
  });
}
for (const corrupted of [false, true]) {
  test(`a streamed evidence file ${corrupted ? "aborts corruption" : "commits verified completion"}`, async ({
    page,
  }) => {
    await installTraceFixture(page, [message(0)]);
    await page.addInitScript(() => {
      const output = window as unknown as {
        saved: number[];
        committed: boolean;
        aborted: boolean;
      };
      output.saved = [];
      output.committed = false;
      output.aborted = false;
      Object.defineProperty(window, "showSaveFilePicker", {
        value: async () => ({
          createWritable: async () => ({
            write: async (bytes: ArrayBuffer) => {
              output.saved.push(...new Uint8Array(bytes));
            },
            close: async () => {
              output.committed = true;
            },
            abort: async () => {
              output.aborted = true;
            },
          }),
        }),
      });
    });
    const bytes = corrupted
      ? Buffer.from(
          archive()
            .toString()
            .replace('"source":"native"', '"source":"altered"'),
        )
      : archive();
    await page.route("**/api/v1/trace/environments/*/export?*", (route) => {
      expect(new URL(route.request().url()).searchParams.get("buffered")).toBe(
        "false",
      );
      return route.fulfill({
        headers: {
          "x-sylph-export-limit": "1048576",
          "x-sylph-export-frame-limit": "2097152",
        },
        body: bytes,
      });
    });
    await openTrace(page);
    await page.getByRole("button", { name: "Details", exact: true }).click();
    await page
      .getByRole("button", { name: "Download evidence archive", exact: true })
      .click();
    if (corrupted) {
      await expect(
        page.getByRole("alert").filter({ hasText: "Evidence export" }),
      ).toContainText("completion does not match");
      expect(
        await page.evaluate(
          () => (window as unknown as { aborted: boolean }).aborted,
        ),
      ).toBe(true);
      expect(
        await page.evaluate(
          () => (window as unknown as { committed: boolean }).committed,
        ),
      ).toBe(false);
    } else {
      await expect(
        page.getByText("Evidence archive verified and saved.", { exact: true }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () => (window as unknown as { committed: boolean }).committed,
        ),
      ).toBe(true);
      expect(
        Buffer.from(
          await page.evaluate(
            () => (window as unknown as { saved: number[] }).saved,
          ),
        ),
      ).toEqual(bytes);
    }
  });
}

test("cancelling an evidence export aborts its pending file without committing", async ({
  page,
}) => {
  await installTraceFixture(page, [message(0)]);
  await page.addInitScript(() => {
    const output = window as unknown as {
      committed: boolean;
      aborted: boolean;
    };
    output.committed = false;
    output.aborted = false;
    Object.defineProperty(window, "showSaveFilePicker", {
      value: async () => ({
        createWritable: async () => ({
          write: async () => {},
          close: async () => {
            output.committed = true;
          },
          abort: async () => {
            output.aborted = true;
          },
        }),
      }),
    });
  });
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let entered = false;
  await page.route("**/api/v1/trace/environments/*/export?*", async (route) => {
    entered = true;
    await ready;
    await route
      .fulfill({
        headers: {
          "x-sylph-export-limit": "1048576",
          "x-sylph-export-frame-limit": "2097152",
        },
        body: archive(),
      })
      .catch(() => {});
  });
  try {
    await openTrace(page);
    await page.getByRole("button", { name: "Details", exact: true }).click();
    await page
      .getByRole("button", { name: "Download evidence archive", exact: true })
      .click();
    await expect.poll(() => entered).toBe(true);
    await page
      .getByRole("button", { name: "Cancel transfer", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { aborted: boolean }).aborted,
        ),
      )
      .toBe(true);
    expect(
      await page.evaluate(
        () => (window as unknown as { committed: boolean }).committed,
      ),
    ).toBe(false);
    await expect(
      page.getByRole("button", {
        name: "Download evidence archive",
        exact: true,
      }),
    ).toBeEnabled();
  } finally {
    release();
  }
});
