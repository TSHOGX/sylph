import { expect, test } from "@playwright/test";
import { gotoReady } from "./fixtures/session";

/**
 * The Configuration section of settings.
 *
 * Worth an E2E spec rather than only a unit test because what it asserts is a
 * *round trip*: the panel has to render what this backend actually resolved, and
 * the harness resolves it differently from any default — its own `SYLPH_HOME`, its
 * own agents directory, a TraceDB path that deliberately does not exist. A
 * component test with a mocked response could not tell the difference between a
 * panel wired to the endpoint and one wired to nothing.
 *
 * The whole point of the section is that a misconfiguration becomes visible, so
 * the assertions here are about the facts that make it diagnosable — the source of
 * each value, and whether the path is there — rather than about the values, which
 * belong to whichever machine ran the suite.
 */

async function openConfig(page: import("@playwright/test").Page) {
  // Role, not name: the shell is what tells us the shortcut's handler is
  // registered. A keypress sent between navigation and hydration reaches a
  // document with no listener and is swallowed.
  await expect(page.getByRole("banner").getByRole("button").first()).toBeVisible();
  await page.keyboard.press("ControlOrMeta+Comma");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "Configuration" }).click();
  return settings;
}

test("the configuration section reports what this backend resolved", async ({ page }) => {
  await gotoReady(page);
  const settings = await openConfig(page);

  // The file to edit, named. Without it the panel shows values a user cannot act
  // on, since nothing else in the UI says where they come from.
  await expect(settings.getByText(/config\.toml$/)).toBeVisible();

  // Every setting the resolver knows. A key present in the backend and missing
  // here is invisible to anyone who never opens a terminal.
  const table = settings.getByRole("table");
  for (const key of [
    "server.host",
    "server.port",
    "paths.data_dir",
    "paths.agents_dir",
    "paths.soul",
    "paths.quick_start_dir",
    "paths.browse_roots",
    "tracedb.bin",
    "log.level",
  ]) {
    await expect(table.getByRole("rowheader", { name: key })).toBeVisible();
  }
});

test("a value's source is shown, so an ignored setting is distinguishable from a wrong one", async ({
  page,
}) => {
  await gotoReady(page);
  const settings = await openConfig(page);
  const table = settings.getByRole("table");

  // The harness sets these through the environment, and the row has to name the
  // variable rather than the word "environment": a reader acting on the row needs
  // the name, and this is the fact that separates "my path is wrong" from "my
  // config.toml is being overridden" — opposite fixes.
  const browseRoots = table.getByRole("row").filter({ hasText: "paths.browse_roots" });
  await expect(browseRoots).toContainText("SYLPH_BROWSE_ROOTS");

  const tracedb = table.getByRole("row").filter({ hasText: "tracedb.bin" });
  await expect(tracedb).toContainText("SYLPH_TRACEDB_BIN");

  // And one the harness leaves alone, so the column is not simply printing "env"
  // for everything.
  const host = table.getByRole("row").filter({ hasText: "server.host" });
  await expect(host).toContainText("default");
});

test("a path that does not exist is called out, and a non-path is not", async ({ page }) => {
  await gotoReady(page);
  const settings = await openConfig(page);
  const table = settings.getByRole("table");

  // The suite points TRACEDB at a file it never creates, which is exactly the
  // shape of the real failure: the archive silently returns nothing and the UI
  // otherwise looks healthy.
  const tracedb = table.getByRole("row").filter({ hasText: "tracedb.bin" });
  await expect(tracedb).toContainText("missing");

  // Seeded by the backend's own first run, so it is there — and proves "missing"
  // is a real check rather than a label on every row.
  const dataDir = table.getByRole("row").filter({ hasText: "paths.data_dir" });
  await expect(dataDir).not.toContainText("missing");

  // A log level is not a path. Marking it missing would report a working setting
  // as broken, which is why `exists` is absent rather than false for these.
  const logLevel = table.getByRole("row").filter({ hasText: "log.level" });
  await expect(logLevel).not.toContainText("missing");
});

test("the adapter directory actually in use is marked among the ones tried", async ({ page }) => {
  await gotoReady(page);
  const settings = await openConfig(page);

  // "No agents" is unactionable without this: a missing install and a mistyped
  // path produce the same empty list and need opposite fixes. Exactly one entry
  // can be the one being read.
  await expect(settings.getByText("in use")).toHaveCount(1);
});

test("the section is read-only, offering no way to edit a path from the browser", async ({
  page,
}) => {
  await gotoReady(page);
  const settings = await openConfig(page);

  // Deliberate, not unfinished: browse_roots bounds what any client of this
  // backend may enumerate and agents_dir names directories the server executes
  // from, so a loopback HTTP surface able to rewrite them is a different threat
  // model. If an edit control is ever added here, that decision should be
  // revisited on purpose rather than by this spec quietly starting to fail.
  await expect(settings.getByRole("textbox")).toHaveCount(0);
  await expect(settings.getByRole("table").getByRole("button")).toHaveCount(0);
});
