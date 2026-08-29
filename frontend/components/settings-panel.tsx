"use client";

/**
 * `Cmd+,`: preferences.
 *
 * Sectioned rather than one long scroll, because the shortcut editor needs room
 * and burying it under appearance toggles makes it undiscoverable.
 *
 * Every string here goes through `t`. This panel is the densest prose in the app
 * and the first place a user changes language, so it is where an untranslated
 * string would be most visible.
 */

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, type Agent } from "@/lib/api";
import {
  DEFAULT_SHORTCUTS,
  useSettings,
  type Density,
  type ShortcutAction,
  type Theme,
} from "@/lib/settings";
import { comboFromEvent, formatCombo, reservedReason, useIsMac } from "@/lib/shortcuts";
import { LOCALE_NAMES, LOCALES, useT, type Locale, type MessageKey } from "@/lib/i18n";
import { useEnvironments } from "@/lib/environments";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { EnvironmentChip } from "@/components/badges";
import { Overlay } from "@/components/overlay";
import { IdentitySection } from "@/components/settings-identity";
import { ConfigSection } from "@/components/settings-config";
import { MenuButton, type MenuItem } from "@/components/ui/menu";
import { ChevronDownIcon } from "@/components/icons";

type Section = "general" | "identity" | "environments" | "workspaces" | "config" | "shortcuts";

const SECTIONS: Array<{ id: Section; label: MessageKey }> = [
  { id: "general", label: "settings.section.general" },
  // Second, right after appearance: it is the only setting here that changes what
  // an agent is told rather than how the app looks, so it should not sit below
  // three panels of display toggles.
  { id: "identity", label: "settings.section.identity" },
  // "Machines" rather than "Agents": which agents exist is decided per machine, so
  // this section is about the machines and their agents is what it lists.
  { id: "environments", label: "settings.section.environments" },
  { id: "workspaces", label: "settings.section.workspaces" },
  // Near the end, because it is the section a user opens when something is wrong
  // rather than when they want to change something — and it is the one that
  // answers "where does this install keep everything", which is the question the
  // sections above raise.
  { id: "config", label: "settings.section.config" },
  { id: "shortcuts", label: "settings.section.shortcuts" },
];

