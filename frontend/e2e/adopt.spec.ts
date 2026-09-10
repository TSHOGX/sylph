import { expect, test } from "@playwright/test";
import { gotoReady } from "./fixtures/session";

/**
 * Adopting a session an agent already has.
 *
 * # What this suite can and cannot reach, stated rather than papered over
 *
 * The E2E fixture agent has **no native store**: it is an in-process fixture, so there is
 * no vendor writing files for it, and it *refuses* `adopt` and enumerates empty on
 * purpose (`crates/adapters/src/e2e.rs` §
 * `adopt_and_watch_refuse_because_there_is_no_native_store`). Synthesizing a store would
 * make this suite assert against records Sylph itself wrote rather than against a parser,
 * which tests the fixture and retires the question.
 *
 * So a **successful adopt is not covered end to end here**, and cannot be without either
 * a real vendor CLI on the runner or a fabricated store. What is covered is everything the
 * client decides: that the surface opens and is reachable by keyboard, that an empty
 * listing reads as "nothing here" rather than as a failure, that a blocked row is shown
 * with its reason rather than hidden or offered, that choosing a row asks to *follow* the
 * store rather than snapshot it, and that the list is the combobox pattern a screen reader
 * can walk.
 *
 * That `watch` assertion reads the request body, because the flag has no rendered
 * consequence — omitting it still answers `201` and still draws a complete-looking
 * transcript, which then silently stops. `lib/adopt-request.test.ts` owns the payload's
 * shape across rows; the case here is narrower and different in kind: that a click on this
 * surface reaches the endpoint carrying it.
 *
 * The rows are supplied by intercepting `GET /sessions/enumerate`, which is the right seam
 * for exactly that reason: every assertion below is about the **client's** branch on a
 * shape the backend's own tests pin. The success path was verified by hand against the
 * real stores — 1000 `claude`, 1258 `codex`, 531 `pi` sessions readable, one `claude`
 * session adopted at 13623 events and one `codex` rollout at 3 bracketed turns — and those
 * measurements are recorded in `crates/server/src/db/ingest.rs`.
 */

/** One row, with only what a picker renders. */
function row(over: Record<string, unknown> = {}) {
  return {
    agent: "e2e",
    nativeId: "11111111-2222-3333-4444-555555555555",
    workdir: "/tmp",
    fidelity: "blocks",
    id: "local:e2e:11111111-2222-3333-4444-555555555555",
    ...over,
  };
}

async function openPicker(page: import("@playwright/test").Page) {
  await gotoReady(page);
  // Through the palette, which is how a keyboard user reaches it — and which asserts the
  // command is *registered* rather than only that the component renders. `Mod+P` is the
  // palette; `Mod+K` is search.
  await page.keyboard.press("ControlOrMeta+p");
  const palette = page.getByRole("dialog");
  await expect(palette).toBeVisible();
  await palette.getByRole("combobox").fill("adopt");
  // Scoped to the palette and matched loosely, then Enter: the palette's own cursor
  // already sits on the first match, and clicking a row by an exact accessible name
  // couples this to label copy that is not what the test is about.
  await expect(palette.getByRole("option").first()).toContainText(/adopt/i);
  await page.keyboard.press("Enter");
}

test("the picker is reachable from the palette and lists what the machine holds", async ({
  page,
}) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({
      json: {
        sessions: [
          row({ title: "Fix the SSE reconnect loop", eventCount: 42 }),
          row({
            nativeId: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            id: "local:e2e:aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            workdir: "/tmp/other",
          }),
        ],
      },
    }),
  );

  await openPicker(page);

  const dialog = page.getByRole("dialog", { name: "Adopt an existing session" });
  await expect(dialog).toBeVisible();

  // The vendor's own title where it kept one.
  await expect(page.getByRole("option", { name: /Fix the SSE reconnect loop/ })).toBeVisible();
  // And a short id where it did not — the honest label, since `codex` and `pi` record no
  // title and their transcripts carry no user text for a fallback to read.
  await expect(page.getByRole("option", { name: /Untitled session aaaaaaaa/ })).toBeVisible();
});

