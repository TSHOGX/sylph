import { expect, test, type Page } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * The agent-first timeline: tool calls as primary content, inline diffs, and
 * expansion policy that follows what a reader needs.
 *
 * These assertions encode design decisions from `docs/agent-ui-design-analysis.md`
 * rather than incidental markup, so a failure means the canvas stopped behaving
 * like an agent workspace — not that a class name changed.
 */

async function runPrompt(page: Page, prompt: string) {
  await createSession(page);
  await page.getByLabel("Message").fill(prompt);
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
}

/**
 * The wide fan-out, and the wait that actually means it finished.
 *
 * The `fanout` scenario spends ~2.6s making one delegate measurably slower than its
 * siblings, so the reply arrives long after the first card does. Waiting on the
 * slowest delegation's *close* is what makes an assertion about durations meaningful:
 * waiting on the reply alone would sometimes read the tree mid-fan-out, where the
 * slowest row is still running and has no duration at all.
 */
async function sendFanout(page: Page) {
  await createSession(page);
  await page.getByLabel("Message").fill("fanout across agents please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Fanning out to six agents.")).toBeVisible({
    timeout: 15_000,
  });
}

/**
 * The fan-out, stopped inside its live window.
 *
 * Exact durations exist only *while a turn is live*. Once it settles the transcript is
 * re-read from consolidated history, where a declared trajectory's phases are folded
 * into one row keyed by its close — so the start, and with it every exact duration, is
 * gone. Anything asserting on a real comparison therefore has to assert before that,
 * and the fixture holds the turn open for 1.2s after its reply so the window is real
 * rather than a microsecond race.
 *
 * Waiting on the reply is what pins that Survey has closed; waiting on **Stop** is what
 * pins that the turn has not settled yet. `Send` is useless for the second: it is also
 * disabled whenever the composer is empty, which is exactly the state a spec is in
 * right after sending.
 */
async function runFanoutLive(page: Page) {
  await sendFanout(page);
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible({
    timeout: 15_000,
  });
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
    "the fixture's post-reply hold should still be running",
  ).toBeVisible();
}

/** The fan-out after its turn settles, which is the state a user scrolls back to. */
async function runFanoutSettled(page: Page) {
  await sendFanout(page);
  // The Send/Stop swap back is the settle signal, for the reason above.
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible({
    timeout: 20_000,
  });
  await expect(page.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0);
}

