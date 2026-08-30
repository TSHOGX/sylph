"use client";

/**
 * `/` — the start screen, shown when no session is open.
 *
 * This replaced an "All sessions" list with filters by agent, workdir, and date.
 * That list was a second, weaker copy of navigation the shell already owns: the
 * sidebar groups every session by the directory it runs in, `Cmd+K` searches all of
 * them, and `Cmd+Shift+K` pages one workspace's history. A user arriving at `/` has not asked to filter sessions by
 * date — they have opened the app and need to start or resume work. The filter
 * form also cost two `GET /api/v1/sessions` requests on first paint, one of them
 * only to populate an agent dropdown.
 *
 * So this is an IDE start screen, not a list: identity, the two ways to begin, the
 * few sessions worth resuming, and the keys that do everything else. The model is
 * VS Code's and JetBrains' welcome screen — but with the Notion-style property that
 * a first-run visit *teaches*, because "a session pairs an agent with a working
 * directory" is not guessable, and a wrong guess here means a session pointed at
 * the wrong tree.
 *
 * Two properties are load-bearing:
 *
 * **It paints before any request settles.** Identity, both actions, and the
 * shortcut list are static, so they are on screen at first paint. Only the middle
 * slot waits on data, and it reserves its height rather than collapsing — a start
 * screen that reflows as sessions arrive reads as broken on every visit.
 *
 * **It adds no request.** The sessions listing is the sidebar's, shared through
 * `liveSessionsQuery` and deduped by TanStack. Rendering this screen costs zero
 * additional round trips, which is what lets it be the default route.
 */

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { Kbd } from "@/components/ui/button";
import { EnvironmentChip, FidelityMarker } from "@/components/badges";
import { formatRelative, shortenHome } from "@/lib/format";
import { relayName, useEnvironments, type Environments } from "@/lib/environments";
import { liveSessionsQuery } from "@/lib/sessions-query";
import { workspacesQuery } from "@/lib/workspaces-query";
import { useShortcutHint } from "@/lib/shortcuts";
import { selectStartState, type RecentWorkspace } from "@/lib/welcome";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import type { ShortcutAction } from "@/lib/settings";

export default function StartPage() {
  const { runAction, browseHistory } = useWorkspaceUI();
  const hint = useShortcutHint();

  const sessions = useQuery(liveSessionsQuery());
  const state = selectStartState(sessions.data?.sessions, { pending: sessions.isPending });
  // Only to decide whether a row should name its machine at all, and to turn a
  // relaying id into a name. One machine means no chips — see `useEnvironments`.
  const environments = useEnvironments();

  // Only to resolve the home prefix for display. Already in cache — the shell
  // fetches it for the Quick Start directory — so this is a cache read, not a
  // request, and `undefined` simply means paths render unabbreviated.
  const workspaces = useQuery(workspacesQuery());

  return (
    <div className="min-h-0 flex-1 pane">
      {/* Anchored to a fixed offset from the top, not vertically centred. Centring
          made the wordmark's position depend on how tall the slot below it was, so
          the whole screen sat at a different height for a first-run user than for a
          returning one — and moved again on a short viewport. A fixed anchor is the
          same on every visit, which is what makes the two actions land in muscle
          memory. */}
      <div className="mx-auto max-w-2xl px-6 pb-16 pt-[12vh]">
        <h1 className="text-2xl font-semibold tracking-tight">Sylph</h1>
        {/* The subtitle states what a session *is*, because that is the one fact the
            rest of the UI assumes and never says. Without it, "New session" reads
            as "new chat" and the directory prompt that follows is a surprise. */}
        <p className="mt-1.5 text-sm text-ink-muted">
          A session pairs a coding agent with a working directory, on one of your
          machines. Work your agents did elsewhere can be brought in and read here too.
        </p>

        <div className="mt-7 flex flex-wrap gap-2">
          {/* Buttons, not links. Creating a session is a mutation once its
              directory is known, and these run the same `runAction` paths the
              shortcuts and the palette do — so pending and failure appear in the
              shell's one banner rather than in a state this page would own. */}
          <PrimaryAction
            label="New session"
            hint={hint("newSession")}
            description="Pick a directory to work in"
            onClick={() => runAction("newSession")}
          />
          <SecondaryAction
            label="Quick session"
            hint={hint("quickStart")}
            description="Scratch work, no project"
            onClick={() => runAction("quickStart")}
          />
        </div>

        <div className="mt-9">
          {state.kind === "pending" ? (
            /* Height held only while the listing is in flight, so the key list below
               does not start high and drop when rows arrive.

               Reserved on the *pending* branch rather than on this container, which
               is where it was first: a container tall enough for five rows leaves
               visible dead space above the keys for the many users who work in
               three directories, and that gap is on screen every visit — strictly
               worse than the one-time shift it was preventing. Three rows is the
               reservation, so a cold load settles by a small amount in whichever
               direction rather than always dropping.

               Not a spinner or skeleton rows: this is one small list on a screen
               that is already painted and fully usable, and a spinner would claim
               the page is loading when nothing here blocks. */
            <output aria-live="polite" className="block h-40">
              <span className="sr-only">Loading recent sessions</span>
            </output>
          ) : state.kind === "firstRun" ? (
            <FirstRun onBrowse={() => browseHistory()} />
          ) : (
            <Recents
                rows={state.workspaces}
                home={workspaces.data?.home}
                environments={environments}
              />
          )}
        </div>

        <ShortcutList hint={hint} />
      </div>
    </div>
  );
}

