"use client";

/**
 * The message composer.
 *
 * One bordered surface holding everything a turn needs: attachments, the message,
 * and the runner configuration that turn will use.
 *
 * # Layout: the message owns its own row
 *
 * The textarea spans the full width and the controls sit on a row beneath it. That
 * is what makes the composer usable on a phone, and the previous shape is worth
 * recording because it looked reasonable and was not:
 *
 * The controls used to share one non-wrapping flex row with `ml-auto` on the right
 * group, and the agent, model, and effort controls were native `<select>`s. A
 * `<select>` is sized by its *longest option*, not by its current value, so a
 * runner declaring `claude-opus-5-20260501` made that control ~180px wide
 * regardless of what was selected. Three of them plus Send exceeded a 390px
 * viewport, and because the width came from content the user could not see, no
 * amount of `min-w-0` or `truncate` could recover it — the intrinsic minimum was
 * already too large. On a desktop the same effect merely squeezed the textarea.
 *
 * So the controls are buttons that render the *current* value and open a menu.
 * A button can truncate. Everything on the control row is `min-w-0` and the row
 * itself scrolls horizontally rather than growing, so no configuration can push
 * Send off screen.
 *
 * # What is on the row, and what is in the `+` menu
 *
 * Split by how often a turn changes it, following ChatGPT and Claude:
 *
 * - **Always visible**: the message, `+`, model, thinking, Send/Stop. Model and
 *   thinking are per-turn decisions that change what the message will *do*, so
 *   hiding them costs a click on the majority path and makes the current model
 *   invisible — which matters when a session is mid-conversation.
 * - **In the `+` menu**: attach a file, save attachments into the workspace, run a
 *   command, switch agent. Each is either occasional or session-scoped. The `+` is
 *   also the affordance a phone user already knows from every messaging app, which
 *   the paperclip it replaced is not: a paperclip claims the menu holds only
 *   attachments, so nothing else could ever be put there.
 *
 * # The controls are generated, never hardcoded
 *
 * From what the agent itself declares. Not defensiveness — the agents genuinely
 * disagree: `codex` declares no `model` at all, and each declares a different `effort`
 * enumeration (`claude` has `xhigh`/`max`, `codex` has `minimal`, `pi` has `off`). A
 * fixed pair of dropdowns would be wrong for `codex` on the first render.
 */

import dynamic from "next/dynamic";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  api,
  ApiError,
  isRetryable,
  sessionFlags,
  type Agent,
  type ConfigOption,
  type Facet,
  type Session,
  type SessionCommand,
} from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { useT, type Translate } from "@/lib/i18n";
import { SlashMenu, useSlashMatches } from "@/components/slash-menu";
import { commandRoute, slashStatus, type SlashStatus } from "@/lib/slash";
import {
  ConfigControl,
  configItems,
  configMenuItems,
} from "@/components/config-picker";
import { useIsMobile } from "@/lib/viewport";
import {
  MAX_ATTACHMENTS,
  MAX_TOTAL_BYTES,
  readAttachments,
  type StagedAttachment,
} from "@/components/attachment-picker";
import { Button, IconButton } from "@/components/ui/button";
import { MenuButton, type MenuItem } from "@/components/ui/menu";
import { Overlay } from "@/components/overlay";
import {
  SelectFilter,
  SelectRows,
  SelectScroll,
  useSelectList,
} from "@/components/ui/select-list";
import {
  ChevronDownIcon,
  ChipIcon,
  FileImageIcon,
  FileTextIcon,
  PaperclipIcon,
  PlusIcon,
  SendIcon,
  SparklesIcon,
  StopIcon,
  XIcon,
} from "@/components/icons";

const DirectoryPicker = dynamic(
  () => import("@/components/directory-picker").then((m) => m.DirectoryPicker),
  { ssr: false },
);

/**
 * Flags promoted to the control row, in order, with the icon each gets.
 *
 * Everything a runner declares beyond these still gets a control — it goes in the
 * `+` menu instead. That keeps the row a fixed shape no matter what a runner
 * declares, which is the property the old sorted-and-spread version lacked: a
 * runner adding a third enumerated flag silently made the row wider.
 */
const ROW_FLAGS = ["model", "effort"] as const;

/**
 * Config categories promoted to the control row, in order.
 *
 * Same rule as `ROW_FLAGS` and the same reason: a fixed-shape row. Everything else
 * an agent declares still gets a control, in the `+` menu, so an agent that reports
 * six knobs cannot widen the composer.
 */
const ROW_CATEGORIES = ["model", "effort"] as const;

/**
 * Sentinel for "the user asked for config but named no option".
 *
 * A distinct value rather than `undefined`, because "no dialog open" and "open the
 * whole config menu" are different states and conflating them makes the dialog
 * impossible to close.
 */
const ALL_CONFIG = "*";

/**
 * Human label for a flag key. `effort` is the reasoning budget.
 *
 * `t` is a parameter rather than a hook call: this is module-level, and the two
 * call sites are already inside components that hold one. An unrecognized flag
 * falls back to its protocol key, which is more honest than inventing a label —
 * a runner may declare any flag it likes.
 */
function flagLabel(key: string, t: Translate): string {
  if (key === "effort") return t("composer.thinking");
  if (key === "model") return t("composer.model");
  return key;
}

function flagIcon(key: string) {
  return key === "effort" ? <SparklesIcon size={14} /> : <ChipIcon size={14} />;
}

