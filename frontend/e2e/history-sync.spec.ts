import { expect, test, type Page } from "@playwright/test";
import type { Session, Turn, StreamEvent } from "../lib/api-types";
import type { ImportJob } from "../lib/imports-api";

const firstSession: Session = {
  id: "history-machine:codex:first",
  environment: "history-machine",
  agent: "codex",
  nativeId: "first",
  workdir: "/tmp/history-sync",
  title: "First history session",
  injectIdentity: false,
  fidelity: "blocks",
  flagsJson: "{}",
  status: "idle",
  createdAtMs: 1,
  updatedAtMs: 2,
};
const secondSession: Session = { ...firstSession, id: "history-machine:codex:second", nativeId: "second", title: "Second history session" };
const importJob: ImportJob = {
  id: "history-import",
  environment: firstSession.environment,
  originSubject: "fixture",
  workdir: firstSession.workdir,
  mode: "import_and_watch",
  status: "completed",
  scanComplete: true,
  cancelRequested: false,
  total: 1,
  imported: 1,
  refreshed: 0,
  skipped: 0,
  failed: 0,
  pending: 0,
  error: null,
};

function sessionPath(session = firstSession) {
  return `/api/v1/sessions/${encodeURIComponent(session.id)}`;
}

async function gotoReady(page: Page, url: string) {
  await page.goto(url);
  await expect(page.getByRole("button", { name: "Open settings" })).toBeVisible();
}

async function fixture(page: Page) {
  await page.addInitScript(({ environment, path }) => {
    window.localStorage.setItem("sylph-web.settings.v1", JSON.stringify({
      showRightSidebar: true,
      pinnedWorkspaces: [{ environment, path }],
    }));
  }, { environment: firstSession.environment, path: firstSession.workdir });
  await page.route("**/api/v1/sessions?*", (route) => route.fulfill({ json: { sessions: [firstSession, secondSession] } }));
  await page.route("**/api/v1/workspaces", (route) => route.fulfill({ json: { workspaces: [], quickStartDir: "/tmp" } }));
  for (const session of [firstSession, secondSession]) {
    const base = `**${sessionPath(session)}`;
    await page.route(base, (route) => route.fulfill({ json: { session } }));
    await page.route(`${base}/turns?*`, (route) => route.fulfill({ json: { turns: [], hasMore: false, nextCursor: null, snapshotSeq: -1 } }));
    await page.route(`${base}/capabilities`, (route) => route.fulfill({ json: { link: { observe: { available: true }, control: { available: false, reason: "environment_unreachable" } }, supports: [] } }));
    await page.route(`${base}/requests`, (route) => route.fulfill({ json: [] }));
    await page.route(`${base}/controls`, (route) => route.fulfill({ json: { controls: [] } }));
    await page.route(`${base}/events`, () => {});
    await page.route(`${base}/trace/events?*`, () => {});
    await page.route(`${base}/files*`, (route) => route.fulfill({ json: {
      root: session.workdir,
      entries: new URL(route.request().url()).searchParams.has("path")
        ? [{ name: "nested.txt", path: "src/nested.txt", isDir: false }]
        : [{ name: "src", path: "src", isDir: true }],
    } }));
  }
  await page.route("**/api/v1/environments/*/imports/list*", (route) => route.fulfill({ json: { jobs: [importJob] } }));
  await page.route(`**/api/v1/imports/${importJob.id}`, (route) => route.fulfill({ json: { job: importJob } }));
  await page.route("**/api/v1/imports/*/items*", (route) => route.fulfill({ json: { items: [], nextCursor: null } }));
}

async function openImports(page: Page) {
  await page.keyboard.press("ControlOrMeta+,");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await settings.getByRole("button", { name: "Workspaces", exact: true }).click();
  return settings;
}

const nativeTurn: Turn = {
  id: "native-turn",
  sessionId: firstSession.id,
  status: "completed",
  origin: "user",
  requestText: "Recover this imported user question",
  commandInvocationId: null,
  idempotencyKey: null,
  errorCode: null,
  errorMessage: null,
  createdAtMs: 3,
  startedAtMs: 3,
  completedAtMs: 4,
  stoppedAtMs: null,
};

for (const source of ["native input", "journaled input", "native input with later steering"]) {
  test(`${source} displays the opening question once`, async ({ page }) => {
    await fixture(page);
    const events: StreamEvent[] = [{ seq: 1, kind: "assistant", phase: "completed", text: "Recovered answer", stability: "stable", source: "native", createdAtMs: 4 }];
    if (source !== "native input") events.unshift({
      seq: 0, kind: "user", text: source === "journaled input" ? nativeTurn.requestText : "Please use the later correction",
      stability: "stable", source: "server", createdAtMs: 3,
    });
    await page.route(`**${sessionPath()}/turns?*`, (route) => route.fulfill({ json: { turns: [{ turn: nativeTurn, events }], hasMore: false, snapshotSeq: 1 } }));
    await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
    const transcript = page.getByRole("main");
    await expect(transcript.getByText(nativeTurn.requestText, { exact: true })).toHaveCount(1);
    await expect(transcript.getByText("Recovered answer", { exact: true })).toBeVisible();
  });
}

