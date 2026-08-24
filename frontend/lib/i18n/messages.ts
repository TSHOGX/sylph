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
  // The only key here with no rendering surface, deliberately: it mirrors the
  // `metadata.title` in `app/layout.tsx`, which Next resolves per *build* rather than
  // per user, so a browser tab cannot follow the locale. Kept so the app's own name
  // has one spelling, and so a future title that *can* be dynamic has somewhere to
  // read it from.
  "app.name": "Sylph Web",
  "app.skipToMain": "Skip to main content",
  "app.connecting": "Connecting to backend...",
  "app.backendUnreachableTitle": "Cannot reach the backend",
  "app.backendUnreachableBody": "Sylph Web needs the Rust backend running on",
  "app.retry": "Retry",

  "action.close": "Close",
  "action.cancel": "Cancel",
  "action.refresh": "Refresh",
  "action.copy": "Copy",
  "action.copied": "Copied",
  // Ends the turn but keeps the agent loaded. Distinct from Stop, which ends the
  // agent — two different intentions, so two different words.
  "action.interrupt": "Interrupt",
  "action.on": "On",
  "action.off": "Off",
  "action.loading": "Loading...",
  "action.expand": "Expand",
  "action.collapse": "Collapse",
  "action.send": "Send",
  "action.sending": "Sending",
  "action.sendingEllipsis": "Sending...",
  "action.stop": "Stop",
  "action.stopping": "Stopping...",
  "action.add": "Add",
  "action.create": "Create",
  "action.delete": "Delete",
  "action.replace": "Replace",
  "action.download": "Download",
  /** A dialog's confirm button while its mutation is in flight. */
  "action.working": "Working...",
  "action.viewSource": "View source",
  "action.viewRendered": "View rendered",

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
  "sidebar.listActions": "Workspace list actions",
  "sidebar.newWorkspaceEllipsis": "New Workspace...",
  "sidebar.newWorkspace": "New Workspace",
  "sidebar.bindWorkspace": "Bind workspace",
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
  "approval.fallbackTitle": "The agent needs permission",
  "approval.agentWants": "Waiting for your decision.",
  "approval.agentWantsTool": "Wants to use {tool}.",
  "approval.proposed": "Proposed",
  "approval.edited": "Edited - the agent will run this instead.",
  "approval.pendingCount": "{count} waiting",
  "approval.stale": "Someone already answered this.",
  "composer.agent": "Agent",
  "composer.model": "Model",
  "composer.thinking": "Thinking",
  "composer.runnerDefault": "Runner default",
  // A control command opens a picker instead of sending a message. Short because it
  // is a badge in a dense list, not a sentence.
  "composer.setting": "setting",
  "composer.agentDefault": "Agent default",
  "composer.configureEllipsis": "Configure...",
  "composer.noConfigOptions": "This agent reports nothing to configure here.",
  "composer.attachEllipsis": "Attach files...",
  "composer.saveToWorkspace": "Save attachments to workspace...",
  "composer.saveIntoWorkspace": "Save attachments into the workspace",
  "composer.saveHere": "Save here",
  "composer.stagedAttachments": "Staged attachments",
  "composer.placeholder": "Send a message...",
  "composer.placeholderBusy": "Waiting for the current turn to finish...",
  "composer.placeholderSteer": "Add to what the agent is doing...",
  "composer.message": "Message",
  "composer.addToMessage": "Add to this message",
  "composer.saveFailed": "Could not save those files into the workspace.",
  "composer.configFailed": "Could not update the runner configuration.",

  // Artifacts pane: the file tree and its actions.
  "files.heading": "Files",
  "files.treeLabel": "File tree",
  "files.noSession.title": "No session open",
  "files.noSession.hint": "Open a session to browse the files in its working directory.",
  /**
   * Deliberately short.
   *
   * "New folder in the working directory" collided with the directory picker's
   * "Working directory" field under `getByLabel`'s substring match, and a screen
   * reader user hears the same overlap. The root path renders beside the button,
   * so the destination needs no spelling out.
   */
  "files.newFolder": "New folder",
  "files.uploadFiles": "Upload files",
  "files.uploadHere": "Upload files here",
  "files.reloadTree": "Reload the file tree",
  "files.rootActions": "Working directory actions",
  "files.entryActions": "{name} actions",
  "files.empty": "Empty",
  "files.addFiles": "add files",
  "files.truncated": "Listing truncated — this directory holds more entries than are shown.",
  "files.symlink": "symbolic link",
  "files.symlinkTitle": "Symbolic link, resolving inside the workspace",
  "files.modified": "Modified {when}",
  "files.openPreview": "Open preview",
  "files.openFullScreen": "Open full screen",
  "files.openInNewTab": "Open in new tab",
  "files.copyRelativePath": "Copy relative path",
  "files.copyName": "Copy name",
  "files.openInExpanded": "This file is open in the expanded view.",
  "files.showHere": "Show it here instead",

  // Artifacts pane: a directory that could not be listed.
  /** The root's name in a listing error, where a path would otherwise go. */
  "files.error.workingDirectory": "the working directory",
  "files.error.gone": "{where} no longer exists on disk.",
  "files.error.sessionGone": "This session is no longer in the journal.",
  "files.error.outside": "{where} is outside this session's working directory.",
  "files.error.list": "Could not list {where}: {message}",
  "files.error.listPlain": "Could not list {where}.",

  // File operations: new folder, upload, delete.
  "files.mkdir.title": "New folder",
  /**
   * Split around `{name}` by `formatParts`, which renders the path in a mono
   * face. One key rather than a prefix and a suffix, because Chinese wraps the
   * value ("在 X 内") where English precedes it.
   */
  "files.mkdir.inside": "Inside {name}",
  "files.mkdir.inRoot": "In the session's working directory",
  "files.mkdir.label": "Folder name",
  "files.mkdir.placeholder": "name",
  "files.delete.fileTitle": "Delete this file?",
  "files.delete.folderTitle": "Delete this folder?",
  "files.delete.file": "{name} will be deleted from disk.",
  "files.delete.folder": "{name} and everything inside it will be deleted from disk.",
  /** Said plainly: there is no trash on this path, by design. */
  "files.delete.irreversible": "This cannot be undone.",
  "files.overwrite.title": "Replace the existing file?",
  "files.overwrite.body":
    "{name} already exists in this folder. Replacing it overwrites its contents.",
  "files.op.gone": "That path no longer exists on disk.",
  "files.op.outside": "That path is outside the session's working directory.",
  "files.op.failed": "Something went wrong.",
  "files.op.tooLarge": "{name} is {size}; the limit is {limit}.",
  "files.op.unreadable": "Could not read {name}. The file may be unreadable.",

  // File viewer: header, notices, and errors.
  "viewer.previewOf": "{kind} preview of {name}",
  "viewer.loading": "Loading {label}...",
  "viewer.back": "Back to the file tree",
  "viewer.reload": "Reload this file from disk",
  "viewer.openInNewTab": "Open in a new browser tab",
  "viewer.download": "Download this file",
  "viewer.expand": "Open full screen",
  "viewer.close": "Close preview",
  "viewer.truncated": "Showing the first {shown} of a {size} file.",
  "viewer.openWhole": "Open the whole file in a new tab",
  "viewer.errorTitle": "Could not read this file",
  "viewer.error.gone": "This file no longer exists on disk.",
  "viewer.error.outside": "This file is outside the session's working directory.",
  "viewer.error.unknown": "Unknown error.",
  "viewer.tooLargeImage": "This image is {size}, over the {limit} inline preview limit.",
  "viewer.binary": "This is a binary file ({size}), so there is nothing to show as text.",
  "viewer.openTab": "Open in a new tab",

  // Viewer kinds. Also the `{label}` a loading line names.
  "viewer.kind.grid": "Table",
  "viewer.kind.blocks": "Document",
  "viewer.kind.image": "Image",
  "viewer.kind.pdf": "PDF",
  "viewer.kind.html": "Web page",
  "viewer.kind.markdown": "Markdown",
  "viewer.kind.text": "Text",

  // Table viewer.
  "grid.worksheets": "Worksheets",
  "grid.sheetTitle": "{name} — {rows} rows, {cols} columns",
  "grid.row": "Row",
  "grid.chartsheet":
    "This sheet holds no cells — it is a chart or macro sheet rather than a worksheet.",
  "grid.noRows": "No rows in this part of the sheet.",
  "grid.previous": "Previous",
  "grid.next": "Next",
  "grid.loading": "loading...",
  /**
   * Two forms because English inflects the noun and the catalogue is where that
   * belongs. Chinese uses one string for both, which is correct rather than lazy:
   * the language marks no singular/plural distinction here.
   */
  "grid.rowCount": "{rows} rows",
  "grid.rowCountOne": "{rows} row",
  "grid.rowRange": "rows {from}–{to} of {total}",

  // Image and PDF viewer.
  "media.zoomOut": "Zoom out",
  "media.zoomIn": "Zoom in",
  "media.fit": "Fit",
  "media.previewTitle": "{name} preview",

  // HTML viewer.
  "html.sandboxed": "Rendered without scripts or network access",
  "html.source": "Source",
  "html.renderedPreview": "Rendered preview of {name}",

  "media.decodeFailed":
    "This image could not be decoded. It may be truncated or not the format its extension claims.",

  // What the HTML sandbox refused to load. Two forms per noun, for English.
  "html.blocked": "Not loaded in this preview: {parts}.",
  "html.blockedFiles": "{count} workspace files",
  "html.blockedFilesOne": "{count} workspace file",
  "html.blockedExternal": "{count} external resources",
  "html.blockedExternalOne": "{count} external resource",
  /** Joins the two clauses above. A word, so it is not assumed to be " and ". */
  "html.blockedJoin": " and ",

  // Document viewer.
  "document.slide": "Slide {number}",

  /** The full-screen viewer dialog's accessible name. */
  "viewer.overlayTitle": "Preview of {name}",

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