export function Composer({
  session,
  agents,
  /** Turns already in this session. Zero means the agent is still changeable. */
  turnCount,
  isLive,
  text,
  onTextChange,
  attachments,
  onAttachmentsChange,
  onSend,
  commands = [],
  config = [],
  agentConnecting = false,
  control,
  onReattach,
  onRunCommand,
  onSetConfig,
  onAgentChange,
  sending,
  sendError,
  configError,
  onStop,
  stopping,
  canInterrupt = false,
  onInterrupt,
  canSteer = false,
  canQueue = false,
  onSteer,
  onQueue,
  blockedCount = 0,
  onShowBlocked,
}: {
  session: Session;
  agents: Agent[];
  turnCount: number;
  isLive: boolean;
  text: string;
  onTextChange: (value: string) => void;
  attachments: StagedAttachment[];
  onAttachmentsChange: (next: StagedAttachment[]) => void;
  onSend: () => void;
  /**
   * Slash commands the session's agent reported.
   *
   * Empty for an agent that declares none, and for a detached session - which
   * is correct rather than a gap: the list is only knowable from a live agent,
   * so offering a remembered one would promise a command that may no longer
   * exist.
   */
  commands?: SessionCommand[];
  /**
   * Typed session config, from the live agent.
   *
   * This drives the model and effort controls, replacing the agent descriptor's
   * static `flags`: those are spawn-time choices, and `claude` declares an empty
   * `model.choices` there because its catalogue is only knowable from a live
   * session. Reading the descriptor left the model control permanently empty.
   */
  config?: ConfigOption[];
  /**
   * Whether a handshake is in flight.
   *
   * Passed so `/` can say "connecting" instead of appearing broken. Establishing
   * a link spawns an agent CLI and takes seconds, and that window is precisely
   * when a user opening a session types the first slash.
   */
  agentConnecting?: boolean;
  /** Why there is no live agent, when an attempt failed. */
  /**
   * Whether Sylph can send to this session, and why not.
   *
   * The facet rather than a boolean: `/` has to be able to say *which* of "still
   * connecting", "agent not installed", "that machine is asleep", and "running in
   * someone else's terminal" it is, and a bare false made all of them the dead
   * keystroke this prop exists to prevent.
   */
  control: Facet;
  /** Retry establishing the link. Absent when there is nothing to retry. */
  onReattach?: () => void;
  /** Invoke a prompt command instead of sending the text as a message. */
  onRunCommand?: (name: string, rest: string) => void;
  /** Set one config option. A control command resolves to this, never to a turn. */
  onSetConfig?: (configId: string, value: string | boolean) => void;
  /**
   * Replace an empty draft with another agent. The caller owns draft lifecycle and
   * navigation; the composer must not create a replacement behind its back because
   * it cannot know whether the current id is ephemeral or durable.
   */
  onAgentChange?: (agent: string) => void;
  sending: boolean;
  sendError: unknown;
  /**
   * Why the last config change failed, when one did.
   *
   * Rendered rather than dropped because a refused set otherwise looks exactly like
   * a broken control: the chip keeps its old value and nothing says why. An agent
   * genuinely can refuse — an option it no longer offers, or a vendor CLI that
   * rejects the value — and that is the user's cue to pick something else.
   */
  configError?: unknown;
  onStop: () => void;
  stopping: boolean;
  /** Whether the agent declared it can end a turn without ending the session. */
  canInterrupt?: boolean;
  onInterrupt?: () => void;
  /** Whether the agent declared it accepts text mid-turn. */
  canSteer?: boolean;
  /** Whether the agent accepts text for *after* the running turn. */
  canQueue?: boolean;
  onSteer?: (text: string) => void;
  onQueue?: (text: string) => void;
  /**
   * How many requests the agent is blocked on with their panel dismissed.
   *
   * Rendered because dismissing is not answering: the request stays open and the
   * agent stays stopped, and with the panel hidden nothing else on screen said so.
   * The dialog's own documentation claimed the composer reported this long before it
   * did — a blocked session that looks merely idle is the worst of the two failures,
   * because the user waits for an agent that is waiting for them.
   */
  blockedCount?: number;
  /** Bring a dismissed request's panel back. */
  onShowBlocked?: () => void;
}) {
  const queryClient = useQueryClient();
  const isMobile = useIsMobile();
  const t = useT();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [slashIndex, setSlashIndex] = useState(0);
  // The query the highlight belongs to, so a narrowing query can reset it during
  // render rather than in an effect.
  const [slashIndexFor, setSlashIndexFor] = useState<string | undefined>(undefined);
  // Which config option a control command opened, or `ALL_CONFIG` for all of them.
  const [openConfigId, setOpenConfigId] = useState<string | null>(null);

  // The menu is open when the text *is* a command being typed: a leading slash,
  // no whitespace yet. Derived rather than a state flag, so there is no way to
  // be in command mode while the text says otherwise - and deleting the slash
  // closes it without anything having to notice.
  const slashQuery = /^\/([^\s]*)$/.exec(text)?.[1];
  const slashTyping = slashQuery !== undefined;
  const slashMatches = useSlashMatches(commands, slashQuery ?? "");
  // `slashActive` gates the *listbox* keys and ARIA wiring, so it requires real
  // options to move through. Whether the surface appears at all is a separate
  // question, below: a status row is not a listbox, and claiming a combobox with
  // no options would point `aria-activedescendant` at an element that does not
  // exist.
  const slashActive = slashTyping && slashMatches.length > 0;
  // Why there is nothing to offer, when there is nothing to offer.
  //
  // Previously the menu required `commands.length > 0` to open at all, so typing
  // `/` against a detached or still-connecting agent did nothing whatsoever — no
  // menu, no message, no indication the key had been read. That is the same
  // observable behaviour as an unimplemented feature, and it is what a
  // misconfigured install looked like from the composer. A deliberate keystroke
  // now always produces an answer, even when the answer is "not yet".
  const status = slashStatus({
    typing: slashTyping,
    matchCount: slashMatches.length,
    commandCount: commands.length,
    connecting: agentConnecting,
    failed: !control.available,
  });

  // Reset the highlight when the candidate set changes, or a narrowing query
  // leaves the selection pointing past the end of a shorter list.
  //
  // Done during render, which is what React prescribes for state derived from a
  // changing input. In an effect it would paint one frame with the stale index -
  // briefly highlighting the wrong command, which is exactly the frame a fast
  // typist would press Enter on.
  if (slashIndexFor !== slashQuery) {
    setSlashIndexFor(slashQuery);
    setSlashIndex(0);
  }

  /**
   * Send, routing a leading slash by what kind of command it is.
   *
   * Both Enter and the Send button call this, so there is no way for one to treat
   * `/verify the auth change` as a command and the other as a message. The rule
   * itself lives in `lib/slash.ts`, where it is unit-tested without a DOM.
   */
  function submit() {
    // Mid-turn, text goes to whichever channel the agent actually has. Routing it as a
    // command would try to start a second turn, which the backend refuses one-per-session,
    // so the message would simply be lost with an error the user did not cause.
    //
    // **Steer is preferred where both exist, and the order is the whole point.** A steer
    // reaches the running turn and a queued follow-up waits for it, so sending a correction
    // to the queue delivers it after the thing it was meant to prevent — which is exactly
    // what happened while `claude` declared a steer it does not have. Where only queueing
    // exists the composer says so rather than silently doing the weaker thing.
    if (isLive && canSteer && onSteer) {
      onSteer(text);
      onTextChange("");
      return;
    }
    if (isLive && canQueue && onQueue) {
      onQueue(text);
      onTextChange("");
      return;
    }

    const route = commandRoute(commands, text);

    if (route.kind === "control") {
      // Nothing is sent. The text is cleared and the picker opened, so the composer
      // does not sit holding a command that has already done its job.
      onTextChange("");
      setOpenConfigId(route.configId ?? ALL_CONFIG);
      return;
    }

    if (route.kind === "prompt" && onRunCommand) {
      onRunCommand(route.name, route.rest);
      return;
    }
    onSend();
  }

  function pickCommand(command: SessionCommand) {
    if (command.kind === "control") {
      // Opened immediately rather than completed into the input: a control command
      // takes no argument, so there is nothing for the user to finish typing.
      onTextChange("");
      setOpenConfigId(command.configId ?? ALL_CONFIG);
      return;
    }
    // Completed into the input rather than sent, because most commands take an
    // argument and sending on selection would deny the user the chance to type
    // one. A trailing space is what makes the next keystroke an argument.
    onTextChange(`/${command.name} `);
    textareaRef.current?.focus();
  }
  const messageId = useId();
  const slashListboxId = useId();
  const problemId = useId();

  const [dragging, setDragging] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [encoding, setEncoding] = useState(false);
  const [uploadTarget, setUploadTarget] = useState<StagedAttachment[] | null>(null);

  const descriptor = agents.find((agent) => agent.name === session.agent);
  const flags = sessionFlags(session);
  // While a turn runs, text is steering rather than a new message — that is what the
  // agent can actually accept mid-turn, and it is why the textarea is no longer
  // disabled for an agent that supports it. Queuing a second turn instead would be
  // refused by the backend, which allows one turn per session.
  // Either channel keeps the textarea usable mid-turn; neither disables it. The two are
  // distinguished at submit and in the placeholder, not here — what this decides is only
  // whether the user may type at all.
  const steering = isLive && canSteer;
  const queueing = isLive && !canSteer && canQueue;
  const disabled = (isLive && !steering && !queueing) || sending;
  const canSend = text.trim().length > 0 && !disabled;

  // The agent is fixed once the conversation has content. Switching it would
  // hand an existing transcript to a different runner — and with it a different
  // identity and lifecycle policy — so it is offered only on an empty session.
  const agentLocked = turnCount > 0;

  // Auto-grow to the content, bounded. A fixed two-row box hides the start of a
  // long message while it is being written, and a freely growing one eats the
  // transcript. The cap is a share of the viewport rather than a fixed 240px: on a
  // phone in landscape, 240px is most of the screen.
  useEffect(() => {
    const node = textareaRef.current;
    if (!node) return;
    const cap = Math.max(120, Math.min(240, window.innerHeight * 0.35));
    node.style.height = "auto";
    node.style.height = `${Math.min(node.scrollHeight, cap)}px`;
  }, [text]);

  const totalBytes = attachments.reduce((sum, item) => sum + item.size, 0);

  async function accept(files: FileList | null) {
    if (!files || files.length === 0) return;
    setProblem(null);
    setEncoding(true);
    const { accepted, rejected } = await readAttachments(files, attachments);
    setEncoding(false);
    if (accepted.length > 0) onAttachmentsChange(accepted);
    setProblem(rejected.length > 0 ? rejected.join("; ") : null);
  }

  const uploadToWorkspace = useMutation({
    mutationFn: async ({ path, items }: { path: string; items: StagedAttachment[] }) => {
      // Sequential rather than concurrent: these are writes into the user's
      // project, and a partial failure is far easier to describe when the files
      // land in a known order.
      for (const item of items) {
        await api.files.upload(session.id, {
          path: relativeTo(path, session.workdir),
          name: item.name,
          data: item.data,
        });
      }
      return items.length;
    },
    onSuccess: () => {
      // The Artifacts pane lists this directory; without this the uploaded file
      // is missing until something else refetches.
      void queryClient.invalidateQueries({ queryKey: ["files", session.id] });
      setUploadTarget(null);
    },
  });

  /**
   * Changing the agent replaces the session: `agent` is part of a session's
   * identity, so it cannot be edited — it is one of the three fields the id is
   * made of. On an empty session there is nothing to preserve, so this creates a
   * fresh one in the same directory on the same machine. The route that owns an
   * ephemeral draft supplies the replacement callback; this component does not
   * guess whether its id is durable or disposable.
   */
  const agentChange = useMutation({
    mutationFn: async (agent: string) => {
      if (!onAgentChange) throw new Error("Agent switching is unavailable here.");
      await onAgentChange(agent);
    },
    /* The page that owns a draft performs replacement, cleanup, and navigation. */
    onSuccess: () => {},
  });

  /**
   * One mutation for every flag.
   *
   * Sends the whole flag set, because `PATCH` replaces rather than merges — a
   * partial send would clear the flag the user is not touching. Previously each
   * control owned its own mutation, which meant N pending states and N error
   * states for what is one endpoint.
   */
  const patchFlags = useMutation({
    mutationFn: ({ key, value }: { key: string; value: string }) => {
      const next = { ...flags };
      // Empty means "let the runner decide", expressed by omitting the flag rather
      // than sending a blank value the runner would pass through.
      if (value) next[key] = value;
      else delete next[key];
      return api.sessions.update(session.id, { flags: next });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["session", session.id] });
    },
  });

  // A free-form flag has nothing to enumerate, so there is no sensible control to
  // draw for it. Omitted rather than shown as an empty menu.
  //
  // `descriptor?.flags ?? {}` is inside the memo, not above it: a fresh `{}` every
  // render makes it a new dependency every render, which defeats the memo entirely.
  const enumerated = useMemo(
    () =>
      Object.entries(descriptor?.flags ?? {}).filter(
        ([, spec]) => (spec.choices ?? []).length > 0,
      ),
    [descriptor?.flags],
  );
  // Split the agent's live config into what sits on the row and what goes in the
  // `+` menu, by its own `category`. An option with no category falls to the menu:
  // the row is a fixed shape, and promoting an unrecognized knob would let a new
  // agent widen the composer.
  const rowConfig = useMemo(
    () =>
      config.filter((option) =>
        (ROW_CATEGORIES as readonly string[]).includes(option.category ?? ""),
      ),
    [config],
  );
  const menuConfig = useMemo(
    () =>
      config.filter(
        (option) => !(ROW_CATEGORIES as readonly string[]).includes(option.category ?? ""),
      ),
    [config],
  );

  // A descriptor flag the live config already covers is dropped rather than shown
  // twice. Two controls for one setting is worse than either alone: they disagree the
  // moment one is used, and nothing on screen says which won.
  const coveredByConfig = useMemo(
    () => new Set(config.flatMap((option) => [option.id, option.category ?? option.id])),
    [config],
  );
  const rowFlags = enumerated.filter(
    ([key]) =>
      (ROW_FLAGS as readonly string[]).includes(key) && !coveredByConfig.has(key),
  );
  const menuFlags = enumerated.filter(
    ([key]) =>
      !(ROW_FLAGS as readonly string[]).includes(key) && !coveredByConfig.has(key),
  );

  /** Choices for one flag, as menu items. */
  const flagItems = (key: string, choices: string[]): MenuItem[] => [
    {
      label: t("composer.runnerDefault"),
      checked: !flags[key],
      onSelect: () => patchFlags.mutate({ key, value: "" }),
    },
    ...choices.map((choice) => ({
      label: choice,
      checked: flags[key] === choice,
      onSelect: () => patchFlags.mutate({ key, value: choice }),
    })),
  ];

  /**
   * A disabled row used as a group heading.
   *
   * The menu has no nesting: a submenu on a touch screen is a reliable miss-tap
   * generator, because the parent row is still a target while the child list is
   * opening. So a group of choices is introduced by an inert labelled row instead,
   * which is also what makes the whole menu one flat arrow-key sequence.
   */
  const heading = (label: string): MenuItem => ({
    label,
    separated: true,
    disabled: true,
    onSelect: () => {},
  });

  const menuItems: MenuItem[] = [
    {
      label: t("composer.attachEllipsis"),
      icon: <PaperclipIcon size={14} />,
      hint: `${MAX_ATTACHMENTS} max`,
      disabled: disabled || encoding,
      onSelect: () => fileInputRef.current?.click(),
    },
    ...(attachments.length > 0
      ? [
          {
            label: t("composer.saveToWorkspace"),
            icon: <FileTextIcon size={14} />,
            disabled: disabled || uploadToWorkspace.isPending,
            onSelect: () => setUploadTarget(attachments),
          },
        ]
      : []),
    // Whatever the agent reports beyond the row's two categories. Present because
    // the agent declared it, so it is real configuration a user must be able to
    // reach — `claude` reports permissions and fast mode here.
    ...(menuConfig.length > 0
      ? configMenuItems(
          menuConfig,
          (configId, value) => onSetConfig?.(configId, value),
          t,
        )
      : []),

    // Agent, which is session-scoped rather than per-turn and so sits below a
    // separator. Two shapes: the choices when it is still switchable, and a single
    // inert row naming the current agent once it is not. Present-but-inert rather
    // than absent — a control that vanishes reads as a bug, where one that states
    // it is fixed explains the rule.
    ...(!onAgentChange || agentLocked || agents.length < 2
      ? [
          {
            label: `Agent: ${session.agent}`,
            icon: <ChipIcon size={14} />,
            hint: agentLocked ? "fixed" : undefined,
            separated: true,
            disabled: true,
            onSelect: () => {},
          } satisfies MenuItem,
        ]
      : [
          heading(t("composer.agent")),
          ...agents.map((agent) => ({
            label: agent.name,
            checked: agent.name === session.agent,
            disabled: disabled || agentChange.isPending,
            onSelect: () => {
              if (agent.name !== session.agent) agentChange.mutate(agent.name);
            },
          })),
        ]),

    // Anything the runner declares beyond model and effort. Rare, but it must have
    // a control: the runner declared it, so it is real configuration.
    ...menuFlags.flatMap(([key, spec]) => [
      heading(flagLabel(key, t)),
      ...flagItems(key, spec.choices ?? []),
    ]),
  ];

  return (
    <div className="shrink-0 border-t border-line bg-canvas px-2 pt-2 pb-safe md:px-4">
      {/* `relative` is load-bearing: the slash menu is positioned against this
          box. Without it the menu resolves against the document, which puts it
          above the viewport - in the DOM, styled correctly, and permanently
          offscreen. */}
      <div className="relative mx-auto max-w-3xl">
        {/* A dismissed request left the agent blocked with nothing on screen saying
            so, which reads as an idle session rather than a waiting one. `role=status`
            because it appears without the user acting. */}
        {blockedCount > 0 && (
          <p
            role="status"
            className="mb-2 flex flex-wrap items-center gap-2 text-2xs text-ink-muted"
          >
            <span
              aria-hidden
              className="size-1.5 shrink-0 rounded-full bg-warning-solid"
            />
            {blockedCount > 1
              ? t("approval.pendingCount", { count: blockedCount })
              : t("composer.blocked")}
            {onShowBlocked ? (
              <button
                type="button"
                onClick={onShowBlocked}
                className="focus-ring rounded underline underline-offset-2 hover:text-ink"
              >
                {t("composer.blockedShow")}
              </button>
            ) : null}
          </p>
        )}
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (canSend) submit();
          }}
          // Drop anywhere on the composer, not only on a dedicated strip: the
          // whole surface is the input, so the whole surface accepts files.
          onDragOver={(event) => {
            if (disabled) return;
            event.preventDefault();
            setDragging(true);
          }}
          onDragLeave={() => setDragging(false)}
          onDrop={(event) => {
            if (disabled) return;
            event.preventDefault();
            setDragging(false);
            void accept(event.dataTransfer.files);
          }}
          // Focus-within on the container, so the whole composer reads as one
          // control rather than the textarea lighting up inside a static box.
          //
          // No `opacity` while disabled. It used to carry `opacity-60`, which is a
          // blunt instrument: opacity composites the whole subtree against the
          // page, so every descendant's contrast drops at once. Measured by axe
          // during a live turn, the muted text fell to 2.87:1 and the Stop button
          // to 3.7:1 — both below the 4.5:1 that WCAG 1.4.3 requires, and Stop is
          // the one control a user needs *while* the turn runs. The muted
          // background says "not accepting input" without touching foreground
          // contrast, and the textarea dims its own text.
          className={`rounded-2xl border transition-colors focus-within:border-line-strong ${
            dragging
              ? "border-focus bg-sunken"
              : disabled
                ? "border-line bg-sunken"
                : "border-line bg-raised"
          }`}
        >
          {attachments.length > 0 && (
            <ul
              aria-label={t("composer.stagedAttachments")}
              className="flex flex-wrap gap-1.5 border-b border-line px-2 pt-2 pb-2"
            >
              {attachments.map((item) => (
                <li
                  key={item.id}
                  className="flex max-w-full items-center gap-1.5 rounded-lg border border-line bg-sunken py-1 pr-1 pl-1 text-2xs"
                >
                  {item.previewUrl ? (
                    // eslint-disable-next-line @next/next/no-img-element
                    <img
                      src={item.previewUrl}
                      alt=""
                      className="h-6 w-6 shrink-0 rounded object-cover"
                    />
                  ) : (
                    <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded bg-active text-ink-muted">
                      <FileImageIcon size={14} />
                    </span>
                  )}
                  <span className="min-w-0 flex-1 truncate font-medium" title={item.name}>
                    {item.name}
                  </span>
                  <span className="shrink-0 text-ink-muted">{formatBytes(item.size)}</span>
                  <IconButton
                    label={`Remove ${item.name}`}
                    disabled={disabled}
                    onClick={() =>
                      onAttachmentsChange(attachments.filter((a) => a.id !== item.id))
                    }
                  >
                    <XIcon size={14} />
                  </IconButton>
                </li>
              ))}
            </ul>
          )}

          {/* The textarea's accessible name, and therefore what a screen reader
              announces — so it is translated like any other user-facing string. The
              English value is unchanged, which is what keeps the E2E specs that
              query `getByLabel("Message")` valid. */}
          <label htmlFor={messageId} className="sr-only">
            {t("composer.message")}
          </label>
          {/* Exactly one of these can be showing: `slashActive` requires matches
              and `status` is non-null only when there are none. Written as two
              conditions rather than a ternary so neither has to be read as the
              other's fallback — the menu is a listbox the textarea drives, the
              status row is an announcement it does not. */}
          {slashActive && (
            <div className="absolute inset-x-0 bottom-full z-30 mb-2">
              <SlashMenu
                commands={commands}
                query={slashQuery ?? ""}
                activeIndex={slashIndex}
                onActiveIndexChange={setSlashIndex}
                onPick={pickCommand}
                listboxId={slashListboxId}
              />
            </div>
          )}
          {status && (
            <div className="absolute inset-x-0 bottom-full z-30 mb-2">
              <SlashStatus status={status} control={control} onReattach={onReattach} />
            </div>
          )}
          <textarea
            id={messageId}
            role={slashActive ? "combobox" : undefined}
            aria-expanded={slashActive ? true : undefined}
            aria-controls={slashActive ? slashListboxId : undefined}
            aria-activedescendant={
              slashActive ? `${slashListboxId}-${slashIndex}` : undefined
            }
            ref={textareaRef}
            value={text}
            rows={1}
            onChange={(event) => onTextChange(event.target.value)}
            onKeyDown={(event) => {
              // The command menu owns these keys while it is open, which is what
              // lets it be navigated without focus leaving the input.
              if (slashActive) {
                if (event.key === "ArrowDown") {
                  event.preventDefault();
                  setSlashIndex((index) => (index + 1) % slashMatches.length);
                  return;
                }
                if (event.key === "ArrowUp") {
                  event.preventDefault();
                  setSlashIndex(
                    (index) => (index - 1 + slashMatches.length) % slashMatches.length,
                  );
                  return;
                }
                if (event.key === "Tab") {
                  event.preventDefault();
                  pickCommand(slashMatches[slashIndex]);
                  return;
                }
                if (event.key === "Enter" && !event.shiftKey) {
                  // Enter completes rather than sends: the highlighted row is a
                  // suggestion the user has not committed to, and sending it
                  // unseen is how a wrong command gets run.
                  event.preventDefault();
                  pickCommand(slashMatches[slashIndex]);
                  return;
                }
                if (event.key === "Escape") {
                  // Leaves the text alone. Clearing it would discard what the user
                  // typed to dismiss a menu they never asked for.
                  event.preventDefault();
                  setSlashIndex(0);
                  textareaRef.current?.blur();
                  textareaRef.current?.focus();
                  return;
                }
              }
              if (event.key !== "Enter") return;
              // On a phone, Return inserts a newline and the Send button sends.
              // Enter-to-send on a soft keyboard means a message cannot contain a
              // line break at all — the Return key is the only way to type one, and
              // there is no Shift to hold. Every mobile messaging app behaves this
              // way for the same reason.
              if (isMobile) return;
              // On a desktop, Enter sends; Shift+Enter and Cmd/Ctrl+Enter insert a
              // newline.
              if (event.shiftKey || event.metaKey || event.ctrlKey) return;
              event.preventDefault();
              if (canSend) submit();
            }}
            onPaste={(event) => {
              // Pasted images are attachments. Without this a screenshot in the
              // clipboard pastes as nothing at all.
              const files = event.clipboardData.files;
              if (files.length > 0) {
                event.preventDefault();
                void accept(files);
              }
            }}
            placeholder={
              steering
                ? t("composer.placeholderSteer")
                : queueing
                  ? // Says the text waits, because it does. Promising a correction the
                    // vendor delivers after the turn is what made a user's "don't do that"
                    // arrive too late while everything reported success.
                    t("composer.placeholderQueue")
                  : isLive
                    ? t("composer.placeholderBusy")
                    : t("composer.placeholder")
            }
            disabled={disabled}
            aria-describedby={problem ? problemId : undefined}
            // `text-base` on mobile is not a style choice: mobile Safari zooms the
            // page when a focused field's text is under 16px and does not zoom back
            // out, which is the other half of the reported "the whole screen moves".
            //
            // `disabled:text-ink-muted` rather than inheriting a container opacity:
            // it dims the one thing that should read as inactive, and still clears
            // 4.5:1 on the disabled background.
            className="block w-full resize-none border-0 bg-transparent px-3 py-2.5 text-base placeholder:text-ink-muted focus:outline-none disabled:cursor-not-allowed disabled:text-ink-muted md:text-sm"
          />

          {/*
            The control row. `flex-nowrap` with a scrolling middle section rather
            than `flex-wrap`: wrapping makes the composer's height depend on how
            many flags a runner declares, so the transcript would reflow when a
            model name changed. Scrolling keeps the height fixed and Send pinned.
          */}
          <div className="flex items-center gap-1 px-2 pb-2">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              disabled={disabled || encoding}
              onChange={(event) => {
                void accept(event.target.files);
                // Reset so selecting the same file twice in a row still fires.
                event.target.value = "";
              }}
              className="sr-only"
              aria-hidden="true"
              tabIndex={-1}
            />

            {/* The trigger's name is "Add" while the panel's is the longer phrase:
                the phrase contains "Message", which is the textarea's own label, so
                reusing it here left two controls a screen reader could not tell
                apart. See the note in AGENTS.md — this cost 40 E2E specs once. */}
            <MenuButton
              label={t("action.add")}
              title={t("composer.addToMessage")}
              items={menuItems}
            >
              {(trigger) => (
                <IconButton
                  label={t("action.add")}
                  variant="ghost"
                  disabled={disabled}
                  size={isMobile ? 20 : 16}
                  {...trigger}
                >
                  <PlusIcon size={isMobile ? 20 : 16} />
                </IconButton>
              )}
            </MenuButton>

            {/*
              Scrolls rather than grows. The `[scrollbar-width:none]` pair hides the
              bar itself: a visible scrollbar under two chips reads as a rendering
              artefact, and the chips are self-evidently swipeable on a touch screen.
            */}
            <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto [-ms-overflow-style:none] [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
              {/*
                Live config first, and the descriptor's flags only for what it does
                not cover. The two answer different questions — what can be chosen
                before a session exists, against what this running session reports —
                and the live answer is the true one. Reading only the descriptor is
                why the model control was permanently empty: `claude` declares
                `model.choices: []` there, because its catalogue depends on the
                account and is not knowable until the CLI has loaded.
              */}
              {rowConfig.map((option) => (
                <ConfigControl
                  key={option.id}
                  option={option}
                  // Mid-turn the agent is already running with the old value, so a
                  // change now would silently apply to the *next* turn while
                  // appearing to affect this one.
                  disabled={isLive}
                  icon={flagIcon(option.category ?? option.id)}
                  onSet={(value) => onSetConfig?.(option.id, value)}
                />
              ))}
              {rowFlags.map(([key, spec]) => (
                <MenuButton
                  key={key}
                  label={flagLabel(key, t)}
                  items={flagItems(key, spec.choices ?? [])}
                >
                  {(trigger) => (
                    <Button
                      variant="chip"
                      disabled={isLive || patchFlags.isPending}
                      icon={flagIcon(key)}
                      className="max-w-40 shrink-0"
                      {...trigger}
                    >
                      <span className="min-w-0 truncate">
                        {flags[key] ?? flagLabel(key, t)}
                      </span>
                      <ChevronDownIcon size={12} />
                    </Button>
                  )}
                </MenuButton>
              ))}
            </div>

            {/*
              Interrupt sits beside Stop rather than replacing it, because they are
              different intentions: interrupt ends the turn and leaves the agent
              loaded, stop kills the process. Offering only one makes the other
              unreachable. Shown only when the agent declared it — an interrupt
              button on an agent that cannot interrupt is a lie.
            */}
            {isLive && canInterrupt && (
              <Button
                variant="secondary"
                onClick={onInterrupt}
                icon={<StopIcon size={14} />}
                className="shrink-0"
              >
                {t("action.interrupt")}
              </Button>
            )}

            {isLive ? (
              <Button
                variant="danger"
                onClick={onStop}
                disabled={stopping}
                icon={<StopIcon size={14} />}
                className="shrink-0"
              >
                {stopping ? t("action.stopping") : t("action.stop")}
              </Button>
            ) : (
              // Icon-only on a phone: the label costs the width the chips need, and
              // an upward arrow in a filled circle is the least ambiguous control in
              // any messaging app. `aria-label` carries the name either way.
              <Button
                type="submit"
                variant="primary"
                disabled={!canSend}
                aria-label={sending ? t("action.sending") : t("action.send")}
                className="shrink-0 max-md:h-9 max-md:w-9 max-md:rounded-full max-md:p-0"
              >
                <SendIcon size={16} />
                <span className="max-md:hidden">{sending ? t("action.sendingEllipsis") : t("action.send")}</span>
              </Button>
            )}
          </div>
        </form>

        {encoding && (
          <output aria-live="polite" className="mt-1 block text-2xs text-ink-muted">
            Encoding attachments...
          </output>
        )}

        {problem && (
          <p id={problemId} role="alert" className="mt-1 text-2xs text-danger">
            {problem}
          </p>
        )}

        {uploadToWorkspace.error != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {uploadToWorkspace.error instanceof Error
              ? uploadToWorkspace.error.message
              : t("composer.saveFailed")}
          </p>
        )}

        {patchFlags.error != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {patchFlags.error instanceof Error
              ? patchFlags.error.message
              : t("composer.configFailed")}
          </p>
        )}

        {agentChange.error != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {agentChange.error instanceof Error
              ? agentChange.error.message
              : "Could not switch agent."}
          </p>
        )}

        {sendError != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {describeSendError(sendError)}
          </p>
        )}

        {configError != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {configError instanceof ApiError || configError instanceof Error
              ? t("composer.setConfigFailed", { reason: configError.message })
              : t("composer.setConfigFailedGeneric")}
          </p>
        )}

        <div className="mt-1 flex items-center justify-between gap-3 px-1">
          {/* Hidden on a phone, where it would be false: Return inserts a newline
              there. A hint that describes the wrong platform is worse than none. */}
          <p className="hidden text-3xs text-ink-muted md:block">
            Enter to send · Shift+Enter for a new line
          </p>
          {attachments.length > 0 && (
            <p className="text-3xs text-ink-muted">
              {attachments.length} of {MAX_ATTACHMENTS} files · {formatBytes(totalBytes)} of{" "}
              {formatBytes(MAX_TOTAL_BYTES)}
            </p>
          )}
        </div>
      </div>

      {/*
        The picker a control command opens.
        `/model` in a terminal opens a list; through a headless protocol the same
        request as prompt text can only come back as prose, so the list lives here
        and is built from the agent's own typed options.
      */}
      {openConfigId && (
        <ConfigDialog
          config={openConfigId === ALL_CONFIG ? config : configFor(config, openConfigId)}
          onClose={() => setOpenConfigId(null)}
          onSet={(configId, value) => {
            onSetConfig?.(configId, value);
            setOpenConfigId(null);
          }}
        />
      )}

      {uploadTarget && (
        <DirectoryPicker
          title={t("composer.saveIntoWorkspace")}
          confirmLabel={t("composer.saveHere")}
          initialPath={session.workdir}
          onClose={() => setUploadTarget(null)}
          onChoose={(path) =>
            uploadToWorkspace.mutate({ path, items: uploadTarget })
          }
        />
      )}
    </div>
  );
}