test("an already-adopted row is shown with its reason rather than hidden", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({
      json: {
        sessions: [
          row({ title: "Already here", blocked: "alreadyAdopted" }),
          row({
            nativeId: "no-dir",
            id: "local:e2e:no-dir",
            title: "Nowhere to run",
            workdir: undefined,
            blocked: "noWorkdir",
          }),
          row({
            nativeId: "outside",
            id: "local:e2e:outside",
            title: "Out of bounds",
            workdir: "/etc",
            blocked: "outsideRoots",
          }),
        ],
      },
    }),
  );

  await openPicker(page);

  // Hiding these would leave a user hunting for a session plainly on the machine;
  // offering them would hand out a 409 nobody can act on. So all three are on screen,
  // each saying which.
  await expect(page.getByRole("option", { name: /Already here/ })).toBeVisible();
  await expect(page.getByRole("option", { name: /Already here/ })).toContainText(
    /Already added/,
  );
  await expect(page.getByRole("option", { name: /Nowhere to run/ })).toContainText(
    /No recorded directory/,
  );
  await expect(page.getByRole("option", { name: /Out of bounds/ })).toContainText(
    /Outside permitted directories/,
  );
});

test("an already-adopted row opens the session instead of adopting it again", async ({
  page,
}) => {
  const id = "local:e2e:already-adopted-1";
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({
      json: {
        sessions: [row({ nativeId: "already-adopted-1", id, title: "Already here", blocked: "alreadyAdopted" })],
      },
    }),
  );

  // Nothing may POST. The useful action for a row Sylph already holds is to open it, and
  // adopting would be a 409 the user cannot act on.
  let adopted = false;
  await page.route("**/api/v1/sessions/adopt", async (route) => {
    adopted = true;
    await route.abort();
  });

  await openPicker(page);
  await page.getByRole("option", { name: /Already here/ }).click();

  // Navigated to the session it names. Asserted on identity — the encoded triple — rather
  // than on a count or a position, because the shared journal holds whatever earlier specs
  // created.
  await expect(page).toHaveURL(new RegExp(encodeURIComponent(id).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  expect(adopted, "an already-adopted row must not be re-adopted").toBe(false);
});

test("an empty listing reads as nothing here rather than as a failure", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );

  await openPicker(page);

  // A machine with the CLIs installed and no sessions genuinely has none. This is also
  // the state the real fixture agent always reports, since it has no native store.
  await expect(page.getByText("No sessions found on this machine.")).toBeVisible();
  // Scoped to the dialog. A bare `getByRole("alert")` on the page matches Next's own
  // `__next-route-announcer__`, which is always mounted — so the assertion would be
  // coupled to framework internals rather than to this surface, and fails for a reason
  // that has nothing to do with adopting.
  await expect(
    page.getByRole("dialog", { name: "Adopt an existing session" }).getByRole("alert"),
  ).toBeHidden();
});

test("a listing failure is reported with a retry rather than as an empty machine", async ({
  page,
}) => {
  // **Both of the first two attempts fail, and that is not belt-and-braces.** The query
  // client retries a 5xx exactly once (`lib/providers.tsx`), so failing only the first
  // attempt lets the retry succeed and the error state never renders — the spec would then
  // pass or fail on the client's retry policy rather than on the branch under test.
  let attempts = 0;
  await page.route("**/api/v1/sessions/enumerate*", async (route) => {
    attempts += 1;
    if (attempts <= 2) {
      await route.fulfill({
        status: 503,
        json: { error: { code: "ENVIRONMENT_UNREACHABLE", message: "not connected" } },
      });
      return;
    }
    await route.fulfill({ json: { sessions: [row({ title: "Back online" })] } });
  });

  await openPicker(page);

  // "Could not read" and "nothing to read" are opposite facts, and an empty list for the
  // first is what makes an unreachable machine look like an empty one.
  await expect(page.getByRole("dialog", { name: "Adopt an existing session" }).getByRole("alert")).toContainText(/Could not read this machine/);
  await page.getByRole("button", { name: "Retry" }).click();
  await expect(page.getByRole("option", { name: /Back online/ })).toBeVisible();
});

