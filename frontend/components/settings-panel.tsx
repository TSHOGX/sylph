"use client";

/**
 * `Cmd+,`: preferences.
 *
 * Sectioned rather than one long scroll, because the shortcut editor needs room
 * and burying it under appearance toggles makes it undiscoverable.
 *
 * Only settings that exist are shown. The design sketch in
 * `docs/keyboard-shortcuts-default-sessions-settings.md` also lists theme
 * switching, a language picker, cache management, and a danger zone that deletes
 * the journal. Those are omitted deliberately: none has an implementation behind
 * it, and a toggle that silently does nothing is worse than an absent one. What
 * each would need is noted where it would go.
 */

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/api";
import {
  DEFAULT_SHORTCUTS,
  useSettings,
  type Density,
  type ShortcutAction,
} from "@/lib/settings";
import { comboFromEvent, formatCombo, useIsMac } from "@/lib/shortcuts";
import { useWorkspaceUI } from "@/lib/workspace-ui";
import { Overlay } from "@/components/overlay";

type Section = "general" | "agents" | "workspaces" | "shortcuts";

const SECTIONS: Array<{ id: Section; label: string }> = [
  { id: "general", label: "General" },
  { id: "agents", label: "Agents & models" },
  { id: "workspaces", label: "Workspaces" },
  { id: "shortcuts", label: "Shortcuts" },
];