/**
 * First run: three steps, not a feature tour.
 *
 * Numbered because the ordering is real — a directory must be chosen before a turn
 * can run — and because a numbered list of three is read, where a paragraph of the
 * same content is skipped. This is the Notion-onboarding property worth borrowing:
 * it does not explain the product, it walks the first actual task.
 *
 * Only ever rendered when the app holds no session at all, so it disappears
 * permanently after the first create and costs a returning user nothing.
 */
function FirstRun({ onBrowse }: { onBrowse: () => void }) {
  return (
    <section aria-labelledby="first-run" className="rounded-xl border border-line p-5">
      <h2 id="first-run" className="text-sm font-semibold">
        Getting started
      </h2>
      <ol className="mt-3 space-y-3">
        <Step n={1} title="Choose a directory">
          A session is bound to one directory, and the agent can only read and write
          inside it. Pick the project you want to work on.
        </Step>
        <Step n={2} title="Say what you want done">
          Type into the composer. Model and thinking effort are per-turn controls
          there, and the agent is switchable until your first message.
        </Step>
        <Step n={3} title="Watch the work, not a chat log">
          Files read, commands run, and diffs applied appear as they happen. Files
          the agent touches open in the Artifacts pane on the right.
        </Step>
      </ol>
      <p className="mt-4 border-t border-line pt-3 text-sm text-ink-muted">
        Agents you have already used elsewhere have history here.{" "}
        <button
          type="button"
          onClick={onBrowse}
          className="font-medium text-ink underline decoration-line-strong underline-offset-2 hover:decoration-ink focus-ring"
        >
          Browse past work
        </button>{" "}
        to read it, without starting anything.
      </p>
    </section>
  );
}

function Step({ n, title, children }: { n: number; title: string; children: string }) {
  return (
    <li className="flex gap-3">
      <span
        aria-hidden="true"
        className="mt-px flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent text-2xs font-semibold text-accent-ink"
      >
        {n}
      </span>
      <div className="min-w-0">
        <p className="text-sm font-medium">{title}</p>
        <p className="mt-0.5 text-sm text-ink-muted">{children}</p>
      </div>
    </li>
  );
}

/**
 * Where to pick up: one row per directory, five directories.
 *
 * Not one row per session. A directory with seven sessions produced seven rows that
 * differed only by a timestamp — and since most sessions are untitled, they were
 * otherwise identical — which pushed every other place a user works off the list.
 * Resuming means returning to a place, so the place is the row, and the row opens
 * the newest session in it. Reaching an older one is the sidebar's job, and it
 * already lists them all under that directory.
 *
 * Five rows, not a paged list: the sidebar lists everything grouped this same way
 * and `Cmd+K` searches all of it. What this answers is narrower — "where was I
 * before I closed the tab" — and five rows answer it. No filters, for the same
 * reason: the old page's four filter controls existed to make a truncated 50-row
 * listing navigable, which is not a problem a five-row list has.
 */