test("an empty agent turn explains the missing content instead of showing an unprompted message", async ({ page }) => {
  await fixture(page);
  await page.route(`**${sessionPath()}/turns?*`, (route) => route.fulfill({ json: {
    turns: [{ turn: { ...nativeTurn, origin: "agent", requestText: "" }, events: [] }], hasMore: false, snapshotSeq: -1,
  } }));
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  await expect(page.getByRole("main").getByText("No transcript content has been saved for this turn.")).toBeVisible();
  await expect(page.getByRole("main").getByText("the agent reported this on its own")).toBeHidden();
});

test("a failed capability read reports the request error and retries without declaring the machine offline", async ({ page }) => {
  await fixture(page);
  let failed = true;
  await page.route(`**${sessionPath()}/capabilities`, (route) => route.fulfill(failed
    ? { status: 503, json: { error: { code: "COMMAND_TIMEOUT", message: "Capability command timed out" } } }
    : { json: { link: { observe: { available: true }, control: { available: true } }, supports: [] } }));
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  const notice = page.getByRole("main").getByRole("alert");
  await expect(notice).toContainText("Could not read session capabilities.");
  await expect(notice).toContainText("Capability command timed out");
  await expect(page.getByText("That machine is not connected.", { exact: true })).toBeHidden();
  failed = false;
  await notice.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(notice).toBeHidden();
});

test("the saved import mode survives pending requests and reopening settings", async ({ page }) => {
  await fixture(page);
  let job = importJob;
  await page.route("**/api/v1/environments/*/imports/list*", (route) => route.fulfill({ json: { jobs: [job] } }));
  await page.route(`**/api/v1/imports/${importJob.id}`, (route) => route.fulfill({ json: { job } }));
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  const settings = await openImports(page);
  await expect(settings.getByRole("radio", { name: "Import and keep syncing" })).toBeChecked();
  await settings.getByRole("radio", { name: "Import once", exact: true }).check();
  let submitted: unknown;
  let accept!: () => void;
  const accepted = new Promise<void>((resolve) => { accept = resolve; });
  await page.route("**/api/v1/environments/*/imports", async (route) => {
    submitted = route.request().postDataJSON();
    await accepted;
    job = { ...importJob, mode: "import_once", status: "running" };
    await route.fulfill({ json: { job } });
  });
  await settings.getByRole("button", { name: "Sync history now" }).click();
  await expect(settings.getByRole("radio", { name: "Import once", exact: true })).toBeDisabled();
  await expect.poll(() => submitted).toMatchObject({ mode: "import_once" });
  accept();
  await expect(settings.getByText(/^running ·/)).toBeVisible();
  expect(submitted).toMatchObject({ mode: "import_once" });
  await settings.getByRole("button", { name: "Close", exact: true }).click();
  const reopened = await openImports(page);
  await expect(reopened.getByRole("radio", { name: "Import once", exact: true })).toBeChecked();
  await expect(reopened.getByRole("radio", { name: "Import once", exact: true })).toBeDisabled();
});

test("retrying an import keeps its identity until acceptance or a mode change", async ({ page }) => {
  await fixture(page);
  await page.route("**/api/v1/environments/*/imports/list*", (route) => route.fulfill({ json: { jobs: [] } }));
  const requests: Array<{ mode: string; idempotencyKey: string }> = [];
  await page.route("**/api/v1/environments/*/imports", (route) => {
    const request = route.request().postDataJSON();
    requests.push(request);
    return route.fulfill(requests.length === 1 || requests.length === 3
      ? { status: 503, json: { error: { code: "COMMAND_TIMEOUT", message: `Import response lost ${requests.length}` } } }
      : { json: { job: { ...importJob, mode: request.mode } } });
  });
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  const settings = await openImports(page);
  const sync = settings.getByRole("button", { name: "Sync history now" });
  await sync.click();
  await expect(settings.getByRole("alert")).toHaveText("Import response lost 1");
  await sync.click();
  await expect(settings.getByText(/^completed ·/)).toBeVisible();
  expect(requests[1].idempotencyKey).toBe(requests[0].idempotencyKey);
  await sync.click();
  await expect(settings.getByRole("alert")).toHaveText("Import response lost 3");
  expect(requests[2].idempotencyKey).not.toBe(requests[1].idempotencyKey);
  await settings.getByRole("radio", { name: "Import and keep syncing" }).check();
  await sync.click();
  await expect(settings.getByRole("alert")).toBeHidden();
  expect(requests[3].idempotencyKey).not.toBe(requests[2].idempotencyKey);
  expect(requests[3].mode).toBe("import_and_watch");
});

