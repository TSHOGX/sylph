/**
 * The message catalogue.
 *
 * English is the source of truth, not merely the default: it is a plain object
 * literal, its keys are the type `Locale` dictionaries are checked against, and
 * a key that does not exist here cannot be translated. That direction matters —
 * with a union of both languages' keys, a typo in a translation would compile and
 * silently become a string nothing renders.
 *
 * # Why a hand-rolled catalogue rather than a framework
 *
 * `next-intl` and `next-i18next` are both built around locale-in-the-URL: a
 * `[locale]` route segment, middleware that negotiates and redirects, and a
 * per-request server-side catalogue load. None of that applies here. This app is
 * one user's loopback tool, its language is a client preference stored beside
 * theme and density, and every string lives in a client component. Adopting the
 * routing model would mean restructuring `app/` around a segment that exists only
 * to carry a value `localStorage` already holds, and it would put the locale in
 * shareable URLs of a single-user app.
 *
 * What a framework would genuinely buy — ICU MessageFormat, per-locale
 * code-splitting, plural and date rules — is either provided by the platform
 * (`Intl.PluralRules`, `Intl.NumberFormat`, both already used in `lib/format.ts`)
 * or not worth its weight at this size: both catalogues together are a few
 * kilobytes gzipped, well under the cost of the machinery to load one lazily.
 *
 * Revisit this if a third locale arrives with translators who need a standard
 * file format, or if the catalogue outgrows ~1000 keys.
 *
 * # Conventions
 *
 * Keys are `area.thing`, dot-namespaced, and read as a path through the UI.
 * Placeholders are `{name}` and substituted by `format` in `./index.ts`.
 *
 * Translations are localizations, not transliterations: `zh` reads as something
 * a Chinese developer would write, not as English word order in Chinese
 * characters. Product nouns that are proper names stay in English — Sylph,
 * TraceDB, an agent's name, `SOUL.md` — because they name things that are spelled
 * that way on disk and in the API.
 */