/**
 * What `/` says when it has no commands to offer.
 *
 * The composer used to render nothing in this case, which is the one thing it
 * must not do: pressing `/` is a deliberate act, and a deliberate act that
 * produces no visible change is indistinguishable from a broken control. That is
 * how a backend with zero usable agents presented — as a composer ignoring
 * a keystroke, three layers from the actual cause.
 *
 * **An empty capability surface means "Sylph cannot reach it", never "there is nothing
 * there."** Those coincide for a session Sylph launched and are opposite for one
 * running in a terminal, where the agent has a full command list and simply will not
 * take instructions from a second party. So this reads the `control` facet's *reason*
 * and never infers anything from the emptiness of the list.
 *
 * Not a `listbox`. There is nothing selectable here, so announcing options a
 * screen reader cannot move through would be a lie, and `aria-activedescendant`
 * would point at an id that does not exist. It is a `status`, which is announced
 * on appearance without stealing focus from the textarea.
 */
function SlashStatus({
  status,
  control,
  onReattach,
}: {
  status: SlashStatus;
  control: Facet;
  onReattach?: () => void;
}) {
  const t = useT();

  const reason = control.available ? undefined : control.reason;

  // Whether a retry can change the answer is the protocol's rule, not this
  // component's: `isRetryable` mirrors `UnavailableReason::is_retryable`, so the button
  // cannot come to disagree with the cache lifetime in `use-agent-session.ts` that is
  // decided from the same rule. Offering a retry for `no_agent` would invite the user
  // to keep paying a process spawn for an outcome that cannot change.
  const retryable = status === "unavailable" && Boolean(reason && isRetryable(reason));

  const label =
    status === "connecting"
      ? t("composer.slash.connecting")
      : status === "unavailable"
        ? // Exhaustive over the closed union, so a reason added to the protocol fails
          // the type check here rather than falling through to a sentence that does
          // not describe it. Each of these sends the reader somewhere different: to a
          // machine to wake, to a terminal they are already in, to an install, or to
          // a retry.
          reason === "no_agent"
          ? t("composer.slash.noAgent")
          : reason === "environment_unreachable"
            ? t("composer.slash.environmentUnreachable")
            : reason === "not_ours_to_drive"
              ? t("composer.slash.notOursToDrive")
              : t("composer.slash.unavailable")
        : status === "none"
          ? t("composer.slash.none")
          : t("composer.slash.noMatch");

  return (
    <div
      role="status"
      className="rounded-lg border border-line bg-raised px-3 py-2 shadow-lg"
    >
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 flex-1 text-xs text-ink-muted">{label}</span>
        {retryable && onReattach && (
          <button
            type="button"
            onClick={onReattach}
            className="shrink-0 rounded border border-line-strong px-1.5 py-0.5 text-2xs font-medium text-ink hover:bg-sunken focus-ring"
          >
            {t("composer.slash.retry")}
          </button>
        )}
      </div>
    </div>
  );
}

