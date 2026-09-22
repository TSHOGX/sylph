import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "./fixtures/test";
import {
  installTraceFixture,
  message,
  openTrace,
  traceView,
  traceLimits,
} from "./fixtures/trace";

async function setup(page: Page, minimumBytes = 200_000) {
  await page.addInitScript(() => {
    const data = window as unknown as Window & { copied: string };
    data.copied = "previous clipboard value";
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (value: string) => {
          data.copied = value;
        },
      },
    });
  });
  const row = message(0, "Retained output preview");
  if (row.content.kind !== "message") throw new Error("message fixture");
  row.content.parts[0].previewTruncated = true;
  const fixture = await installTraceFixture(page, [row]);
  const line = "α中🙂 retained output\r\n";
  const text = line.repeat(Math.ceil(minimumBytes / Buffer.byteLength(line)));
  fixture.bodies.set("body-0", text);
  return { fixture, text, row };
}
async function openBody(page: Page) {
  await openTrace(page);
  await page
    .getByRole("button", {
      name: "Read recorded conversation result",
      exact: true,
    })
    .click();
  await expect(
    page.getByRole("button", { name: "Copy visible text", exact: true }),
  ).toBeEnabled();
}
function headers(bytes: number, maximum = 32 * 1024 * 1024) {
  return {
    "x-sylph-observed-bytes": String(bytes),
    "x-sylph-transfer-limit": String(maximum),
  };
}
function digest(bytes: string | Buffer) {
  return createHash("sha256").update(bytes).digest("hex");
}
test("visible copy stays in its range while complete copy transfers all retained UTF-8 without widening the reader", async ({
  page,
}) => {
  const { text, row } = await setup(page, 20 * 1024 * 1024);
  const bytes = Buffer.from(text);
  const requests: URL[] = [];
  await page.route(
    (url) => url.pathname.endsWith("/payloads/body-0/transfer"),
    async (route) => {
      const url = new URL(route.request().url());
      requests.push(url);
      expect(url.searchParams.get("kind")).toBe("copy");
      expect(url.searchParams.get("at_snapshot")).toBe(traceView().token);
      expect(url.searchParams.get("via_ref")).toBe(row.anchor);
      expect(url.searchParams.get("revision")).toBe("revision-1");
      await route.fulfill({ headers: headers(bytes.length), body: bytes });
    },
  );
  await openBody(page);
  await page
    .getByRole("button", { name: "Copy visible text", exact: true })
    .click();
  const visible = await page.evaluate(
    () => (window as unknown as Window & { copied: string }).copied,
  );
  expect(visible.length).toBeGreaterThan(0);
  expect(visible.length).toBeLessThan(text.length);
  expect(text.startsWith(visible)).toBe(true);
  expect(requests).toHaveLength(0);
  await page
    .getByRole("button", {
      name: "Copy complete observed content",
      exact: true,
    })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as Window & { copied: string }).copied.length,
      ),
    )
    .toBe(text.length);
  const completeDigest = await page.evaluate(async () => {
    const copied = (window as unknown as Window & { copied: string }).copied;
    const hash = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(copied),
    );
    return Array.from(new Uint8Array(hash), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join("");
  });
  expect(completeDigest).toBe(digest(text));
  expect(requests).toHaveLength(1);
  const rendered = await page.locator("[data-trace-address]").innerText();
  expect(Buffer.byteLength(rendered)).toBeLessThan(
    traceLimits.maxPayloadBytes + 2 * traceLimits.maxPreviewBytes,
  );
});
test("cancelled complete copy leaves the clipboard unchanged and reading usable", async ({
  page,
}) => {
  const { text } = await setup(page);
  let release!: () => void;
  let started!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const entered = new Promise<void>((resolve) => {
    started = resolve;
  });
  await page.route(
    (url) => url.pathname.endsWith("/payloads/body-0/transfer"),
    async (route) => {
      started();
      await gate;
      await route
        .fulfill({ headers: headers(Buffer.byteLength(text)), body: text })
        .catch(() => {});
    },
  );
  await openBody(page);
  await page
    .getByRole("button", {
      name: "Copy complete observed content",
      exact: true,
    })
    .click();
  await entered;
  await expect(
    page.getByRole("button", { name: "Next range", exact: true }),
  ).toBeEnabled();
  await page
    .getByRole("button", { name: "Cancel transfer", exact: true })
    .click();
  await expect(
    page.getByText("Transfer cancelled.", { exact: true }),
  ).toBeVisible();
  release();
  expect(
    await page.evaluate(
      () => (window as unknown as Window & { copied: string }).copied,
    ),
  ).toBe("previous clipboard value");
  await expect(
    page.getByRole("button", {
      name: "Copy complete observed content",
      exact: true,
    }),
  ).toBeEnabled();
});