export const en = {
  // Shell chrome and navigation.
  "app.name": "Sylph Web",
  "app.skipToMain": "Skip to main content",
  "app.connecting": "Connecting to backend...",
  "app.backendUnreachableTitle": "Cannot reach the backend",
  "app.retry": "Retry",

  "action.close": "Close",
  "action.cancel": "Cancel",
  "action.save": "Save",
  "action.reset": "Reset",
  "action.refresh": "Refresh",
  "action.copy": "Copy",
  "action.copied": "Copied",
  "action.dismiss": "Dismiss",
  "action.loading": "Loading...",
  "action.expand": "Expand",
  "action.collapse": "Collapse",
  "action.send": "Send",
  "action.sending": "Sending",
  "action.sendingEllipsis": "Sending...",
  "action.stop": "Stop",
  "action.stopping": "Stopping...",
  "action.add": "Add",

  // Shell: panes, drawers, and the top bar.
  "shell.workspaces": "Workspaces",
  "shell.artifacts": "Artifacts",
  "shell.files": "Files",
  "shell.openWorkspaces": "Open workspaces",
  "shell.closeWorkspaces": "Close workspaces",
  "shell.showWorkspaces": "Show workspaces sidebar",
  "shell.hideWorkspaces": "Hide workspaces sidebar",
  "shell.openFiles": "Open files",
  "shell.closeFiles": "Close files",
  "shell.showArtifacts": "Show artifacts sidebar",
  "shell.hideArtifacts": "Hide artifacts sidebar",
  "shell.search": "Search sessions and archive",
  "shell.createFailed": "Could not create the session.",
  "shell.newSession.title": "New session",
  "shell.newSession.confirm": "Start session here",

  // Sidebar: workspaces and their sessions.
  "sidebar.title": "Sessions by workspace",
  "sidebar.quickSessions": "Quick Sessions",
  "sidebar.quickSessionsHint": "Quick Start puts scratch sessions here.",
  "sidebar.noSessions": "No sessions yet",
  "sidebar.workspaceActions": "Workspace actions",
  "sidebar.listActions": "Workspace list actions",
  "sidebar.newWorkspaceEllipsis": "New Workspace...",
  "sidebar.newWorkspace": "New Workspace",
  "sidebar.bindWorkspace": "Bind workspace",
  "sidebar.newSessionHereEllipsis": "New session here...",
  "sidebar.newSessionHere": "New session here",
  "sidebar.browseHistory": "Browse history...",
  "sidebar.pin": "Pin to sidebar",
  "sidebar.unpin": "Unpin from sidebar",
  "sidebar.pinned": "Pinned",
  "sidebar.renameEllipsis": "Rename workspace...",
  "sidebar.renameLabel": "Workspace display name",
  "sidebar.copyPath": "Copy path",
  "sidebar.copyWorkspacePath": "Copy workspace path",
  "sidebar.copyOriginalId": "Copy original session ID",
  "sidebar.copyWebId": "Copy Web session ID",
  "sidebar.refreshTranscript": "Refresh transcript",
  "sidebar.loadingHistory": "Loading history...",
  "sidebar.archiveUnavailable": "Archive unavailable — showing live sessions only",
  "sidebar.turnRunning": "A turn is running",

  // Composer.
  "composer.agent": "Agent",
  "composer.model": "Model",
  "composer.thinking": "Thinking",
  "composer.runnerDefault": "Runner default",
  "composer.attachEllipsis": "Attach files...",
  "composer.saveToWorkspace": "Save attachments to workspace...",
  "composer.runCommand": "Run a command...",
  "composer.stagedAttachments": "Staged attachments",
  "composer.placeholder": "Send a message...",
  "composer.placeholderBusy": "Waiting for the current turn to finish...",
  "composer.message": "Message",
  "composer.addToMessage": "Add to this message",
  "composer.saveFailed": "Could not save those files into the workspace.",
  "composer.configFailed": "Could not update the runner configuration.",

  // Settings: shell and sections.
  "settings.title": "Settings",
  "settings.sections": "Settings sections",
  "settings.section.general": "General",
  "settings.section.identity": "Identity",
  "settings.section.agents": "Agents & models",
  "settings.section.workspaces": "Workspaces",
  "settings.section.shortcuts": "Shortcuts",

  // Settings: general.
  "settings.theme.label": "Theme",
  "settings.theme.hint": "System follows your operating system's appearance setting.",
  "settings.theme.light": "Light",
  "settings.theme.dark": "Dark",
  "settings.theme.system": "System",
  "settings.language.label": "Language",
  "settings.language.hint":
    "Applies immediately. Agent output stays in whatever language the agent wrote it in.",
  "settings.density.label": "Timeline density",
  "settings.density.hint": "Controls spacing and text size in the conversation.",
  "settings.density.compact": "Compact",
  "settings.density.normal": "Normal",
  "settings.density.comfortable": "Comfortable",
  "settings.expansion.label": "Default expansion",
  "settings.expansion.hint":
    "Errors always expand regardless of these, since a failure's output is the answer.",
  "settings.expansion.thinking": "Expand thinking blocks",
  "settings.expansion.toolOutput": "Expand tool output on success",
  "settings.dataDir.label": "Data directory",
  "settings.dataDir.hint": "Set with SYLPH_WEB_DATA_DIR; read-only here.",
  "settings.reset.label": "Reset",
  "settings.reset.hint": "Clears stored preferences. Sessions and history are untouched.",
  "settings.reset.button": "Reset preferences",
  "settings.reset.confirm": "Reset all preferences to their defaults?",

  // Settings: identity / SOUL.md.
  "settings.identity.inject.label": "Sylph identity",
  "settings.identity.inject.hint":
    "Whether new sessions started here have SOUL.md appended to the agent's system prompt. Fixed when a session is created, so changing this affects the next session, not the ones already open.",
  "settings.identity.inject.on": "Append SOUL.md (Sylph)",
  "settings.identity.inject.off": "Use the agent as it comes",
  "settings.identity.unavailable":
    "No SOUL.md was found, so every session runs on the agent's own identity. Sylph looks for it beside the runners directory, or wherever SYLPH_SOUL points.",
  "settings.identity.preview.label": "SOUL.md",
  "settings.identity.preview.show": "Show",
  "settings.identity.preview.hide": "Hide",
  "settings.identity.preview.truncated":
    "Showing the first 128 KB. The file itself is larger.",
  "settings.identity.preview.readFailed": "This file could not be read.",
  "settings.identity.meta": "{bytes}, edited {when}",
  "settings.identity.appliesTo":
    "Native sessions imported from the archive never receive SOUL.md, whatever this is set to. That is a product invariant, not a preference.",

  // Settings: agents.
  "settings.agents.default.label": "Default agent",
  "settings.agents.default.hint": "Used by Quick Start and preselected in the create dialog.",
  "settings.agents.default.first": "First discovered runner",
  "settings.agents.models.label": "Default model per agent",
  "settings.agents.models.hint": "Applied as the `model` flag on new sessions.",
  "settings.agents.models.none": "No runners discovered. Check SYLPH_RUNNERS_DIR, then refresh.",
  "settings.agents.models.noFlag": "declares no model flag",
  "settings.agents.models.runnerDefault": "Runner default",
  "settings.agents.discovery.label": "Discovery",
  "settings.agents.discovery.hint": "Re-runs `--describe` against every runner script.",
  "settings.agents.discovery.button": "Refresh runners",
  "settings.agents.discovery.busy": "Refreshing...",
  "settings.agents.discovery.result": "{agents} runners, {models} models declared.",
  "settings.agents.discovery.unchanged":
    "Nothing changed. A runner that lists its models as a fixed set in its own `--describe` output will keep reporting the same ones until that script is edited — refreshing re-reads the script, it does not re-read the agent's configuration.",
  "settings.agents.discovery.changed": "Discovery updated: {summary}",
  "settings.agents.discovery.failed": "Refresh failed: {message}",
  "settings.agents.broken.label": "Runners that failed to describe themselves",
  "settings.agents.broken.hint":
    "These scripts exist but did not return a usable descriptor, so they offer no agent.",

  // Settings: workspaces.
  "settings.workspaces.quickStart.label": "Quick Start directory",
  "settings.workspaces.quickStart.hint":
    "Where {combo} starts a session. Resolved by the backend, which is the only side that can expand ~.",
  "settings.workspaces.managed":
    "Workspaces themselves are managed in the sidebar, where each one lists its sessions: right-click a workspace to pin, rename, or browse its history.",

  // Settings: shortcuts.
  "settings.shortcuts.hint":
    "Click a binding, then press the keys. Escape cancels. A combo already in use is taken over, so two actions never share one shortcut.",
  "settings.shortcuts.record": "press keys...",
  "settings.shortcuts.unbound": "unbound",
  "settings.shortcuts.resetDefaults": "Reset to defaults",
  "settings.shortcuts.reserved":
    "{combo} is reserved by the browser and cannot reach this page. Pick another combination.",
  "settings.shortcuts.reservedBinding": "reserved by the browser",
  "settings.shortcuts.reservedHint":
    "A browser keeps a few combinations for itself — new window, new tab, close tab — and a page cannot intercept them. Those are refused here rather than accepted and silently ignored.",
  "settings.shortcuts.ariaBinding": "{action} shortcut, currently {combo}",
  "settings.shortcuts.ariaUnbound": "{action} shortcut, unbound",

  // Shortcut action names.
  "shortcut.search": "Global search",
  "shortcut.commandPalette": "Command palette",
  "shortcut.browseWorkspaceHistory": "Browse workspace history",
  "shortcut.newSession": "New session",
  "shortcut.quickStart": "Quick Start session",
  "shortcut.toggleLeftSidebar": "Toggle workspaces sidebar",
  "shortcut.toggleRightSidebar": "Toggle artifacts sidebar",
  "shortcut.openSettings": "Open settings",
  "shortcut.stopTurn": "Stop running turn",
  "shortcut.expandAll": "Expand all blocks",
  "shortcut.collapseAll": "Collapse all blocks",
  "shortcut.findInSession": "Find in session",
} as const;

/** Every key in the catalogue. Derived from English, which is the source. */
export type MessageKey = keyof typeof en;

/**
 * A translation dictionary.
 *
 * `Record<MessageKey, string>` and not `Partial<...>`: a missing key falls back
 * to English at runtime, but the type requires the whole set so an untranslated
 * string is a build error rather than a surprise in the UI. The fallback exists
 * for a stale persisted catalogue, not as a licence to skip keys.
 */
export type Dictionary = Record<MessageKey, string>;