export function SettingsPanel() {
  const { closeOverlay } = useWorkspaceUI();
  const [section, setSection] = useState<Section>("general");
  const t = useT();

  return (
    <Overlay title={t("settings.title")} onClose={closeOverlay} size="panel">
      <div className="flex items-center justify-between border-b border-line px-3 py-2">
        <h3 className="text-sm font-semibold">{t("settings.title")}</h3>
        <button
          type="button"
          onClick={closeOverlay}
          className="rounded px-2 py-0.5 text-xs text-ink-muted hover:bg-hover hover:text-ink focus-ring"
        >
          {t("action.close")}
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        <nav aria-label={t("settings.sections")} className="w-40 shrink-0 border-r border-line p-2">
          <ul className="space-y-0.5">
            {SECTIONS.map((entry) => (
              <li key={entry.id}>
                <button
                  type="button"
                  onClick={() => setSection(entry.id)}
                  aria-current={section === entry.id ? "true" : undefined}
                  className={`w-full rounded px-2 py-1 text-left text-xs focus-ring ${
                    section === entry.id
                      ? "bg-accent text-accent-ink"
                      : "text-ink-soft hover:bg-hover"
                  }`}
                >
                  {t(entry.label)}
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <div className="min-h-0 flex-1 pane p-3">
          {section === "general" && <GeneralSection />}
          {section === "identity" && <IdentitySection />}
          {section === "environments" && <EnvironmentsSection />}
          {section === "workspaces" && <WorkspacesSection />}
          {section === "config" && <ConfigSection />}
          {section === "shortcuts" && <ShortcutsSection />}
        </div>
      </div>
    </Overlay>
  );
}

function GeneralSection() {
  const { settings, update, reset } = useSettings();
  const t = useT();

  return (
    <div className="space-y-4">
      {/*
        First, because it is the setting a user goes looking for. `System` follows
        `prefers-color-scheme` and keeps following it, so a machine on a
        light-at-day schedule changes with it; an explicit choice overrides that
        permanently.
      */}
      <Field label={t("settings.theme.label")} hint={t("settings.theme.hint")}>
        <Choices
          values={["light", "dark", "system"] as Theme[]}
          current={settings.theme}
          onPick={(value) => update({ theme: value })}
          label={(value) => t(`settings.theme.${value}` as MessageKey)}
        />
      </Field>

      {/*
        Labels are endonyms and deliberately not translated — see `LOCALE_NAMES`.
        A user who has switched to a language they cannot read has to be able to
        find their way back, which only works if each entry is written in the
        language it selects.
      */}
      <Field label={t("settings.language.label")} hint={t("settings.language.hint")}>
        <Choices
          values={[...LOCALES]}
          current={settings.locale}
          onPick={(value: Locale) => update({ locale: value })}
          label={(value) => LOCALE_NAMES[value]}
          capitalize={false}
        />
      </Field>

      <Field label={t("settings.density.label")} hint={t("settings.density.hint")}>
        <Choices
          values={["compact", "normal", "comfortable"] as Density[]}
          current={settings.density}
          onPick={(value) => update({ density: value })}
          label={(value) => t(`settings.density.${value}` as MessageKey)}
        />
      </Field>

      <Field label={t("settings.expansion.label")} hint={t("settings.expansion.hint")}>
        <Checkbox
          label={t("settings.expansion.thinking")}
          checked={settings.expandThinking}
          onChange={(checked) => update({ expandThinking: checked })}
        />
        <Checkbox
          label={t("settings.expansion.toolOutput")}
          checked={settings.expandToolOutput}
          onChange={(checked) => update({ expandToolOutput: checked })}
        />
      </Field>

      {/*
        The data directory used to be shown here as a lone read-only path. It has
        moved into the Configuration section, where it sits beside the source that
        supplied it and whether it exists — which is what a user reading a path
        actually needs. A second copy here would be the same value with less of
        the context that makes it useful.
      */}

      {/*
        Cache management is in the design sketch but not here: there is nothing to
        manage. TanStack holds the only client cache, and the palette's "Reload
        sessions and workspaces" already invalidates it.
      */}

      <Field label={t("settings.reset.label")} hint={t("settings.reset.hint")}>
        <button
          type="button"
          onClick={() => {
            if (window.confirm(t("settings.reset.confirm"))) reset();
          }}
          className="rounded border border-line-strong px-2 py-1 text-xs font-medium hover:bg-sunken focus-ring"
        >
          {t("settings.reset.button")}
        </button>
      </Field>
    </div>
  );
}

/**
 * Which machines exist, what each can run, and the model defaults that depend on it.
 *
 * # There is no "discovery" here any more, and that deleted a whole control
 *
 * This section used to hold a Refresh button that re-ran a `--describe` pass over
 * adapter directories, plus a fingerprint of what it found so the panel could
 * distinguish "re-read and something changed" from "re-read and nothing did" — because
 * a runner hardcoding its model list keeps reporting the same models until its script
 * is edited, which made "nothing changed" the common case and an honest button look
 * broken.
 *
 * All of it is gone with the discovery pass. An agent is available when its CLI is on
 * some machine's `PATH` and reports a version, answered by that machine's envagent
 * (`docs/api-design.md` § Agents and environments), so there is no pass on this side to
 * re-run and nothing for a fingerprint to compare. What replaces it is better: the list
 * is *per machine*, which is the question a user actually has — "why can I not use this
 * agent" now has one answer with one fix, and it names the machine.
 *
 * The `broken` map went the same way. A pass over directories could half-succeed and
 * needed to report a directory that failed to describe itself; `PATH` plus a version
 * cannot, so there is no third state between available and absent.
 */
function EnvironmentsSection() {
  const { settings, update } = useSettings();
  const t = useT();
  const environments = useEnvironments();

  /**
   * Agents to offer as a *preference*, across every reachable machine.
   *
   * Deliberately not "the agent list", which no longer exists: `defaultAgent` and
   * `defaultModels` are preferences expressed with no environment in view, so they have
   * to be pickable from a union — while `resolveSessionDefaults` validates the choice
   * against whichever machine a session actually lands on. That split is the honest
   * shape: the preference is global and the availability is not.
   */
  const offered = useMemo(() => {
    const byName = new Map<string, Agent>();
    for (const environment of environments.all) {
      if (!environment.connected) continue;
      for (const agent of environment.agents) {
        // First writer wins, which only decides whose `flags` populate the model
        // picker. Two machines with the same agent at different versions can declare
        // different model lists, and there is no correct merge — the *session's* own
        // live config is the authority on what it can be set to, and this is only
        // seeding a default.
        if (!byName.has(agent.name)) byName.set(agent.name, agent);
      }
    }
    return [...byName.values()].sort((a, b) => a.name.localeCompare(b.name));
  }, [environments.all]);

  return (
    <div className="space-y-4">
      {/* The machines themselves, first: everything below is scoped by them, and a
          user opening this panel because an agent is missing needs to see whether the
          machine it lives on is even connected. */}
      <Field
        label={t("settings.environments.label")}
        hint={t("settings.environments.hint")}
      >
        {environments.isPending ? (
          <p className="text-xs text-ink-muted">{t("action.loading")}</p>
        ) : environments.all.length === 0 ? (
          <p className="text-xs text-ink-muted">{t("settings.environments.none")}</p>
        ) : (
          <ul className="space-y-1.5">
            {environments.all.map((environment) => (
              <li key={environment.id} className="rounded border border-line px-2 py-1.5">
                <div className="flex items-baseline gap-2">
                  <span className="min-w-0 flex-1 truncate text-xs font-medium">
                    {environment.name}
                  </span>
                  {/* Connectedness carries a word, not only a hue (WCAG 1.4.1), and
                      the relay chip is drawn distinguishably because it changes what
                      AFK can promise — control on a relayed machine ends when its
                      relay leaves. */}
                  {environment.reachableVia && (
                    <EnvironmentChip
                      name={environment.name}
                      reachableVia={
                        environments.nameOf(environment.reachableVia) ?? environment.reachableVia
                      }
                    />
                  )}
                  <span
                    className={`shrink-0 text-2xs font-medium ${
                      environment.connected ? "text-success" : "text-ink-muted"
                    }`}
                  >
                    {environment.connected
                      ? t("settings.environments.connected")
                      : t("settings.environments.offline")}
                  </span>
                </div>
                <p className="mt-0.5 text-2xs text-ink-soft">
                  {environment.agents.length === 0
                    ? t("settings.environments.noAgents")
                    : environment.agents
                        .map((agent) => `${agent.name} ${agent.version}`)
                        .join(" · ")}
                </p>
              </li>
            ))}
          </ul>
        )}
      </Field>

      <Field label={t("settings.agents.default.label")} hint={t("settings.agents.default.hint")}>
        <select
          value={settings.defaultAgent}
          onChange={(event) => update({ defaultAgent: event.target.value })}
          aria-label={t("settings.agents.default.label")}
          className="w-full rounded border border-line px-2 py-1 text-xs focus-ring"
        >
          <option value="">{t("settings.agents.default.first")}</option>
          {offered.map((agent) => (
            <option key={agent.name} value={agent.name}>
              {agent.name}
            </option>
          ))}
        </select>
      </Field>

      <Field label={t("settings.agents.models.label")} hint={t("settings.agents.models.hint")}>
        {offered.length === 0 ? (
          <p className="text-xs text-ink-muted">{t("settings.agents.models.none")}</p>
        ) : (
          <ul className="space-y-1.5">
            {offered.map((agent) => {
              // Spawn-time choices, from what the agent declares about itself. An agent
              // that declares no `model` flag correctly offers none rather than a
              // guessed list — `codex` is that case, measured.
              const choices = agent.flags?.model?.choices ?? [];
              return (
                <li key={agent.name} className="flex items-center gap-2">
                  <span className="w-24 shrink-0 truncate text-xs font-medium">{agent.name}</span>
                  {choices.length === 0 ? (
                    <span className="text-2xs text-ink-muted">
                      {t("settings.agents.models.noFlag")}
                    </span>
                  ) : (
                    <ModelDefaultPicker
                      agent={agent.name}
                      choices={choices}
                      value={settings.defaultModels[agent.name] ?? ""}
                      onChange={(choice) =>
                        update({
                          defaultModels: {
                            ...settings.defaultModels,
                            [agent.name]: choice,
                          },
                        })
                      }
                    />
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </Field>
    </div>
  );
}
/**
 * A default-model choice, as a button that opens a filterable list.
 *
 * A button rather than a `<select>`, for the reason recorded in `ui/menu.tsx`: a
 * `<select>` is sized by its longest *option*, so one long model id widens the row
 * permanently and no `truncate` can recover it — the intrinsic minimum is already
 * too large. A runner declaring twenty models also gave a `<select>` no way to be
 * searched, which is the reported bug this shares its fix with.
 */
function ModelDefaultPicker({
  agent,
  choices,
  value,
  onChange,
}: {
  agent: string;
  choices: string[];
  value: string;
  onChange: (value: string) => void;
}) {
  const t = useT();
  const items = useMemo<MenuItem[]>(
    () => [
      {
        label: t("settings.agents.models.runnerDefault"),
        checked: !value,
        onSelect: () => onChange(""),
      },
      ...choices.map((choice) => ({
        label: choice,
        checked: value === choice,
        onSelect: () => onChange(choice),
      })),
    ],
    [choices, value, onChange, t],
  );

  const current = value || t("settings.agents.models.runnerDefault");
  // The menu is named for the agent; the trigger is named for the agent *and* its
  // current value. Deliberately different strings: a screen reader announces
  // "e2e model, Runner default" on the trigger rather than a bare value with no
  // idea what it configures, and two controls sharing one name is the collision
  // that once failed 40 specs at once.
  const name = t("settings.agents.models.forAgent", { agent });

  return (
    <MenuButton label={name} items={items}>
      {(trigger) => (
        <button
          {...trigger}
          type="button"
          aria-label={`${name}: ${current}`}
          className="flex min-w-0 flex-1 items-center gap-1 rounded border border-line px-2 py-1 text-xs hover:bg-sunken focus-ring"
        >
          <span className="min-w-0 flex-1 truncate">{current}</span>
          <ChevronDownIcon size={12} className="shrink-0 text-ink-muted" />
        </button>
      )}
    </MenuButton>
  );
}

function WorkspacesSection() {
  const { settings } = useSettings();
  const isMac = useIsMac();
  const t = useT();
  const { data } = useQuery({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: 30_000,
  });

  // Read from the live bindings, not written out: shortcuts are rebindable, and a
  // hardcoded combo in a hint becomes a lie the first time one is changed.
  const quickStartCombo = useMemo(() => {
    const combo = Object.entries(settings.shortcuts).find(
      ([, action]) => action === "quickStart",
    )?.[0];
    return combo ? formatCombo(combo, isMac) : t("shortcut.quickStart");
  }, [settings.shortcuts, isMac, t]);

  return (
    <div className="space-y-4">
      {/* Read-only, and the only thing this section holds.
          A "known workspaces" table lived here with pin and rename controls, and
          it was a second, worse copy of the sidebar: the sidebar lists the same
          directories with their sessions under them, and its context menu already
          pins, renames, and binds a new directory. */}
      <Field
        label={t("settings.workspaces.quickStart.label")}
        hint={t("settings.workspaces.quickStart.hint", { combo: quickStartCombo })}
      >
        <p className="rounded border border-line bg-sunken px-2 py-1 font-mono text-2xs text-ink-muted">
          {data?.quickStartDir ?? t("action.loading")}
        </p>
      </Field>

      <p className="text-xs text-ink-muted">{t("settings.workspaces.managed")}</p>
    </div>
  );
}

/** Action label keys, in the order the shortcuts list renders them. */
const ACTION_KEYS: Record<ShortcutAction, MessageKey> = {
  search: "shortcut.search",
  commandPalette: "shortcut.commandPalette",
  browseWorkspaceHistory: "shortcut.browseWorkspaceHistory",
  newSession: "shortcut.newSession",
  adoptSession: "shortcut.adoptSession",
  quickStart: "shortcut.quickStart",
  toggleLeftSidebar: "shortcut.toggleLeftSidebar",
  toggleRightSidebar: "shortcut.toggleRightSidebar",
  openSettings: "shortcut.openSettings",
  stopTurn: "shortcut.stopTurn",
  expandAll: "shortcut.expandAll",
  collapseAll: "shortcut.collapseAll",
  findInSession: "shortcut.findInSession",
};

function ShortcutsSection() {
  const { settings, update } = useSettings();
  const isMac = useIsMac();
  const t = useT();
  const [recording, setRecording] = useState<ShortcutAction | null>(null);
  const [rejected, setRejected] = useState<string | null>(null);

  const bindings = useMemo(() => {
    const byAction = new Map<ShortcutAction, string>();
    for (const [combo, action] of Object.entries(settings.shortcuts)) {
      if (!byAction.has(action)) byAction.set(action, combo);
    }
    return byAction;
  }, [settings.shortcuts]);

  function record(action: ShortcutAction, event: React.KeyboardEvent) {
    // Modifier-only presses are intermediate: waiting for the real key is what
    // lets a user hold Cmd+Shift and then choose a letter.
    if (["Meta", "Control", "Shift", "Alt"].includes(event.key)) return;
    event.preventDefault();

    if (event.key === "Escape") {
      setRecording(null);
      setRejected(null);
      return;
    }

    const combo = comboFromEvent(event.nativeEvent);

    // A combo the browser keeps for itself is refused rather than stored. It
    // would be accepted, look bound, and never fire — which is exactly the bug
    // the default `Mod+N` had: a new browser window opened while the app's own
    // dialog opened behind it. Note the keystroke may not even arrive here, in
    // which case the recorder simply stays open; the check covers the browsers
    // that do deliver it before acting anyway.
    const reserved = reservedReason(combo);
    if (reserved) {
      setRejected(t("settings.shortcuts.reserved", { combo: formatCombo(combo, isMac) }));
      return;
    }

    // Rebinding replaces the old combo for this action rather than adding a
    // second one, and steals the combo from whatever held it — a duplicate
    // binding would make behaviour depend on object key order.
    const next: Record<string, ShortcutAction> = {};
    for (const [existing, existingAction] of Object.entries(settings.shortcuts)) {
      if (existingAction === action || existing === combo) continue;
      next[existing] = existingAction;
    }
    next[combo] = action;
    update({ shortcuts: next });
    setRecording(null);
    setRejected(null);
  }

  return (
    <div className="space-y-3">
      <p className="text-2xs text-ink-muted">{t("settings.shortcuts.hint")}</p>

      {/* Assertive rather than polite: this interrupts a recording the user is in
          the middle of, and the reason has to reach them before they try the same
          combination again. */}
      {rejected && (
        <p
          role="alert"
          className="rounded border border-warn-line bg-warn-bg px-2 py-1 text-2xs text-ink-soft"
        >
          {rejected}
        </p>
      )}

      <ul className="divide-y divide-line rounded border border-line">
        {(Object.keys(ACTION_KEYS) as ShortcutAction[]).map((action) => {
          const combo = bindings.get(action);
          const isRecording = recording === action;
          const label = t(ACTION_KEYS[action]);
          return (
            <li key={action} className="flex items-center gap-2 px-2 py-1.5">
              <span className="min-w-0 flex-1 truncate text-xs">{label}</span>
              <button
                type="button"
                onClick={() => {
                  setRecording(isRecording ? null : action);
                  setRejected(null);
                }}
                onKeyDown={(event) => isRecording && record(action, event)}
                aria-label={
                  combo
                    ? t("settings.shortcuts.ariaBinding", {
                        action: label,
                        combo: formatCombo(combo, isMac),
                      })
                    : t("settings.shortcuts.ariaUnbound", { action: label })
                }
                className={`shrink-0 rounded border px-2 py-0.5 font-mono text-2xs focus-ring ${
                  isRecording
                    ? "border-info-line bg-info-bg text-info"
                    : "border-line-strong hover:bg-sunken"
                }`}
              >
                {isRecording
                  ? t("settings.shortcuts.record")
                  : combo
                    ? formatCombo(combo, isMac)
                    : t("settings.shortcuts.unbound")}
              </button>
            </li>
          );
        })}
      </ul>

      <p className="text-2xs text-ink-muted">{t("settings.shortcuts.reservedHint")}</p>

      <button
        type="button"
        onClick={() => update({ shortcuts: DEFAULT_SHORTCUTS })}
        className="rounded border border-line-strong px-2 py-1 text-xs font-medium hover:bg-sunken focus-ring"
      >
        {t("settings.shortcuts.resetDefaults")}
      </button>
    </div>
  );
}

/**
 * A row of mutually exclusive choices.
 *
 * Extracted because theme, language, and density were three copies of the same
 * fifteen lines, and adding language would have made four. `aria-pressed` on a
 * button group rather than a radio group: these apply immediately and have no
 * submit step, which is what a toggle button communicates and a radio does not.
 */
function Choices<T extends string>({
  values,
  current,
  onPick,
  label,
  capitalize = true,
}: {
  values: T[];
  current: T;
  onPick: (value: T) => void;
  label: (value: T) => string;
  capitalize?: boolean;
}) {
  return (
    <div className="flex flex-wrap gap-1.5">
      {values.map((value) => (
        <button
          key={value}
          type="button"
          onClick={() => onPick(value)}
          aria-pressed={current === value}
          className={`tap-target focus-ring rounded-md border px-2 py-1 text-xs ${
            capitalize ? "capitalize" : ""
          } ${
            current === value
              ? "border-accent bg-accent text-accent-ink"
              : "border-line hover:bg-hover"
          }`}
        >
          {label(value)}
        </button>
      ))}
    </div>
  );
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section>
      <h4 className="text-xs font-semibold text-ink">{label}</h4>
      {hint && <p className="mt-0.5 text-2xs leading-relaxed text-ink-muted">{hint}</p>}
      <div className="mt-1.5 space-y-1.5">{children}</div>
    </section>
  );
}

function Checkbox({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex items-center gap-2 text-xs">
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
        className="h-3.5 w-3.5 rounded border-line-strong focus-ring"
      />
      {label}
    </label>
  );
}
