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
 * From the runner's `--describe` output. Not defensiveness — the four installed
 * runners genuinely disagree: `codex` declares no `model` at all, and every runner
 * declares a different `effort` enumeration (`claude` has `xhigh`/`max`, `codex`
 * has `minimal`, `pi` has `off`). A fixed pair of dropdowns would be wrong for
 * `codex` on the first render.
 */

import dynamic from "next/dynamic";
import { useRouter } from "next/navigation";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  api,
  ApiError,
  sessionFlags,
  type Agent,
  type Session,
  type SessionCommand,
} from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { useT, type Translate } from "@/lib/i18n";
import { SlashMenu, useSlashMatches } from "@/components/slash-menu";
import { useIsMobile } from "@/lib/viewport";
import {
  MAX_ATTACHMENTS,
  MAX_TOTAL_BYTES,
  readAttachments,
  type StagedAttachment,
} from "@/components/attachment-picker";
import { Button, IconButton } from "@/components/ui/button";
import { MenuButton, type MenuItem } from "@/components/ui/menu";
import {
  ChevronDownIcon,
  ChipIcon,
  CommandIcon,
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
  onRunCommand,
  sending,
  sendError,
  onStop,
  stopping,
  hasCommands,
  onOpenCommands,
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
  /** Invoke a command instead of sending the text as a message. */
  onRunCommand?: (name: string, rest: string) => void;
  sending: boolean;
  sendError: unknown;
  onStop: () => void;
  stopping: boolean;
  hasCommands: boolean;
  onOpenCommands: () => void;
}) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const isMobile = useIsMobile();
  const t = useT();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const [slashIndex, setSlashIndex] = useState(0);
  // The query the highlight belongs to, so a narrowing query can reset it during
  // render rather than in an effect.
  const [slashIndexFor, setSlashIndexFor] = useState<string | undefined>(undefined);

  // The menu is open when the text *is* a command being typed: a leading slash,
  // no whitespace yet. Derived rather than a state flag, so there is no way to
  // be in command mode while the text says otherwise - and deleting the slash
  // closes it without anything having to notice.
  const slashQuery = /^\/([^\s]*)$/.exec(text)?.[1];
  const slashOpen = slashQuery !== undefined && commands.length > 0;
  const slashMatches = useSlashMatches(commands, slashQuery ?? "");
  const slashActive = slashOpen && slashMatches.length > 0;

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
   * Send, routing a leading slash to the command API.
   *
   * Both Enter and the Send button call this, so there is no way for one to
   * treat `/verify the auth change` as a command and the other as a message.
   * An unknown command is deliberately *not* intercepted: it goes through as
   * text, and the agent says it does not recognize it - which is what a
   * terminal does, and is more honest than this UI guessing.
   */
  function submit() {
    const match = /^\/([^\s]+)\s*([\s\S]*)$/.exec(text.trim());
    const known =
      match && commands.some((command) => command.name === match[1]);
    if (known && onRunCommand) {
      onRunCommand(match[1], match[2].trim());
      return;
    }
    onSend();
  }

  function pickCommand(command: SessionCommand) {
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
  const disabled = isLive || sending;
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
   * Changing the agent replaces the session: `agent` is fixed at creation
   * server-side, and identity policy hangs off it. On an empty session there is
   * nothing to preserve, so this creates a fresh one in the same directory and
   * navigates to it — which is why it is only offered at turn zero.
   */
  const recreate = useMutation({
    mutationFn: (agent: string) =>
      api.sessions.create({
        agent,
        workdir: session.workdir,
        // Carried from the session being replaced, not re-read from settings.
        // The user is changing one thing — the agent — and identity is fixed at
        // creation, so re-deriving it here would silently flip a session started
        // with the agent's own identity back to Sylph's the moment its model was
        // switched, with nothing on screen saying so.
        identityMode: session.identityMode === "preserve" ? "preserve" : "sylph",
      }),
    onSuccess: ({ session: created }) => {
      void queryClient.invalidateQueries({ queryKey: ["sessions"] });
      void queryClient.invalidateQueries({ queryKey: ["workspaces"] });
      router.push(`/sessions/${created.id}`);
    },
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
  const rowFlags = enumerated.filter(([key]) =>
    (ROW_FLAGS as readonly string[]).includes(key),
  );
  const menuFlags = enumerated.filter(
    ([key]) => !(ROW_FLAGS as readonly string[]).includes(key),
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
    ...(hasCommands
      ? [
          {
            label: t("composer.runCommand"),
            icon: <CommandIcon size={14} />,
            separated: true,
            disabled,
            onSelect: onOpenCommands,
          },
        ]
      : []),

    // Agent, which is session-scoped rather than per-turn and so sits below a
    // separator. Two shapes: the choices when it is still switchable, and a single
    // inert row naming the current agent once it is not. Present-but-inert rather
    // than absent — a control that vanishes reads as a bug, where one that states
    // it is fixed explains the rule.
    ...(agentLocked || agents.length < 2
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
            disabled: disabled || recreate.isPending,
            onSelect: () => {
              if (agent.name !== session.agent) recreate.mutate(agent.name);
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

          <label htmlFor={messageId} className="sr-only">
            Message
          </label>
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
              isLive ? t("composer.placeholderBusy") : t("composer.placeholder")
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
                  disabled={disabled && !hasCommands}
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
              {rowFlags.map(([key, spec]) => (
                <MenuButton
                  key={key}
                  label={flagLabel(key, t)}
                  items={flagItems(key, spec.choices ?? [])}
                >
                  {(trigger) => (
                    <Button
                      variant="chip"
                      // Mid-turn the runner is already running with the old value,
                      // so a change now would silently apply to the *next* turn
                      // while appearing to affect this one.
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

        {recreate.error != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {recreate.error instanceof Error
              ? recreate.error.message
              : "Could not switch agent."}
          </p>
        )}

        {sendError != null && (
          <p role="alert" className="mt-1 text-2xs text-danger">
            {describeSendError(sendError)}
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

      {uploadTarget && (
        <DirectoryPicker
          title="Save attachments into the workspace"
          confirmLabel="Save here"
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