test("tool calls render as work, with target, status, and no bubbles", async ({ page }) => {
  await runPrompt(page, "exercise tools please");

  // Each call is a labelled region, which is what lets a screen reader jump
  // between operations rather than reading one undifferentiated stream.
  await expect(page.getByRole("region", { name: "Read tool call" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Edit tool call" })).toBeVisible();
  const bash = page.getByRole("region", { name: "Bash tool call" });
  await expect(bash).toBeVisible();

  // The target is shown, not just the tool name: "Read" alone says nothing about
  // what was read.
  await expect(page.getByRole("region", { name: "Read tool call" })).toContainText("src/lib.rs");
  await expect(bash).toContainText("cargo test --lib");

  // A failure is announced in text, not by colour alone (WCAG 1.4.1), and carries
  // the exit code because that is the first thing anyone wants from a failed run.
  await expect(bash).toContainText("failed");
  await expect(bash).toContainText("exit 101");
});

test("an edit renders as an inline diff, and a failed command auto-expands", async ({
  page,
}) => {
  await runPrompt(page, "exercise tools please");

  // The diff is computed in the browser from the edit's before/after strings, so
  // both sides must appear with their change markers.
  const edit = page.getByRole("region", { name: "Edit tool call" });
  await expect(edit).toContainText('println!("before");');
  await expect(edit).toContainText('tracing::info!("after");');
  // One line replaced: one addition, one removal.
  await expect(edit).toContainText("+1");
  await expect(edit).toContainText("−1");
  // The unchanged surrounding lines are context, so they are shown too.
  await expect(edit).toContainText("fn main() {");

  // A failing command's output is the answer to "what went wrong", so it is
  // visible without interaction.
  const bash = page.getByRole("region", { name: "Bash tool call" });
  await expect(bash).toContainText("cannot find value `after`");

  // A successful read is context-gathering, so its output stays collapsed.
  const read = page.getByRole("region", { name: "Read tool call" });
  await expect(read).not.toContainText('println!("before")');
  await read.getByRole("button", { name: "more" }).click();
  await expect(read).toContainText('println!("before")');
});

test("assistant prose renders Markdown without chat bubbles", async ({ page }) => {
  await runPrompt(page, "exercise tools please");

  // `**real**` in the runner's reply must render as emphasis, not literal
  // asterisks — the Markdown path is what makes agent prose readable.
  await expect(page.locator("strong", { hasText: "real" })).toBeVisible();
});

/**
 * Sibling comparison across a fan-out.
 *
 * The rollup on a card answers "should I open this branch" and cannot answer "which of
 * these six was slow", because it never compares a node to the node beside it. These
 * specs are written against the *wide* fixture for that reason: at two delegates the
 * answer is visible without an overview, so a two-delegate scenario cannot observe
 * whether the comparison works.
 */
/**
 * Assert the turn is still live, so a closed window fails as itself.
 *
 * This guard was written when exactness was available *only* while a turn was live,
 * because a settled row's start had to be reconstructed. Publishing `startedAtMs`
 * removed that asymmetry — a settled delegate is exact too — so the guard is no longer
 * about exactness. It is still needed, and for the more general reason: these specs
 * assert on live-only states (a running trajectory, an open extent with no end), and
 * every one of them is *absent* once the turn settles.
 *
 * That is the expensive shape. When the correct behaviour is the absence of a signal, a
 * window that closed mid-spec is indistinguishable from the feature working — so the
 * failure presents as a broken overview rather than as a timing problem. Called
 * immediately before each assertion that depends on the window, not only at the top:
 * the window is consumed from both ends, and the whole sequence has to fit inside it
 * rather than merely start there.
 */
async function stillLive(page: Page, what: string) {
  await expect(
    page.getByRole("button", { name: "Stop", exact: true }),
    `the turn settled before ${what}; the fixture's post-reply hold was too short for this run`,
  ).toBeVisible();
}

/** Open the strip and hand back its region. */
async function openOverview(page: Page) {
  const overview = page.getByRole("region", { name: "Delegation overview" });
  await expect(overview).toBeVisible();
  // Closed by default, and the toggle states the count so opening it is informed
  // rather than a surprise.
  const toggle = overview.getByRole("button", { name: /Compare \d+ delegations/ });
  await expect(toggle).toBeVisible();
  await toggle.click();
  return overview;
}

test("a fan-out reports which delegation was the slow one", async ({ page }) => {
  test.slow(); // The fixture spends ~2.9s making one delegate genuinely slower.
  await runFanoutLive(page);

  const overview = await openOverview(page);
  await stillLive(page, "the slowest row could be read");

  // The answer, on the row it belongs to. `Survey` is the outlier the fixture builds;
  // asserting the label rather than a bar width is what keeps this about the claim
  // rather than about pixels.
  const slowest = overview.getByRole("button", { name: /^Survey,/ });
  await expect(slowest).toContainText("slowest");
  // And exactly one row claims it: two rows both marked slowest is a tie reported as
  // a winner. `exact` matters here — Playwright's text matching is a substring match,
  // so a bare "slowest" also matches the "slowest first" ordering control beside it,
  // which made this count one whether any row was badged or not.
  await expect(overview.getByText("slowest", { exact: true })).toHaveCount(1);

  // Failure and slowness are different facts, and the fixture puts them on different
  // rows on purpose — a view that conflated them would look correct whenever they
  // coincide.
  const failed = overview.getByRole("button", { name: /^Typecheck, failed/ });
  await expect(failed).toBeVisible();
  await expect(failed).not.toContainText("slowest");
});

/**
 * What the same fan-out says once its turn settles.
 *
 * This is the state a user scrolls back to, and it is the state that used to lose its
 * figures. Consolidation folds a declared trajectory's phases into one row keyed by its
 * close, so `createdAtMs` becomes the *end* — and while the block's own
 * `startedAtMs` went unpublished, every settled duration had to be reconstructed from
 * whatever the interior happened to forward. That made them lower bounds, and a rank
 * over lower bounds is not a rank: a delegate that ran 9s and forwarded one event at
 * 8.9s bounds at 0.1s, so ranking bounds would answer this view's one question
 * confidently and wrongly.
 *
 * The field is now on the wire, so a settled delegate is exact and rankable. What
 * remains withheld is narrower and still worth pinning: a trajectory that never
 * finished has an end timestamp without an outcome, so its arithmetic is exact while
 * its span is not, and one such row withholds the badge from the whole set.
 */
test("a settled fan-out reports exact durations and withholds the rank for the one row that never finished", async ({
  page,
}) => {
  test.slow();
  await runFanoutSettled(page);

  const overview = await openOverview(page);

  // This spec used to assert the opposite — that a settled fan-out degrades to lower
  // bounds — and that was true only because `startedAtMs` was computed, stored, and
  // never published. A consolidated block is keyed on its close, so without that field
  // a settled trajectory reported when it ended and not when it began, and the start
  // had to be reconstructed from whatever the interior happened to forward. Now the
  // block's own start is on the wire, so a settled delegate is exact.
  await expect(overview).not.toContainText(/lower bound/);
  await expect(
    overview.getByRole("button", { name: /^Survey, .*measured from the work it forwarded/ }),
  ).toHaveCount(0);

  // Exact figures, and the case that matters most: a delegate that forwarded nothing is
  // now timed anyway, because the block records its own start rather than depending on
  // it having spoken. Previously this row could only ever be a caret.
  //
  // Matched as text, not as a button — such a row is deliberately not a control, since
  // focusing it would re-root onto a pane with nothing in it.
  await expect(
    overview.getByText(/^Audit, finished, took \d/),
  ).toBeVisible();

  // But the rank is still withheld, for a different and narrower reason. `Cutoff` was
  // still open when the turn ended, so it has an end timestamp without anyone having
  // reported an outcome: the subtraction is exact and the span is not. Its real
  // duration can only be longer, so it cannot lose this comparison and must not win
  // it — and a row labelled "never finished" was previously eligible to be called "the
  // longest here". One unrankable row withholds the badge from every row, because a
  // partial rank is still a claim about the whole set.
  await expect(overview).toContainText(/never finished/);
  await expect(overview.getByText("slowest", { exact: true })).toHaveCount(0);
  await expect(
    overview.getByRole("button", { name: /the longest here/ }),
  ).toHaveCount(0);
});

test("the overview is legible without seeing the bars", async ({ page }) => {
  test.slow();
  await runFanoutLive(page);

  const overview = await openOverview(page);
  await stillLive(page, "the accessible names could be read");

  // Each row's accessible name carries what its bar shows. A purely visual overview
  // excludes the reader who most needs a summary, so the durations, the ordering
  // evidence, and the caveats all have to survive with the bars removed.
  const slowest = overview.getByRole("button", { name: /^Survey,/ });
  await expect(slowest).toHaveAttribute("aria-label", /finished/);
  await expect(slowest).toHaveAttribute("aria-label", /took \d/);
  await expect(slowest).toHaveAttribute("aria-label", /the longest here/);

  // A delegate that forwarded nothing says so, and is not a control: focusing it would
  // re-root onto an empty pane.
  await expect(overview.getByText(/^Audit, .*forwarded none of its own events/)).toBeVisible();
  await expect(overview.getByRole("button", { name: /^Audit,/ })).toHaveCount(0);

  // Nesting is reported, so a deep row is not read as a sibling of the turn.
  await expect(
    overview.getByText(/^Deep probe, .*nested 1 deep/),
  ).toBeVisible();
});

test("a delegation can be reached from the overview by keyboard", async ({ page }) => {
  test.slow();
  await runFanoutLive(page);

  const overview = await openOverview(page);

  // Ordering is a control, not a static label: which order is in effect changes what
  // the list means, so it is announced rather than shown by colour alone.
  const slowestFirst = overview.getByRole("button", { name: "slowest first" });
  await expect(slowestFirst).toHaveAttribute("aria-pressed", "false");
  await slowestFirst.press("Enter");
  await expect(slowestFirst).toHaveAttribute("aria-pressed", "true");

  // Longest first, so the slow one is the first row in the list rather than something
  // to be found among twenty overlapping bars.
  //
  // Guarded, and this one is not about exactness — it is about the *press* surviving.
  // When the turn settles the transcript swaps from the live stream to consolidated
  // history, which re-mounts the strip: `order` resets to `structure` and this click is
  // gone. In structure order rows follow the tree, and after consolidation a block is
  // keyed by its *close*, so an early-closing delegate precedes the slow one — the
  // first row is then legitimately not Survey. It presented as the ordering control not
  // working, which is three layers from a re-mount. An earlier version of this spec was
  // left unguarded on the reasoning that ordering holds for bounds too; it does, but the
  // pressed order does not.
  await stillLive(page, "the pressed ordering could be read");
  const rows = overview.getByRole("listitem");
  await expect(rows.first()).toContainText("Survey");

  // And the row re-roots the timeline, which is the whole point of finding it.
  await overview.getByRole("button", { name: /^Survey,/ }).press("Enter");
  await expect(page.getByRole("navigation", { name: "Delegation path" })).toBeVisible();
  await expect(
    page.getByRole("navigation", { name: "Delegation path" }),
  ).toContainText("Survey");
});

/**
 * A collapsed delegation reports its conclusion, not only that it happened.
 *
 * The backend records a delegate's summary — `task_notification` carries it — and the
 * card rendered neither it nor the progress snapshot, so the account was stored and
 * never shown. That is the one line of *content* a collapsed row can carry: without it
 * the row is a status and a set of counts, which say work happened and nothing about
 * what came of it. It matters most where a late outcome arrives on a turn of the
 * agent's own, because then the summary is the whole of what a user learns about work
 * that finished after the turn that dispatched it.
 */
test("a collapsed delegation shows what it concluded, without being opened", async ({
  page,
}) => {
  test.slow();
  await runFanoutSettled(page);

  // Collapsed — nothing has been clicked — and the summary is on the card.
  const survey = page.getByRole("region", { name: "Survey delegation" });
  await expect(survey).toContainText("surveyed 412 files");
  // Still collapsed, so this is the summary and not the opened interior.
  await expect(survey).not.toContainText("Survey is starting.");

  // A sibling's summary must not appear on it: the report is per-delegation, and a
  // rollup-style leak would caption every row with the last one to arrive.
  await expect(survey).not.toContainText("formatted");
  await expect(page.getByRole("region", { name: "Lint delegation" })).toContainText("clean");
});

/**
 * A delegation whose name never arrived still gets a card and a row.
 *
 * The case a real `claude` session produces constantly — a delegation's brief arrives as
 * streamed partial JSON, so at `started` there is nothing to name it with — and which no
 * fixture reached until now: all twelve `subagent` opens in this suite carried a name, so
 * the client's fallback was covered by unit tests and by nothing that renders.
 *
 * It is also what makes five `exact: true` locators in this suite load-bearing rather
 * than merely correct. The fallback label is "Subagent delegation", which *contains*
 * "agent delegation" — so `{ name: "Agent delegation" }` is ambiguous with it, which
 * cost two real failures. Before this row, no test could produce the string those guards
 * exist for.
 */
test("a delegation with no name still renders, and does not shadow a named one", async ({
  page,
}) => {
  test.slow();
  await runFanoutSettled(page);

  // The fallback reached the DOM as a real card, not as an empty or dropped row.
  await expect(
    page.getByRole("region", { name: "Subagent delegation", exact: true }),
  ).toBeVisible();

  // And the guard works: an exact locator for a *named* delegation must not pick up the
  // fallback, even though "Subagent delegation" contains its text. Without `exact` this
  // resolves to two elements and fails strict mode — which is the failure mode the
  // `exact: true` fixes across this suite were added for, now actually reachable.
  const overview = await openOverview(page);
  await expect(overview.getByRole("button", { name: /^Subagent,/ })).toHaveCount(0);
  // Nameless *and* forwarded nothing, so it is not navigable — the same rule the card
  // follows. It still appears, because a row that reported nothing must not vanish.
  await expect(overview.getByText(/^Subagent, /)).toBeVisible();
});

test("a promoted host reports its outcome, not its own arguments", async ({ page }) => {
  await createSession(page);
  await page.getByLabel("Message").fill("promote this please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Running a workflow.")).toBeVisible({ timeout: 15_000 });
  // Waited to settle, so the tree is stable before it is read. Mid-stream this scenario
  // is still opening and closing four hosts, and a card read then can be a row that a
  // later phase reshapes.
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeVisible({
    timeout: 20_000,
  });

  // A promoted host's `text` is its tool call's serialized input, so reading `text`
  // uniformly would caption the card with a JSON blob. Its outcome arrives as the
  // `subagent` close held beside the row — the field that exists so promotion does not
  // overwrite the call. `Agent` is the host that forwards no interior, which is exactly
  // where this summary is the only thing anyone can learn.
  //
  // `exact` is not optional here: accessible-name matching is a *substring* match, so
  // "Agent delegation" also matches "Subagent delegation" — the fallback name a
  // trajectory renders when it has none. This scenario can produce one, because
  // `pr-host-b`'s close is a `subagent` event carrying text and no name, so a build that
  // ever sees that close without its `tool_call` row makes an unnamed row from it. It
  // cost a strict-mode failure that read as the summary being absent.
  const agent = page.getByRole("region", { name: "Agent delegation", exact: true });
  await expect(agent).toContainText("reported nothing");
  await expect(agent).not.toContainText("summarize");
});

/**
 * The overview, audited by axe.
 *
 * Written here rather than in `accessibility.spec.ts` because the surface needs the
 * `fanout` fixture and its live window, which that file's delegation audit does not
 * set up. Two states are audited because they are different trees: a `region` with an
 * `aria-pressed` group and an `ol` of controls, and the same list with a
 * non-navigable row that carries its name as text instead of as a label — naming is
 * prohibited on a paragraph, so `aria-label` there is a violation axe reports and a
 * screen reader may ignore.
 */
test("the overview has no automated accessibility violations", async ({ page }) => {
  test.slow();
  await runFanoutLive(page);
  const overview = await openOverview(page);
  await expect(overview.getByRole("button", { name: /^Survey,/ })).toBeVisible();

  const { default: AxeBuilder } = await import("@axe-core/playwright");
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"])
    .analyze();

  const violations = results.violations.map((violation) => ({
    id: violation.id,
    impact: violation.impact,
    nodes: violation.nodes.slice(0, 3).map((node) => ({
      target: node.target.join(" "),
      summary: node.failureSummary?.replace(/\s+/g, " ").slice(0, 240),
    })),
  }));

  expect(
    violations,
    `axe violations on the delegation overview: ${JSON.stringify(violations, null, 2)}`,
  ).toEqual([]);
});

/**
 * The same overview at 412px.
 *
 * This project runs at 1280px, where a bar chart is merely roomy. The failure this
 * guards is the one a desktop viewport cannot see: a row whose label, badges, and
 * duration overflow the viewport, which is how a `<select>` and an un-truncated name
 * broke the composer on a phone. The viewport is set here rather than in
 * `mobile.spec.ts` because that file is the mobile *project*, and these assertions
 * belong beside the rest of the overview's.
 */
test("the overview fits a phone without overflowing it", async ({ page }) => {
  test.slow();
  await page.setViewportSize({ width: 412, height: 915 });
  // Settled rather than live: geometry is what this asserts, and a row that is still
  // streaming can change width between the measurement and the comparison.
  await runFanoutSettled(page);

  const overview = await openOverview(page);
  await expect(overview.getByRole("button", { name: /^Survey,/ })).toBeVisible();

  // Nothing extends past the right edge. Asserted as an invariant rather than as a
  // pixel width, so it keeps holding as the design moves.
  const overflow = await overview.evaluate((node: HTMLElement) => {
    const limit = document.documentElement.clientWidth;
    return Array.from(node.querySelectorAll("*"))
      .filter((child) => child.getBoundingClientRect().right > limit + 1)
      .map((child) => child.className)
      .slice(0, 3);
  });
  expect(overflow, "overview children wider than the viewport").toEqual([]);

  // The bars still share one origin at this width: an indented track would offset a
  // nested bar by its depth and destroy the only comparison the view makes.
  const origins = await overview.evaluate((node: HTMLElement) =>
    Array.from(node.querySelectorAll("li > span[aria-hidden]")).map((track) =>
      Math.round(track.getBoundingClientRect().left),
    ),
  );
  expect(new Set(origins).size, `track origins: ${origins.join(",")}`).toBe(1);
});

/**
 * Render cost for a long conversation.
 *
 * The Phase 6 exit criterion is a 1000-message conversation rendering in under
 * two seconds. The bulk prompt journals 1000 events (250 iterations × 4), and the
 * measurement is taken on a reload so it covers a cold render of durable history
 * rather than incremental streaming — the slower and more meaningful direction.
 *
 * The budget here is deliberately the spec's 2s and not a tighter number: this
 * runs on whatever CI machine is available, and a threshold tuned to a fast
 * laptop would fail for reasons unrelated to the code.
 */
test("a 1000-event conversation renders within the performance budget", async ({ page }) => {
  test.slow(); // Journalling 1000 events through the supervisor is not instant.

  await runPrompt(page, "bulk render please");

  // Confirm the fixture really produced the volume the budget is about, so the
  // measurement cannot silently pass against a short transcript.
  //
  // Polled rather than counted once: the transcript now paints incrementally, so
  // the reply block that `runPrompt` waits for appears while rows are still
  // arriving. A single count here races the stream and reads a partial log.
  await expect
    .poll(
      () => page.getByRole("region", { name: /tool call/ }).count(),
      { message: "bulk fixture should emit 250 tool calls", timeout: 30_000 },
    )
    .toBeGreaterThanOrEqual(250);

  const started = Date.now();
  await page.reload();
  // The last emitted block is the reply, so seeing it means the whole transcript
  // has been laid out.
  await expect(page.getByText("Hello from the E2E runner.")).toBeVisible();
  await expect(page.getByText("Step 250:")).toBeVisible();
  const elapsed = Date.now() - started;

  expect(
    elapsed,
    `cold render of a 1000-event conversation took ${elapsed}ms`,
  ).toBeLessThan(2000);
});