test("choosing a row asks to follow the store rather than snapshot it", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "Still running in a terminal" })] } }),
  );

  // The request body, which is the only place this decision is observable: `watch` is
  // optional on the wire and `false` by default, so omitting it yields a 201 and a session
  // that renders exactly like a followed one and then stops updating. Everything the picker
  // draws afterwards is identical either way.
  let body: Record<string, unknown> | null = null;
  await page.route("**/api/v1/sessions/adopt", async (route) => {
    body = route.request().postDataJSON();
    // Refused, so nothing navigates and the assertion stays about the request. `lib/
    // adopt-request.test.ts` covers the payload's own shape; this covers that the picker's
    // click reaches the endpoint carrying it.
    await route.fulfill({
      status: 422,
      json: { error: { code: "NATIVE_SESSION_NOT_RESUMABLE", message: "refused on purpose" } },
    });
  });

  await openPicker(page);
  await page.getByRole("option", { name: /Still running in a terminal/ }).click();

  await expect
    .poll(() => body, { message: "the picker must POST /sessions/adopt" })
    .not.toBeNull();
  expect(body!.watch, "adopting must follow the store as it grows").toBe(true);
});

test("the picker says adopting keeps up with the terminal", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "One" })] } }),
  );

  await openPicker(page);

  // Copy, because the behaviour is not inferable from the verb: "adopt" reads as a one-time
  // import, and a user expecting a snapshot has no reason to leave the terminal running.
  await expect(
    page.getByRole("dialog", { name: "Adopt an existing session" }),
  ).toContainText(/keeps up with the terminal/);
});

test("a refused adopt is rendered rather than swallowed", async ({ page }) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "Will be refused" })] } }),
  );
  // The real refusal an unreadable source produces. A mutation with no error path turns
  // this into a phantom UI bug: the cached list is untouched, the row re-renders from it,
  // and the only observable outcome is that clicking did nothing.
  await page.route("**/api/v1/sessions/adopt", (route) =>
    route.fulfill({
      status: 422,
      json: {
        error: {
          code: "NATIVE_SESSION_NOT_RESUMABLE",
          message: "That native session recorded no working directory",
        },
      },
    }),
  );

  await openPicker(page);
  await page.getByRole("option", { name: /Will be refused/ }).click();

  await expect(page.getByRole("dialog", { name: "Adopt an existing session" }).getByRole("alert")).toContainText(/no working directory/);
  // And the surface stays up, because the user has other rows to choose from.
  await expect(page.getByRole("dialog", { name: "Adopt an existing session" })).toBeVisible();
});

test("the list is a combobox over a listbox, and the cursor is announced from the input", async ({
  page,
}) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({
      json: {
        sessions: [
          row({ title: "First session" }),
          row({ nativeId: "second", id: "local:e2e:second", title: "Second session" }),
        ],
      },
    }),
  );

  await openPicker(page);

  // A filterable list is the combobox pattern, not a `role="menu"`: a menu may only
  // contain menu items, so a text field inside one is invalid ARIA that leaves a screen
  // reader with a control its container says cannot exist.
  const input = page.getByRole("combobox", { name: /native sessions/i });
  await expect(input).toBeVisible();

  // `aria-activedescendant` belongs on the **focused control**, not on the list it names.
  // axe passes either way; a screen reader announces nothing when it sits on the popup,
  // because the cursor moves without focus moving and the announcement follows focus.
  await input.press("ArrowDown");
  await expect(input).toHaveAttribute("aria-activedescendant", /.+/);
});