/** The one option a control command named, or nothing when it names none. */
function configFor(config: ConfigOption[], configId: string): ConfigOption[] {
  const option = config.find((candidate) => candidate.id === configId);
  return option ? [option] : [];
}

/**
 * A control command's choices, as a modal.
 *
 * A dialog rather than the inline menu the row uses, because this is opened from the
 * *composer text* rather than from a button: there is no trigger element to anchor a
 * popover to, and anchoring it to the textarea would put it over what the user typed.
 *
 * An option that reports no choices still gets a row saying so. A command the agent
 * offered must not open an empty box — that reads as broken rather than as "this
 * agent has nothing to configure here".
 */
function ConfigDialog({
  config,
  onSet,
  onClose,
}: {
  config: ConfigOption[];
  onSet: (configId: string, value: string | boolean) => void;
  onClose: () => void;
}) {
  const t = useT();
  const items = useMemo(
    () =>
      config.length === 1
        ? configItems(config[0], (value) => onSet(config[0].id, value), t)
        : configMenuItems(config, onSet, t),
    [config, onSet, t],
  );
  const name =
    config.length === 1
      ? (config[0].name ?? config[0].id)
      : t("composer.configureEllipsis");

  // The same list the row's menu renders, so `/model` and the model chip cannot
  // disagree about filtering, ordering, or which row is current. `onClose` is not
  // passed: the caller closes this dialog from `onSet`, because setting a value is
  // a mutation whose response is what the surface behind re-renders from.
  const list = useSelectList(items);

  return (
    <Overlay
      title={name}
      onClose={onClose}
      // A filterable list focuses its own field; a short one has nothing to focus,
      // so the dialog itself must take focus or it cannot be answered or dismissed
      // by keyboard at all.
      autoFocus={!list.search}
    >
      {items.length === 0 ? (
        <p className="px-4 py-3 text-xs text-ink-muted">{t("composer.noConfigOptions")}</p>
      ) : (
        <>
          {/* Always ready: a dialog is visible from its first paint, unlike an
              anchored popover that has to be measured before it can take focus. */}
          {list.search && <SelectFilter list={list} name={name} />}
          {list.search ? (
            <SelectScroll list={list} label={name} fill>
              <SelectRows list={list} density="comfortable" />
            </SelectScroll>
          ) : (
            <div
              role="menu"
              aria-label={name}
              aria-activedescendant={list.activeDescendant}
              onKeyDown={list.onKeyDown}
              className="flex min-h-0 flex-col"
            >
              <SelectScroll list={list} label={name} fill>
                <SelectRows list={list} density="comfortable" />
              </SelectScroll>
            </div>
          )}
        </>
      )}
    </Overlay>
  );
}