test("finishing an import refreshes an already open transcript and file tree", async ({ page }) => {
  await fixture(page);
  let finished = false;
  await page.route(`**/api/v1/imports/${importJob.id}`, (route) => route.fulfill({ json: { job: { ...importJob, status: finished ? "completed" : "running" } } }));
  await page.route(`**${sessionPath()}/files*`, (route) => route.fulfill({ json: { root: firstSession.workdir, entries: finished ? [{ name: "synced.txt", path: "synced.txt", isDir: false }] : [] } }));
  await page.route(`**${sessionPath()}`, (route) => route.fulfill({ json: { session: { ...firstSession, title: finished ? "Synchronized title" : firstSession.title } } }));
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  await expect(page.getByRole("heading", { name: firstSession.title! })).toBeVisible();
  const settings = await openImports(page);
  await expect(settings.getByText(/^running ·/)).toBeVisible();
  finished = true;
  await expect(settings.getByText(/^completed ·/)).toBeVisible();
  await settings.getByRole("button", { name: "Close", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Synchronized title" })).toBeVisible();
  await expect(page.getByRole("complementary", { name: "Artifacts" }).getByTitle("synced.txt", { exact: true })).toBeVisible();
});

test("refreshing a transcript refreshes a file tree that previously failed", async ({ page }) => {
  await fixture(page);
  let refreshed = false;
  await page.route(`**${sessionPath()}/files*`, (route) => route.fulfill(refreshed
    ? { json: { root: firstSession.workdir, entries: [{ name: "recovered.txt", path: "recovered.txt", isDir: false }] } }
    : { status: 409, json: { error: { code: "WORKDIR_UNKNOWN", message: "No recorded workdir" } } }));
  await page.route(`**${sessionPath()}/reread`, (route) => {
    refreshed = true;
    return route.fulfill({ json: { reread: { examined: 1, ingested: 1, alreadyHeld: 0, readErrors: 0 } } });
  });
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  const pane = page.getByRole("complementary", { name: "Artifacts" });
  await expect(pane.getByText(/No recorded workdir/)).toBeVisible();
  const sidebar = page.getByRole("complementary", { name: "Workspaces" });
  await sidebar.getByRole("link", { name: /First history session/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Refresh transcript" }).click();
  await expect(pane.getByTitle("recovered.txt", { exact: true })).toBeVisible();
});

test("switching sessions resets the file tree expansion state", async ({ page }) => {
  await fixture(page);
  await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
  const pane = page.getByRole("complementary", { name: "Artifacts" });
  await pane.getByTitle("src", { exact: true }).click();
  await expect(pane.getByTitle("src/nested.txt", { exact: true })).toBeVisible();
  await page.getByRole("complementary", { name: "Workspaces" }).getByRole("link", { name: /Second history session/ }).click();
  await expect(page.getByRole("heading", { name: secondSession.title! })).toBeVisible();
  await expect(pane.getByRole("treeitem", { name: /src/ }).first()).toHaveAttribute("aria-expanded", "false");
});

for (const suffix of ["", "/turns", "/capabilities", "/requests", "/files"]) {
  test(`switching sessions aborts the outstanding ${suffix || "session"} read`, async ({ page }) => {
    await fixture(page);
    let received = false;
    let aborted = false;
    const target = `${sessionPath()}${suffix}`;
    await page.addInitScript((targetPath) => {
      const originalFetch = window.fetch;
      window.fetch = (input, options) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (new URL(url, window.location.origin).pathname === targetPath) {
          options?.signal?.addEventListener("abort", () => window.dispatchEvent(new Event("history-read-aborted")), { once: true });
        }
        return originalFetch(input, options);
      };
    }, target);
    await page.exposeFunction("historyReadAborted", () => { aborted = true; });
    await page.addInitScript(() => window.addEventListener("history-read-aborted", () => {
      void (window as unknown as { historyReadAborted: () => Promise<void> }).historyReadAborted();
    }));
    await page.route((url) => url.pathname === target, () => { received = true; });
    await gotoReady(page, `/sessions/${encodeURIComponent(firstSession.id)}`);
    await expect.poll(() => received).toBe(true);
    await page.getByRole("complementary", { name: "Workspaces" }).getByRole("link", { name: /Second history session/ }).click();
    await expect(page.getByRole("heading", { name: secondSession.title! })).toBeVisible();
    await expect.poll(() => aborted).toBe(true);
  });
}