/**
 * The discoverability paths, which are the whole reason the picker is reachable at all.
 *
 * The palette entry above is a keyboard route, and it was the *only* route: opening the
 * adopt picker required knowing to press the palette shortcut and search "adopt". These
 * two specs pin the surfaces that lead a user there without already knowing.
 *
 * Both assert on the **journey**, not on the picker's internals — that a real click in
 * Settings and a real click in an empty history each land on the adopt dialog. The picker's
 * own behaviour is covered above and must not be re-asserted here, or one broken row would
 * fail two suites for two different reasons.
 */
test("settings offers each connected machine a way into its own native sessions", async ({
  page,
}) => {
  // Enumerate is stubbed so this spec fails only for its own reason. The real fixture
  // agent enumerates empty by design, which would still open the dialog — but a row on
  // screen is what proves the picker was handed a usable environment rather than opening
  // scoped to nothing.
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "Found on this machine" })] } }),
  );

  await gotoReady(page);
  await page.keyboard.press("ControlOrMeta+Comma");
  const settings = page.getByRole("dialog");
  await expect(settings).toBeVisible();
  await settings.getByRole("button", { name: "Machines" }).click();

  // The affordance is per machine, and its accessible name says which — every connected
  // row renders one, so a bare "Browse native sessions" would be several controls a
  // screen reader user cannot tell apart.
  const browse = settings.getByRole("button", { name: /Browse native sessions on / });
  await expect(browse.first()).toBeVisible();
  await browse.first().click();

  // Settings is replaced rather than stacked: one overlay at a time, so exactly one focus
  // trap is ever mounted.
  const picker = page.getByRole("dialog", { name: "Adopt an existing session" });
  await expect(picker).toBeVisible();
  await expect(page.getByRole("option", { name: /Found on this machine/ })).toBeVisible();

  // Opening it moved focus into the surface, which is what makes the overlay dismissable
  // by keyboard at all (WCAG 2.1.2) — the filter field claims it.
  await expect(picker.locator(":focus")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(picker).toBeHidden();
});

test("an empty workspace history points at adoption instead of reading as no work here", async ({
  page,
}) => {
  // The defect: `GET /sessions` returns only what Sylph adopted, so a directory whose
  // history lives in the vendor's own store correctly answers `[]` — and the empty state
  // then said "No sessions here yet", which a user with fifty terminal sessions in that
  // directory reads as Sylph being unable to see their work.
  await page.route("**/api/v1/sessions?*", (route) =>
    route.fulfill({ json: { sessions: [] } }),
  );
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "Was in the vendor store" })] } }),
  );

  await gotoReady(page);
  await page.keyboard.press("ControlOrMeta+Shift+k");
  const history = page.getByRole("dialog");
  await expect(history).toBeVisible();

  // The hint states what is true unconditionally — that unimported sessions are not
  // listed — rather than claiming *this* directory has some, which this surface cannot
  // check: a workspace row carries no environment, and enumerating costs seconds per
  // agent. The picker it opens answers that for real.
  await expect(history.getByText(/not listed until they are imported/)).toBeVisible();
  await history.getByRole("button", { name: "Look for sessions on this machine" }).click();

  await expect(page.getByRole("dialog", { name: "Adopt an existing session" })).toBeVisible();
  await expect(page.getByRole("option", { name: /Was in the vendor store/ })).toBeVisible();
});

test("the agent filter is buttons, not a select whose width its longest option decides", async ({
  page,
}) => {
  await page.route("**/api/v1/sessions/enumerate*", (route) =>
    route.fulfill({ json: { sessions: [row({ title: "One" })] } }),
  );

  await openPicker(page);

  const filter = page.getByRole("group", { name: "Filter by agent" });
  await expect(filter).toBeVisible();
  // A `<select>` is sized by its longest option and cannot be truncated or styled, and a
  // native session's label is frequently a whole sentence — which is what pushed the
  // composer off the right edge of a phone.
  await expect(filter.locator("select")).toHaveCount(0);

  const all = filter.getByRole("button", { name: "All agents", exact: true });
  await expect(all).toHaveAttribute("aria-pressed", "true");
});
