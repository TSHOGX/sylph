import { expect, test } from "@playwright/test";
import { createSession } from "./fixtures/session";

/**
 * Delegated trajectories: does the timeline show the tree the agent actually ran?
 *
 * Before `span` existed, every one of these assertions was unwritable. A
 * subagent's thinking, prose, and tool calls arrived indistinguishable from the
 * parent's own, so the only observable difference between "the agent did this" and
 * "an agent it delegated to did this" was nothing at all.
 *
 * The `delegate` scenario emits the shape that catches the failures a shallow
 * fixture cannot: two trajectories running concurrently with interleaved events,
 * one nested three deep, a failure buried at the bottom, and one left open so the
 * server has to close it. Each test below fails on a different mistake, which is
 * why they are separate rather than one walkthrough.
 */

async function runDelegation(page: import("@playwright/test").Page) {
  await createSession(page);
  await page.getByLabel("Message").fill("delegate this please");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  // The dispatch line precedes every trajectory, so waiting on it means the turn
  // is genuinely under way rather than the page merely being loaded.
  await expect(page.getByText("Fanning out to two agents.")).toBeVisible({
    timeout: 15000,
  });
}

test("each delegation is one card, named for the agent rather than the tool", async ({
  page,
}) => {
  await runDelegation(page);

  // Named regions, not text: a card is a landmark a screen reader can navigate to,
  // and asserting on the accessible name is what pins that it has one.
  await expect(page.getByRole("region", { name: "Explore delegation" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Verify delegation" })).toBeVisible();

  // "Task" is claude's tool name and must never reach the UI — six rows all
  // reading "Task" is the unreadable timeline this design replaces.
  await expect(page.getByRole("region", { name: "Task delegation" })).toHaveCount(0);
});

test("a delegation's work is hidden until asked for, then belongs to that delegation", async ({
  page,
}) => {
  await runDelegation(page);

  const explore = page.getByRole("region", { name: "Explore delegation" });

  // Collapsed by default. This is the performance property as much as the visual
  // one: a closed card mounts none of its subtree, so a twenty-agent fan-out costs
  // twenty rows until someone drills in.
  await expect(explore).not.toContainText("Searching for the call sites.");

  await explore.getByRole("button", { name: "Explore", exact: true }).click();
  await expect(explore).toContainText("Searching for the call sites.");

  // And it is scoped: the sibling's reasoning must not appear here. On a flat
  // stream both were siblings at the root, so this is the assertion that fails
  // when grouping goes by arrival order instead of by `span`.
  await expect(explore).not.toContainText("Planning the verification.");
});

test("a nested delegation is a card inside a card, to the depth the agent used", async ({
  page,
}) => {
  await runDelegation(page);

  const verify = page.getByRole("region", { name: "Verify delegation" });

  // Depth 3 — turn, Verify, Worker — which is the dynamic-workflow shape: a phase
  // that itself delegates. A flat "list of subagents" model passes at depth 1 and
  // has nowhere to put this.
  //
  // Verify is already open, and not because a click opened it: a failed branch
  // expands itself, all the way down to the tool that failed. That is the same
  // policy a failed shell command follows, and it is what keeps the rollup from
  // being the end of the trail — the counts say a failure is in there, and the
  // open branch says where.
  const worker = verify.getByRole("region", { name: "Worker delegation" });
  await expect(worker).toBeVisible();
  await expect(worker).toContainText("cargo test --lib");

  // The clean sibling stays shut, which is what makes the auto-open meaningful.
  const explore = page.getByRole("region", { name: "Explore delegation" });
  await expect(explore.getByRole("button", { name: "Explore", exact: true })).toHaveAttribute(
    "aria-expanded",
    "false",
  );
});

test("a failure three levels down is reported on the collapsed card above it", async ({
  page,
}) => {
  await runDelegation(page);

  // Verify's own result was clean; its grandchild's Bash call failed. A green card
  // over that is the worst possible outcome of collapsing by default — the user
  // would have to open every branch to learn something went wrong, which is the
  // work collapsing was supposed to save.
  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toContainText(/failed|failures/);

  // The sibling that genuinely succeeded must not be marked. A rollup that
  // escalated every card would be as useless as one that escalated none.
  const explore = page.getByRole("region", { name: "Explore delegation" });
  await expect(explore).not.toContainText(/failed|failures/);
});

test("a collapsed card says what it is hiding", async ({ page }) => {
  await runDelegation(page);

  // The justification for hiding it: the counts have to make the subtree legible
  // enough to decide whether to open it.
  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toContainText("1 nested");
  await expect(verify).toContainText(/\d+ tools?/);
});

test("a delegation the agent never closed is resolved when the turn ends", async ({
  page,
}) => {
  await runDelegation(page);

  // The fixture opens this one and never closes it. Without the server closing
  // open trajectories on settle, the card spins forever — a permanent "running"
  // on finished work, which reads as a hung session.
  const stranded = page.getByRole("region", { name: "Stranded delegation" });
  await expect(stranded).toBeVisible();
  await expect(stranded).toContainText("never finished", { timeout: 15000 });

  // Not "failed": nobody said it failed, only that nobody said. Reporting an
  // interrupted delegation as a broken one is a different claim.
  await expect(stranded).not.toContainText("failed");
});

test("focusing a delegation re-roots the timeline and offers the way back", async ({
  page,
}) => {
  await runDelegation(page);

  const verify = page.getByRole("region", { name: "Verify delegation" });
  await verify.getByRole("button", { name: "Focus Verify" }).click();

  // Re-rooted: Verify's child is now top level, and the turn's own prose is out of
  // view. This is what makes unbounded depth navigable rather than displayable —
  // at four levels there is no indentation budget left to spend.
  const path = page.getByRole("navigation", { name: "Delegation path" });
  await expect(path).toBeVisible();
  await expect(page.getByRole("region", { name: "Worker delegation" })).toBeVisible();
  await expect(page.getByText("Fanning out to two agents.")).toBeHidden();

  // And back out, by the breadcrumb rather than the browser.
  await path.getByRole("button", { name: "Turn" }).click();
  await expect(page.getByText("Fanning out to two agents.")).toBeVisible();
});

test("delegation structure survives a reload", async ({ page }) => {
  await runDelegation(page);

  // Structure has to be journaled, not projected. A `subagent` `started` is where a
  // trajectory's id, name, and parent live, so if the storage tier dropped it as an
  // ephemeral phase the whole subtree would flatten into the parent's own work on
  // reload — the pre-`span` bug, reintroduced one layer down. It also has to survive
  // *consolidation*, which folds a block's phases into one row: that path kept only
  // the closing event's fields, and a trajectory is named when it opens.
  await page.reload();

  const verify = page.getByRole("region", { name: "Verify delegation" });
  await expect(verify).toBeVisible({ timeout: 15000 });
  // Named, not "Subagent": the name came from a phase that consolidation folds away.
  await expect(page.getByRole("region", { name: "Explore delegation" })).toBeVisible();
  await expect(verify.getByRole("region", { name: "Worker delegation" })).toBeVisible();

  // And the nesting is still nesting, not three cards side by side.
  await expect(page.getByRole("region", { name: "Worker delegation" })).toHaveCount(1);
  await expect(verify).toContainText("1 nested");
});

/**
 * Promotion: delegation by a tool that never declared itself one.
 *
 * `docs/session-protocol.md` § Promotion. Every delegating tool outside claude's
 * built-in `Task` has this shape — `Agent`, `Workflow`, anything an MCP server
 * adds — and none of it was covered, which is how two real workflow cards shipped
 * reading *running* permanently with no visible interior.
 *
 * The `promote` scenario emits three hosts on purpose: one multiplexing two
 * delegates with a failure in the second, one reporting no interior at all, and one
 * left open so only the server closes it. A host with a single delegate cannot
 * observe the collapse this exists to catch.
 */
async function runPromotion(page: import("@playwright/test").Page) {
  await createSession(page);
  await page.getByLabel("Message").fill("promote this workflow");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Running a workflow.")).toBeVisible({ timeout: 15000 });
}

/**
 * Open every `Workflow` host, then return the one containing `delegate`.
 *
 * Two reasons this is a helper rather than a locator. A collapsed card mounts none
 * of its subtree, so `filter({ has: <descendant> })` matches nothing until the host
 * is open — and a card auto-opens only when its status is `failed` *at mount*, which
 * during a stream is before its delegate's failure has arrived. So expansion is
 * timing-dependent, and a spec that relies on it passes or fails on render order.
 *
 * Selecting by contained delegate rather than by position is the other half: three
 * of the fixture's hosts are named `Workflow`, so `.last()` and `.nth()` silently
 * retarget whenever the scenario grows one. A positional selector that keeps passing
 * while pointing at the wrong row is worse than one that fails.
 */
async function workflowHosting(
  page: import("@playwright/test").Page,
  delegate: string,
) {
  const hosts = page.getByRole("region", { name: /Workflow delegation/ });
  const count = await hosts.count();
  for (let i = 0; i < count; i += 1) {
    const disclosure = hosts.nth(i).getByRole("button", { name: "Workflow", exact: true });
    if ((await disclosure.getAttribute("aria-expanded")) !== "true") {
      await disclosure.click();
    }
  }
  return hosts.filter({
    has: page.getByRole("region", { name: `${delegate} delegation` }),
  });
}

test("every delegate under one host gets its own card", async ({ page }) => {
  await runPromotion(page);

  // The measured bug: keyed by the host, two delegates collapsed into one row and
  // the last name won — "Demo: agent-1" was absent from the timeline entirely.
  const host = await workflowHosting(page, "Demo: agent-1");
  await expect(host).toHaveCount(1);
  await expect(host.getByRole("region", { name: "Demo: agent-1 delegation" })).toBeVisible();
  await expect(host.getByRole("region", { name: "Demo: agent-2 delegation" })).toBeVisible();
});

test("a delegating tool nobody declared still contains its delegates", async ({
  page,
}) => {
  await runPromotion(page);

  // The host is a `tool_call`; its children name it as their span, which is the
  // whole of what makes it their container. Nesting, not adjacency: the delegate
  // must be *inside* the host's region, or the delegation reads as flat.
  const host = page.getByRole("region", { name: /Workflow delegation/ }).first();
  // `exact` matters: the card also carries a "Focus Workflow" control, and
  // `getByRole` name matching is a substring match, so a bare "Workflow" is
  // ambiguous between the disclosure and the focus button.
  const disclosure = host.getByRole("button", { name: "Workflow", exact: true });
  // Toggled only if it is closed. This host's second delegate failed, and a card
  // whose subtree contains a failure opens itself — so an unconditional click
  // *collapses* it, and the assertion below then fails on a tree that was correct.
  if ((await disclosure.getAttribute("aria-expanded")) !== "true") {
    await disclosure.click();
  }
  await expect(
    host.getByRole("region", { name: "Demo: agent-1 delegation" }),
  ).toBeVisible();
});

test("a promoted host reports a failure buried in a delegate while collapsed", async ({
  page,
}) => {
  await runPromotion(page);

  // The rule that makes collapsing deferral rather than loss. `agent-2` failed two
  // levels down; a green host hiding that is worse than not collapsing at all.
  const host = page.getByRole("region", { name: /Workflow delegation/ }).first();
  // Collapsed, so this is read off the rollup rather than the failed row itself —
  // which is the property that makes collapsing deferral rather than loss.
  await expect(host).toContainText("failed");
});

test("a host that forwarded no interior says so instead of offering an empty card", async ({
  page,
}) => {
  await runPromotion(page);

  // A closed trajectory with no children is a terminal state, not a loading one.
  // Presented as expandable it invites opening a branch that can never fill, and
  // saying so only after the click is the lossy collapse the tree exists to avoid.
  const agent = page.getByRole("region", { name: "Agent delegation" });
  await expect(agent).toContainText("no interior");
  // Collapsed, the card says what came back from inside it — nothing — without the
  // user having to open it to find that out. Whether it *can* be opened is a separate
  // question, answered by the spec below: a promoted host still has its own call to
  // show, while a declared trajectory with no interior has nothing at all.
  await expect(agent).not.toContainText("summarize");
});

test("a promoted host left open is closed by the server, not left spinning", async ({
  page,
}) => {
  await runPromotion(page);

  // The path `phase = 'started'` missed entirely: this host's trajectory was opened
  // by a delegate naming it, so the turn-settle sweep never saw it and the card
  // stayed *running* across reloads. Measured on a real session before the fix.
  // The host, not the delegate: the delegate declared itself and was always swept,
  // while its *container* was promoted and is the thing the old query missed.
  //
  // Selected by the delegate it contains rather than by position. `.last()` silently
  // retargeted the moment the fixture grew a fourth host, and a positional selector
  // that still passes while pointing at the wrong row is worse than a failing one.
  const host = await workflowHosting(page, "Stage A: A:item-1");
  await expect(host).toBeVisible();
  // "never finished" is the abandoned wording, kept distinct from "failed": the
  // server closing what an adapter left open means nobody reported an outcome, and
  // colouring that as a failure reports an interrupted turn as a broken one.
  await expect(host).toContainText("never finished", { timeout: 20000 });
  await expect(host).not.toContainText("running");
});

test("a host that dispatched successfully is not failed by a delegate the server closed", async ({
  page,
}) => {
  await runPromotion(page);

  // The reading that was wrong twice over on a real session. The call succeeded and
  // returned; its delegate was still running when the turn ended, so the server
  // closed that. `abandoned` means nobody reported an outcome — counting it as a
  // descendant failure badged a workflow red for finishing exactly as designed.
  const host = await workflowHosting(page, "Barrier: worker-1");
  // `finished`, not `failed`. Asserted on the badge rather than on absence alone,
  // because a card that failed to render at all would also "not contain failed".
  await expect(host).toContainText("finished");
  await expect(host).not.toContainText("failed");

  // And the delegate must not claim "no interior": nobody looked, which is a
  // different fact from a host that finished and forwarded nothing.
  const worker = page.getByRole("region", { name: "Barrier: worker-1 delegation" });
  await expect(worker).toContainText("outlived its turn");
  await expect(worker).not.toContainText("no interior");
});

test("a promoted host shows its own call, not only the delegates under it", async ({
  page,
}) => {
  await runPromotion(page);

  // A promoted host is a tool call *and* a container, and the row keeps
  // `kind: "tool_call"` so its name, arguments, and result survive. The card rendered
  // only trajectory chrome, so a `Workflow` showed a name and a rollup and nothing
  // else — opening or focusing one answered with an empty pane, which is what made a
  // correctly-built tree look broken. The script is the part that matters: it is what
  // defines what the delegates are.
  const host = await workflowHosting(page, "Demo: agent-1");
  await expect(host).toContainText("parallel");
});

test("a host with no forwarded interior is still worth opening", async ({ page }) => {
  await runPromotion(page);

  // `no interior` says nothing came back from inside. That is not the same as nothing
  // to show: the call itself is still there, so this one keeps its disclosure while a
  // *declared* trajectory with no interior does not have one to keep.
  const agent = page.getByRole("region", { name: "Agent delegation" });
  await expect(agent).toContainText("no interior");
  const disclosure = agent.getByRole("button", { name: "Agent", exact: true });
  await expect(disclosure).toHaveCount(1);
  await disclosure.click();
  await expect(agent).toContainText("summarize");
});
