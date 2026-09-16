import { expect, test, type Page } from "./fixtures/test";
import type { Session } from "../lib/api-types";
import type { ImportJob } from "../lib/imports-api";
import { CHILD_AGENT, installTraceFixture, MAIN_AGENT, message } from "./fixtures/trace";

const firstSession: Session = { id: "history-machine:codex:first", traceAgentRef: MAIN_AGENT, environment: "history-machine", agent: "codex", nativeId: "first", workdir: "/tmp/history-sync", title: "First history session", injectIdentity: false, fidelity: "blocks", flagsJson: "{}", status: "idle", createdAtMs: 1, updatedAtMs: 2 };
const secondSession: Session = { ...firstSession, id: "history-machine:codex:second", traceAgentRef: CHILD_AGENT, nativeId: "second", title: "Second history session" };
const importJob: ImportJob = { id: "history-import", environment: firstSession.environment, originSubject: "fixture", workdir: firstSession.workdir, mode: "import_and_watch", status: "completed", scanComplete: true, cancelRequested: false, total: 1, imported: 1, refreshed: 0, skipped: 0, failed: 0, pending: 0, error: null };
function sessionPath(session = firstSession) { return `/api/v1/sessions/${encodeURIComponent(session.id)}`; }
async function gotoReady(page: Page, url: string) { await page.goto(url); await expect(page.getByRole("button", { name: "Open settings" })).toBeVisible(); }
async function fixture(page: Page) {
  const state = await installTraceFixture(page, [message(0, "Retained imported answer")]);
  state.mainHeader.agent.name = firstSession.title; state.childHeader.agent.name = secondSession.title;
  state.mainHeader.fileContext = { sessionId: firstSession.id, environment: firstSession.environment, workdir: firstSession.workdir };
  state.childHeader.fileContext = { sessionId: secondSession.id, environment: secondSession.environment, workdir: secondSession.workdir };
  await page.addInitScript(({ environment, path }) => localStorage.setItem("sylph-web.settings.v1", JSON.stringify({ showRightSidebar: true, showLeftSidebar: true, pinnedWorkspaces: [{ environment, path }] })), { environment: firstSession.environment, path: firstSession.workdir });
  await page.route("**/api/v1/sessions?*", (route) => route.fulfill({ json: { sessions: [firstSession, secondSession] } }));
  await page.route("**/api/v1/workspaces", (route) => route.fulfill({ json: { workspaces: [], quickStartDir: "/tmp" } }));
  for (const session of [firstSession, secondSession]) {
    await page.route(`**${sessionPath(session)}`, (route) => route.fulfill({ json: { session } }));
    await page.route(`**${sessionPath(session)}/files*`, (route) => route.fulfill({ json: { root: session.workdir, entries: new URL(route.request().url()).searchParams.has("path") ? [{ name: "nested.txt", path: "src/nested.txt", isDir: false }] : [{ name: "src", path: "src", isDir: true }] } }));
  }
  await page.route("**/api/v1/environments/*/imports/list*", (route) => route.fulfill({ json: { jobs: [importJob] } }));
  await page.route(`**/api/v1/imports/${importJob.id}`, (route) => route.fulfill({ json: { job: importJob } }));
  await page.route("**/api/v1/imports/*/items*", (route) => route.fulfill({ json: { items: [], nextCursor: null } }));
  return state;
}
async function openImports(page: Page) { await page.keyboard.press("ControlOrMeta+,"); const settings = page.getByRole("dialog", { name: "Settings" }); await settings.getByRole("button", { name: "Workspaces", exact: true }).click(); return settings; }

test("the saved import mode survives pending requests and reopening settings", async ({ page }) => {
  await fixture(page);
  let job = importJob;
  await page.route("**/api/v1/environments/*/imports/list*", (route) => route.fulfill({ json: { jobs: [job] } }));
  await page.route(`**/api/v1/imports/${importJob.id}`, (route) => route.fulfill({ json: { job } }));
  await gotoReady(page, `/agents/${firstSession.traceAgentRef}`);
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
  await gotoReady(page, `/agents/${firstSession.traceAgentRef}`);
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
  await gotoReady(page, `/agents/${firstSession.traceAgentRef}`);
  const pane = page.getByRole("complementary", { name: "Trace inspector" });
  await expect(pane.getByText(/No recorded workdir/)).toBeVisible();
  const sidebar = page.getByRole("complementary", { name: "Workspaces" });
  await sidebar.getByRole("link", { name: /First history session/ }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Refresh transcript" }).click();
  await expect(pane.getByTitle("recovered.txt", { exact: true })).toBeVisible();
});

test("switching sessions resets the file tree expansion state", async ({ page }) => {
  await fixture(page);
  await gotoReady(page, `/agents/${firstSession.traceAgentRef}`);
  const pane = page.getByRole("complementary", { name: "Trace inspector" });
  await pane.getByTitle("src", { exact: true }).click();
  await expect(pane.getByTitle("src/nested.txt", { exact: true })).toBeVisible();
  await page.getByRole("complementary", { name: "Workspaces" }).getByRole("link", { name: /Second history session/ }).click();
  await expect(page.getByRole("heading", { name: secondSession.title! })).toBeVisible();
  await expect(pane.getByRole("treeitem", { name: /src/ }).first()).toHaveAttribute("aria-expanded", "false");
});


for (const targetKind of ["bootstrap", "control", "files"]) {
  test(`switching agents aborts an outstanding ${targetKind} read`, async ({ page }) => {
    await fixture(page);
    const target = targetKind === "files" ? `${sessionPath()}/files` : `/api/v1/trace/agents/${MAIN_AGENT}${targetKind === "control" ? "/control" : ""}`;
    let received = false, aborted = false;
    await page.exposeFunction("traceReadAborted", () => { aborted = true; });
    await page.addInitScript((targetPath) => {
      const original = window.fetch;
      window.fetch = (input, options) => {
        const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
        if (new URL(url, location.origin).pathname === targetPath) options?.signal?.addEventListener("abort", () => { void (window as unknown as { traceReadAborted: () => Promise<void> }).traceReadAborted(); }, { once: true });
        return original(input, options);
      };
    }, target);
    await page.route((url) => url.pathname === target, () => { received = true; });
    await gotoReady(page, `/agents/${firstSession.traceAgentRef}`); await expect.poll(() => received).toBe(true);
    await page.getByRole("complementary", { name: "Workspaces" }).getByRole("link", { name: /Second history session/ }).click();
    await expect(page.getByRole("heading", { name: secondSession.title! })).toBeVisible(); await expect.poll(() => aborted).toBe(true);
  });
}