export function SettingsPanel() {
  const { closeOverlay } = useWorkspaceUI();
  const [section, setSection] = useState<Section>("general");

  return (
    <Overlay title="Settings" onClose={closeOverlay} size="panel">
      <div className="flex items-center justify-between border-b border-gray-200 px-3 py-2">
        <h3 className="text-sm font-semibold">Settings</h3>
        <button
          type="button"
          onClick={closeOverlay}
          className="rounded px-2 py-0.5 text-xs text-gray-500 hover:bg-gray-100 hover:text-gray-900 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Close
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        <nav aria-label="Settings sections" className="w-40 shrink-0 border-r border-gray-200 p-2">
          <ul className="space-y-0.5">
            {SECTIONS.map((entry) => (
              <li key={entry.id}>
                <button
                  type="button"
                  onClick={() => setSection(entry.id)}
                  aria-current={section === entry.id ? "true" : undefined}
                  className={`w-full rounded px-2 py-1 text-left text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
                    section === entry.id
                      ? "bg-gray-900 text-white"
                      : "text-gray-700 hover:bg-gray-100"
                  }`}
                >
                  {entry.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <div className="min-h-0 flex-1 overflow-y-auto p-3">
          {section === "general" && <GeneralSection />}
          {section === "agents" && <AgentsSection />}
          {section === "workspaces" && <WorkspacesSection />}
          {section === "shortcuts" && <ShortcutsSection />}
        </div>
      </div>
    </Overlay>
  );
}

function GeneralSection() {
  const { settings, update, reset } = useSettings();
  const workspaces = useQuery({
    queryKey: ["workspaces"],
    queryFn: () => api.workspaces.list(),
    staleTime: 60_000,
  });

  return (
    <div className="space-y-4">
      <Field label="Timeline density" hint="Controls spacing and text size in the conversation.">
        <div className="flex gap-1.5">
          {(["compact", "normal", "comfortable"] as Density[]).map((value) => (
            <button
              key={value}
              type="button"
              onClick={() => update({ density: value })}
              aria-pressed={settings.density === value}
              className={`rounded border px-2 py-1 text-xs capitalize focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
                settings.density === value
                  ? "border-gray-900 bg-gray-900 text-white"
                  : "border-gray-200 hover:bg-gray-50"
              }`}
            >
              {value}
            </button>
          ))}
        </div>
      </Field>

      <Field
        label="Default expansion"
        hint="Errors always expand regardless of these, since a failure's output is the answer."
      >
        <Checkbox
          label="Expand thinking blocks"
          checked={settings.expandThinking}
          onChange={(checked) => update({ expandThinking: checked })}
        />
        <Checkbox
          label="Expand tool output on success"
          checked={settings.expandToolOutput}
          onChange={(checked) => update({ expandToolOutput: checked })}
        />
      </Field>

      <Field label="Data directory" hint="Set with SYLPH_WEB_DATA_DIR; read-only here.">
        <p className="rounded border border-gray-200 bg-gray-50 px-2 py-1 font-mono text-[11px] text-gray-600">
          {workspaces.data?.dataDir ?? "loading..."}
        </p>
      </Field>

      {/*
        Theme, UI language, and cache management are in the design sketch but not
        here. Theme needs a `dark:` pass over every component plus a resolved
        `prefers-color-scheme` fallback; language needs an i18n framework and
        extraction of every string. Both are real work, not a toggle, and a
        control that stores a preference nothing reads would misrepresent that.
      */}

      <Field label="Reset" hint="Clears stored preferences. Sessions and history are untouched.">
        <button
          type="button"
          onClick={() => {
            if (window.confirm("Reset all preferences to their defaults?")) reset();
          }}
          className="rounded border border-gray-300 px-2 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          Reset preferences
        </button>
      </Field>
    </div>
  );
}

function AgentsSection() {
  const { settings, update } = useSettings();
  const agentsQuery = useQuery({ queryKey: ["agents"], queryFn: () => api.agents.list() });
  const [refreshing, setRefreshing] = useState(false);

  const agents = agentsQuery.data?.agents ?? [];

  return (
    <div className="space-y-4">
      <Field
        label="Default agent"
        hint="Used by Quick Start and preselected in the create dialog."
      >
        <select
          value={settings.defaultAgent}
          onChange={(event) => update({ defaultAgent: event.target.value })}
          aria-label="Default agent"
          className="w-full rounded border border-gray-200 px-2 py-1 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          <option value="">First discovered runner</option>
          {agents.map((agent) => (
            <option key={agent.name} value={agent.name}>
              {agent.name}
            </option>
          ))}
        </select>
      </Field>

      <Field label="Default model per agent" hint="Applied as the `model` flag on new sessions.">
        {agents.length === 0 ? (
          <p className="text-xs text-gray-500">
            No runners discovered. Check SYLPH_RUNNERS_DIR, then refresh.
          </p>
        ) : (
          <ul className="space-y-1.5">
            {agents.map((agent) => {
              // Models come from the runner's own `--describe` output, so an
              // agent that declares no `model` flag correctly offers no choice
              // rather than a guessed list.
              const choices = agent.flags?.model?.choices ?? [];
              return (
                <li key={agent.name} className="flex items-center gap-2">
                  <span className="w-24 shrink-0 truncate text-xs font-medium">{agent.name}</span>
                  {choices.length === 0 ? (
                    <span className="text-[11px] text-gray-500">
                      declares no model flag
                    </span>
                  ) : (
                    <select
                      value={settings.defaultModels[agent.name] ?? ""}
                      onChange={(event) =>
                        update({
                          defaultModels: {
                            ...settings.defaultModels,
                            [agent.name]: event.target.value,
                          },
                        })
                      }
                      aria-label={`Default model for ${agent.name}`}
                      className="flex-1 rounded border border-gray-200 px-2 py-1 text-xs focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
                    >
                      <option value="">Runner default</option>
                      {choices.map((choice) => (
                        <option key={choice} value={choice}>
                          {choice}
                        </option>
                      ))}
                    </select>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </Field>

      <Field label="Discovery" hint="Re-runs `--describe` against every runner script.">
        <button
          type="button"
          disabled={refreshing}
          onClick={async () => {
            setRefreshing(true);
            try {
              await api.agents.refresh();
              await agentsQuery.refetch();
            } finally {
              setRefreshing(false);
            }
          }}
          className="rounded border border-gray-300 px-2 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 disabled:opacity-50"
        >
          {refreshing ? "Refreshing..." : "Refresh runners"}
        </button>
      </Field>
    </div>
  );
}

function WorkspacesSection() {
  const { settings } = useSettings();
  const isMac = useIsMac();
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
    return combo ? formatCombo(combo, isMac) : "Quick Start";
  }, [settings.shortcuts, isMac]);

  return (
    <div className="space-y-4">
      {/* Read-only, and the only thing this section holds.
          A "known workspaces" table lived here with pin and rename controls, and
          it was a second, worse copy of the sidebar: the sidebar lists the same
          directories with their sessions under them, and its context menu already
          pins, renames, and binds a new directory. Settings could only show the
          rows without the sessions that give them meaning, so the same action was
          two clicks further from the thing it acts on. Pointing at the sidebar
          also beats restating its menu here, which would drift the moment the menu
          changes. */}
      <Field
        label="Quick Start directory"
        hint={`Where ${quickStartCombo} starts a session. Resolved by the backend, which is the only side that can expand ~.`}
      >
        <p className="rounded border border-gray-200 bg-gray-50 px-2 py-1 font-mono text-[11px] text-gray-600">
          {data?.quickStartDir ?? "loading..."}
        </p>
      </Field>

      <p className="text-xs text-gray-500">
        Workspaces themselves are managed in the sidebar, where each one lists its
        sessions: right-click a workspace to pin, rename, or browse its history.
      </p>
    </div>
  );
}

const ACTION_LABELS: Record<ShortcutAction, string> = {
  search: "Global search",
  commandPalette: "Command palette",
  browseWorkspaceHistory: "Browse workspace history",
  newSession: "New session",
  quickStart: "Quick Start session",
  toggleLeftSidebar: "Toggle workspaces sidebar",
  toggleRightSidebar: "Toggle artifacts sidebar",
  openSettings: "Open settings",
  stopTurn: "Stop running turn",
  expandAll: "Expand all blocks",
  collapseAll: "Collapse all blocks",
  findInSession: "Find in session",
};

function ShortcutsSection() {
  const { settings, update } = useSettings();
  const isMac = useIsMac();
  const [recording, setRecording] = useState<ShortcutAction | null>(null);

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
      return;
    }

    const combo = comboFromEvent(event.nativeEvent);
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
  }

  return (
    <div className="space-y-3">
      <p className="text-[11px] text-gray-500">
        Click a binding, then press the keys. Escape cancels. A combo already in use
        is taken over, so two actions never share one shortcut.
      </p>

      <ul className="divide-y divide-gray-100 rounded border border-gray-200">
        {(Object.keys(ACTION_LABELS) as ShortcutAction[]).map((action) => {
          const combo = bindings.get(action);
          const isRecording = recording === action;
          return (
            <li key={action} className="flex items-center gap-2 px-2 py-1.5">
              <span className="min-w-0 flex-1 truncate text-xs">{ACTION_LABELS[action]}</span>
              <button
                type="button"
                onClick={() => setRecording(isRecording ? null : action)}
                onKeyDown={(event) => isRecording && record(action, event)}
                aria-label={`${ACTION_LABELS[action]} shortcut${combo ? `, currently ${formatCombo(combo, isMac)}` : ", unbound"}`}
                className={`shrink-0 rounded border px-2 py-0.5 font-mono text-[11px] focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900 ${
                  isRecording
                    ? "border-blue-500 bg-blue-50 text-blue-800"
                    : "border-gray-300 hover:bg-gray-50"
                }`}
              >
                {isRecording ? "press keys..." : combo ? formatCombo(combo, isMac) : "unbound"}
              </button>
            </li>
          );
        })}
      </ul>

      <button
        type="button"
        onClick={() => update({ shortcuts: DEFAULT_SHORTCUTS })}
        className="rounded border border-gray-300 px-2 py-1 text-xs font-medium hover:bg-gray-50 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      >
        Reset to defaults
      </button>
    </div>
  );
}

function Field({
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
      <h4 className="text-xs font-semibold text-gray-900">{label}</h4>
      {hint && <p className="mt-0.5 text-[11px] text-gray-500">{hint}</p>}
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
        className="h-3.5 w-3.5 rounded border-gray-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
      />
      {label}
    </label>
  );
}