function Recents({
  rows,
  home,
  environments,
}: {
  rows: RecentWorkspace[];
  home?: string;
  environments: Environments;
}) {
  return (
    <section aria-labelledby="recents">
      <h2 id="recents" className="text-xs font-semibold uppercase tracking-wider text-ink-muted">
        Pick up where you left off
      </h2>
      <ul className="-mx-2 mt-1">
        {rows.map(({ workdir, session, otherCount }) => (
          <li key={workdir}>
            <Link
              // Encoded, matching `workspace-sidebar.tsx`'s own link. A session id is the
              // identity triple, so it holds `:`, and a native id can hold `/`. The two
              // entry points into a session must agree, or one of them opens a route the
              // other does not.
              href={`/sessions/${encodeURIComponent(session.id)}`}
              className="flex items-baseline gap-3 rounded-lg px-2 py-2 hover:bg-sunken focus-ring"
            >
              <span className="min-w-0 flex-1">
                {/* The path is the row's identity, so it carries the stronger
                    weight — not the session label. `sessionLabel` falls back to
                    `agent · leaf` when a session is untitled, which reads as the
                    directory named twice once the path is beside it. */}
                <span className="block truncate text-sm font-medium">
                  {shortenHome(workdir, home)}
                </span>
                <span className="mt-0.5 block truncate text-xs text-ink-muted">
                  {session.title?.trim() ?? session.agent}
                  {otherCount > 0 && ` · ${otherCount + 1} sessions`}
                </span>
              </span>

              {/* Fidelity when it is not `stream`, and the machine when it is not
                  this one. Both are properties of the row a reader needs *before*
                  opening it: a coarse record cannot be timed, and a session on a
                  sleeping laptop cannot be continued. `stream` is unmarked for the
                  same reason `sylph` was — a marker on every row carries no
                  information. */}
              <FidelityMarker value={session.fidelity} />
              {environments.showEnvironment && (
                <EnvironmentChip
                  name={session.environmentName}
                  reachableVia={relayName(session.environment, environments)}
                />
              )}
              {session.status === "running" && (
                <span className="shrink-0 text-xs font-medium text-info">running</span>
              )}
              <span className="shrink-0 text-xs text-ink-muted">
                {formatRelative(session.updatedAtMs)}
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * The keys, taught rather than hidden in a settings panel.
 *
 * Rendered from the live bindings, so a rebound shortcut is shown rebound and a
 * Linux user sees `Ctrl`. An action with no binding renders no row rather than an
 * empty cap. This is the cheapest part of the screen to justify: every one of
 * these is otherwise discoverable only by opening `Cmd+,`, which itself is one of
 * them.
 */
function ShortcutList({ hint }: { hint: (action: ShortcutAction) => string }) {
  const rows: [ShortcutAction, string][] = [
    ["newSession", "New session"],
    ["search", "Search sessions"],
    ["browseWorkspaceHistory", "Browse this workspace's history"],
    ["commandPalette", "Command palette"],
    ["toggleLeftSidebar", "Toggle the workspaces sidebar"],
    ["openSettings", "Settings and key bindings"],
  ];

  return (
    <section aria-labelledby="shortcuts" className="mt-9 border-t border-line pt-5">
      <h2 id="shortcuts" className="text-xs font-semibold uppercase tracking-wider text-ink-muted">
        Keys
      </h2>
      <dl className="mt-2 grid gap-x-8 gap-y-1.5 sm:grid-cols-2">
        {rows.map(([action, label]) => {
          const combo = hint(action);
          if (!combo) return null;
          return (
            <div key={action} className="flex items-baseline justify-between gap-3">
              <dt className="truncate text-sm text-ink-muted">{label}</dt>
              <dd className="shrink-0">
                <Kbd>{combo}</Kbd>
              </dd>
            </div>
          );
        })}
      </dl>
    </section>
  );
}

function PrimaryAction({
  label,
  hint,
  description,
  onClick,
}: {
  label: string;
  hint: string;
  description: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      // `border-transparent`, not no border: the secondary action has a 1px border,
      // and without a matching one here the two buttons differ by 2px in both
      // dimensions and sit visibly misaligned in the same row.
      className="flex min-w-52 flex-col items-start rounded-lg border border-transparent bg-accent px-4 py-3 text-left hover:bg-accent-hover focus-ring"
    >
      <span className="flex w-full items-center justify-between gap-3 text-sm font-medium text-accent-ink">
        {label}
        {/* `text-accent-ink-muted` on `bg-accent`, not the `Kbd` component: that one is
            styled for light chrome and measures 3.66:1 here. */}
        {hint && <span className="font-mono text-xs text-accent-ink-muted">{hint}</span>}
      </span>
      <span className="mt-0.5 text-xs text-accent-ink-muted">{description}</span>
    </button>
  );
}

function SecondaryAction({
  label,
  hint,
  description,
  onClick,
}: {
  label: string;
  hint: string;
  description: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex min-w-52 flex-col items-start rounded-lg border border-line-strong px-4 py-3 text-left hover:bg-sunken focus-ring"
    >
      <span className="flex w-full items-center justify-between gap-3 text-sm font-medium">
        {label}
        {hint && <span className="font-mono text-xs text-ink-muted">{hint}</span>}
      </span>
      <span className="mt-0.5 text-xs text-ink-muted">{description}</span>
    </button>
  );
}