// A standard synthetic ZIP exercises byte delivery; server tests independently
// parse the production ZIP writer's manifest and every retained range.
const archive = Buffer.from(
  "UEsDBBQAAAAAAGs4N11QSV+xYwAAAGMAAAANAAAAbWFuaWZlc3QuanNvbnsib2JzZXJ2ZWRCeXRlcyI6IDYsICJyYW5nZXMiOiBbeyJzdGFydCI6IDAsICJlbmRFeGNsdXNpdmUiOiA2fV0sICJjb3ZlcmFnZSI6IHsia2luZCI6ICJjb21wbGV0ZSJ9fVBLAwQUAAAAAABrODddtyTUQgYAAAAGAAAADQAAAGJ5dGVzLzAtNi5iaW5leGFjdApQSwECFAMUAAAAAABrODddUElfsWMAAABjAAAADQAAAAAAAAAAAAAAgAEAAAAAbWFuaWZlc3QuanNvblBLAQIUAxQAAAAAAGs4N123JNRCBgAAAAYAAAANAAAAAAAAAAAAAACAAY4AAABieXRlcy8wLTYuYmluUEsFBgAAAAACAAIAdgAAAL8AAAAAAA==",
  "base64",
);
test("a buffered observed-content download saves the exact archive", async ({
  page,
}) => {
  await setup(page);
  await page.addInitScript(() =>
    Object.defineProperty(window, "showSaveFilePicker", {
      value: undefined,
      configurable: true,
    }),
  );
  await page.route(
    (url) => url.pathname.endsWith("/payloads/body-0/transfer"),
    async (route) => {
      const url = new URL(route.request().url());
      expect(url.searchParams.get("kind")).toBe("download");
      expect(url.searchParams.get("buffered")).toBe("true");
      await route.fulfill({
        headers: { ...headers(6), "content-type": "application/zip" },
        body: archive,
      });
    },
  );
  await openBody(page);
  const pending = page.waitForEvent("download");
  await page
    .getByRole("button", { name: "Download observed content", exact: true })
    .click();
  const download = await pending;
  expect(download.suggestedFilename()).toBe("observed-content.zip");
  expect(digest(await readFile((await download.path())!))).toBe(
    digest(archive),
  );
});
for (const truncated of [false, true]) {
  test(`the streaming file writer ${truncated ? "aborts an incomplete" : "commits a complete"} archive`, async ({
    page,
  }) => {
    await setup(page);
    await page.addInitScript(() => {
      const data = window as unknown as Window & {
        saved: number[];
        committed: boolean;
        aborted: boolean;
      };
      data.saved = [];
      data.committed = false;
      data.aborted = false;
      Object.defineProperty(window, "showSaveFilePicker", {
        configurable: true,
        value: async () => ({
          createWritable: async () => ({
            write: async (bytes: ArrayBuffer) => {
              data.saved.push(...new Uint8Array(bytes));
            },
            close: async () => {
              data.committed = true;
            },
            abort: async () => {
              data.aborted = true;
            },
          }),
        }),
      });
    });
    await page.route(
      (url) => url.pathname.endsWith("/payloads/body-0/transfer"),
      async (route) => {
        expect(
          new URL(route.request().url()).searchParams.get("buffered"),
        ).toBe("false");
        await route.fulfill({
          headers: headers(6),
          body: truncated ? archive.subarray(0, archive.length - 1) : archive,
        });
      },
    );
    await openBody(page);
    await page
      .getByRole("button", { name: "Download observed content", exact: true })
      .click();
    if (truncated) {
      await expect(
        page.getByRole("alert").filter({ hasText: "completion record" }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () =>
            (window as unknown as Window & { committed: boolean }).committed,
        ),
      ).toBe(false);
      expect(
        await page.evaluate(
          () => (window as unknown as Window & { aborted: boolean }).aborted,
        ),
      ).toBe(true);
    } else {
      await expect(
        page.getByText("Observed content saved.", { exact: true }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () =>
            (window as unknown as Window & { committed: boolean }).committed,
        ),
      ).toBe(true);
      expect(
        digest(
          Buffer.from(
            await page.evaluate(
              () => (window as unknown as Window & { saved: number[] }).saved,
            ),
          ),
        ),
      ).toBe(digest(archive));
    }
  });
}

test("complete previews expose content actions and opaque bytes remain downloadable without plaintext controls", async ({
  page,
}) => {
  const short = message(0, "Short retained text");
  const opaque = message(1, "");
  if (opaque.content.kind !== "message") throw new Error("message fixture");
  opaque.content.parts[0].kind = "opaque";
  opaque.content.parts[0].payload = {
    ...opaque.content.parts[0].payload,
    encoding: "binary",
    mediaType: "application/octet-stream",
    totalBytes: 6,
    availableRanges: [{ start: 0, endExclusive: 6 }],
    availability: {
      kind: "opaque",
      reason: "No plaintext representation was observed",
    },
  };
  const fixture = await installTraceFixture(page, [short, opaque]);
  fixture.bodies.set("body-0", "Short retained text");
  await openTrace(page);
  const first = page.locator(`[data-trace-address="${short.address}"]`);
  await first
    .getByRole("button", { name: "Content actions", exact: true })
    .click();
  await expect(
    first.getByRole("button", { name: "Copy visible text", exact: true }),
  ).toBeEnabled();
  const second = page.locator(`[data-trace-address="${opaque.address}"]`);
  await expect(
    second.getByRole("button", {
      name: "Download observed content",
      exact: true,
    }),
  ).toBeEnabled();
  await expect(
    second.getByRole("button", {
      name: "Copy complete observed content",
      exact: true,
    }),
  ).toHaveCount(0);
  await expect(
    second.getByText("No plaintext representation was observed", {
      exact: false,
    }),
  ).toBeVisible();
});