/**
 * Express an absolute picked directory relative to the session's workdir.
 *
 * The upload endpoint takes a workdir-relative path, while the picker deals in
 * absolute ones. A directory outside the workdir yields `""` — the workdir root —
 * rather than an escaping path, because the backend would refuse it anyway and
 * the root is the harmless interpretation.
 */
export function relativeTo(absolute: string, workdir: string): string {
  const root = workdir.replace(/\/+$/, "");
  if (absolute === root) return "";
  if (absolute.startsWith(`${root}/`)) return absolute.slice(root.length + 1);
  return "";
}

/**
 * Turn an API failure into text that names the actual cause.
 *
 * The backend's stable error codes carry more than their prose does: a 409 on a
 * busy session and a rejected attachment need different next actions from the
 * user, and "Request failed" tells them neither.
 */
function describeSendError(error: unknown): string {
  if (error instanceof ApiError) {
    switch (error.code) {
      case "SESSION_ALREADY_RUNNING":
        return "This session already has a turn in flight. Wait for it to finish, or stop it first.";
      case "ATTACHMENT_INVALID":
        return `Attachment rejected: ${error.message}`;
      case "PAYLOAD_TOO_LARGE":
        return "The message plus attachments exceed the 10 MB per-turn limit. Remove a file and try again.";
      default:
        return error.message;
    }
  }
  return error instanceof Error ? error.message : "Could not send the message.";
}
