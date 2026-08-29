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
  "app.back": "Back",

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
  // A choice list past `SELECT_SEARCH_THRESHOLD` options gains a filter field.
  // `{name}` is what is being filtered, so the field's accessible name never
  // collides with the list's own — two controls a screen reader cannot tell apart
  // is a real defect, not test fragility.
  "select.filter.label": "Filter {name}",
  "select.filter.placeholder": "Type to filter",
  "select.filter.none": "No matches",
  // The keyboard contract, shown in a picker's footer. One string rather than one
  // per surface: every list here answers the same three keys, and three copies is
  // how two of them come to disagree.
  "select.keys": "↑↓ navigate · ↵ choose · Esc close",
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
  "shell.search": "Search sessions",
  "shell.createFailed": "Could not create the session.",
  "shell.newSession.title": "New session",
  "shell.newSession.confirm": "Start session here",

  // Finding a session and jumping to it (`Mod+K`). One store, so one kind of result:
  // this used to say "and the TraceDB archive", and the import badge and
  // archive-unavailable notice that went with it are gone too.
  //
  // `search.field` is deliberately *not* `shell.search`, which names the top-bar
  // button that opens this overlay. Giving the button and the field inside it one
  // accessible name leaves a screen reader two controls it cannot tell apart — the
  // collision that failed forty specs at once and is recorded under Pitfalls.
  "search.title": "Search sessions",
  "search.field": "Search query",
  "search.placeholder": "Search titles and transcripts...",
  "search.results": "Search results",
  "search.none": "No matching sessions.",
  "search.empty": "No sessions yet.",
  // "open" rather than `select.keys`' "choose": this list navigates to a session
  // rather than picking a value, and the footer is the only place that says so.
  "search.keys": "↑↓ navigate · ↵ open · Esc close",
  // A real failure now, not an optional dependency being absent: the index is part of
  // the server's own store, so this says so and leaves the cached rows usable.
  "search.indexFailed": "Could not search transcripts — showing cached sessions only",
  // Lowercase on purpose: this sits in a trailing slot beside the field, not in a
  // sentence.
  "search.searching": "searching transcripts...",

  // The command palette (`Mod+P`).
  //
  // Group names are keys rather than literals because they are *rendered* as
  // headings, not just used to sort — and the palette flattens the groups away as
  // soon as the user types, so an untranslated heading is visible in exactly the
  // state a new user opens first.
  "palette.title": "Command palette",
  "palette.field": "Command query",
  "palette.placeholder": "Run a command...",
  "palette.list": "Commands",
  "palette.none": "No matching command.",
  "palette.group.session": "Session",
  "palette.group.navigation": "Navigation",
  "palette.group.view": "View",
  "palette.group.settings": "Settings",
  // Not a group an author assigns: the palette promotes recently used commands
  // above the authored order, so this heading labels a run of rows that could have
  // come from any group.
  "palette.group.recent": "Recent",
  // "run" rather than "choose": this list executes rather than picking a value.
  "palette.keys": "↑↓ navigate · ↵ run · Esc close",

  // Command labels. Named after the command id so the two cannot drift apart.
  //
  // No directory in `quickStart`: it is configurable, so naming one would be wrong
  // for anyone who changed it, and Settings → Configuration reports what resolved.
  "palette.cmd.newSession": "New session",
  // Named for what it does rather than for the endpoint: "adopt" is the product word,
  // and `import` is a keyword so anyone looking for the old name still finds it.
  "palette.cmd.adoptSession": "Adopt an existing session",
  "palette.cmd.quickStart": "Quick Start session",
  "palette.cmd.stopTurn": "Stop the running turn",
  "palette.cmd.search": "Search sessions and archive",
  "palette.cmd.browseHistory": "Browse this workspace's history",
  "palette.cmd.startScreen": "Go to the start screen",
  // Each toggle reads as the action it performs, not the state it is in, so the
  // label answers "what happens if I press this" rather than "what is true now".
  "palette.cmd.hideLeft": "Hide workspaces sidebar",
  "palette.cmd.showLeft": "Show workspaces sidebar",
  "palette.cmd.hideRight": "Hide artifacts sidebar",
  "palette.cmd.showRight": "Show artifacts sidebar",
  "palette.cmd.expandAll": "Expand all thinking and tool blocks",
  "palette.cmd.collapseAll": "Collapse all thinking and tool blocks",
  "palette.cmd.collapseThinking": "Collapse thinking blocks by default",
  "palette.cmd.expandThinking": "Expand thinking blocks by default",
  "palette.cmd.collapseToolOutput": "Collapse tool output by default",
  "palette.cmd.expandToolOutput": "Expand tool output by default",
  // `{density}` is the *translated* value, read from `settings.density.*` rather
  // than interpolated raw — the same words the settings panel shows for it.
  "palette.cmd.density": "Cycle density (currently {density})",
  "palette.cmd.settings": "Open settings",
  "palette.cmd.refreshEnvironments": "Refresh machines and their agents",
  "palette.cmd.reloadData": "Reload sessions and workspaces",

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
  // **One entry, because there is one id.** This was two — "copy original session ID"
  // and "copy Web session ID" — because a session had a Sylph-minted UUID as well as
  // an agent-native one, and pasting the wrong one into the wrong tool found nothing
  // with no error. The id is the identity triple now, so there is one string to copy.
  "sidebar.copySessionId": "Copy session ID",
  "sidebar.refreshTranscript": "Refresh transcript",
  "sidebar.loadingHistory": "Loading history...",
  "sidebar.turnRunning": "A turn is running",

  // Choosing a workspace: the directories this machine has worked in, ranked by
  // recency. Reached from `Mod+N`, the sidebar, and the history browser, which is
  // why these are their own namespace rather than any one caller's.
  "workspacePicker.title": "Choose a workspace",
  "workspacePicker.filterName": "workspaces",
  // Names the *path* explicitly: the visible column is a leaf name, so nothing
  // else tells the user that a middle segment will match.
  "workspacePicker.filterPlaceholder": "Filter by any part of the path...",
  "workspacePicker.loading": "Loading workspaces...",
  "workspacePicker.loadFailed": "Could not load workspaces",
  "workspacePicker.empty": "No workspaces yet. Browse for a folder to start one.",
  "workspacePicker.browse": "Browse folders...",
  "workspacePicker.browseTitle": "Browse for a folder",
  "workspacePicker.sessions": "{count} sessions",

  // Adopting a session an agent already has, from that agent's own native store.
  // Its own namespace because it is a distinct act rather than a variant of creating
  // one: nothing is started, and the transcript already exists.
  "adopt.title": "Adopt an existing session",
  "adopt.filterName": "native sessions",
  // Names both columns the filter reaches, because neither is obvious from a row: the
  // visible label is a title the vendor recorded, and the second line is the path.
  "adopt.filterPlaceholder": "Filter by title or path...",
  "adopt.agentFilterLabel": "Filter by agent",
  "adopt.allAgents": "All agents",
  // Says seconds rather than showing a bare spinner. A cold listing genuinely takes
  // them — two of the three adapters read more than a header per file so they never
  // report an absent title or an absent end stamp — and a spinner that sits for four
  // seconds reads as a hang.
  "adopt.loading": "Reading what this machine holds...",
  "adopt.loadingMore": "Loading...",
  "adopt.loadMore": "Load more",
  "adopt.loadFailed": "Could not read this machine's sessions",
  // Distinct from "no match": an install with the CLIs present and no sessions yet has
  // nothing to adopt, and saying so is the difference between a dead end and an answer.
  "adopt.empty": "No sessions found on this machine.",
  "adopt.adopting": "Adopting...",
  "adopt.failed": "Could not adopt that session",
  // The honest label for a session no vendor titled. `codex` and `pi` record none at
  // all, and their transcripts carry no user text for a fallback to read, so a short id
  // is what is actually known — anything friendlier would be a guess shown as a fact.
  "adopt.untitled": "Untitled session {id}",
  "adopt.events": "{count} events",
  // Why a row cannot be adopted. Three reasons because three fixes: open it, widen
  // configuration, and nothing.
  "adopt.blocked.alreadyAdopted": "Already added — open it",
  "adopt.blocked.noWorkdir": "No recorded directory",
  "adopt.blocked.outsideRoots": "Outside permitted directories",

  // The history browser's own verb for the filesystem fallback, which is where a
  // chosen folder lands when it was reached by browsing rather than by picking.
  "history.showHistory": "Show its history",
  // Browsing one workspace's history.
  "history.title": "Workspace history",
  "history.field": "Filter this workspace's history",
  "history.placeholder": "Filter loaded sessions...",
  "history.list": "Sessions in this workspace",
  "history.chooseWorkspace": "Choose a workspace...",
  // The switcher's accessible name states the current directory *and* the verb,
  // because a button showing a truncated path is announced as that path alone.
  "history.workspaceButton": "Workspace: {path}. Change",
  "history.noWorkspace": "none chosen",
  // `+` marks that the listing hit its cap rather than being part of the number.
  "history.loaded": "{count} loaded",
  "history.loadedMore": "{count}+ loaded",
  "history.loadFailed": "Could not load history",
  "history.none": "No loaded session matches.",
  "history.empty": "No sessions here yet.",
  // Every row opens a continuable session now — there is no read-only archive view —
  // so this no longer promises a read-only one.
  "history.keys": "↑↓ navigate · ↵ open · Esc close",
  // Shown only when the cap actually bites. `GET /sessions` takes no cursor, so past
  // this there is nothing to page to and a truncated list would otherwise read as a
  // complete one. `Mod+K` reaches the rest by text.
  "history.capped":
    "showing the newest {count} — search to reach older ones",

  // The filesystem fallback: one directory level per request.
  "directoryPicker.title": "Choose a folder",
  "directoryPicker.confirm": "Use this folder",
  "directoryPicker.filterName": "folders",
  "directoryPicker.filterPlaceholder": "Filter folders, or type a path...",
  "directoryPicker.folders": "Folders",
  "directoryPicker.path": "Path",
  "directoryPicker.roots": "Starting points",
  "directoryPicker.parent": "Parent folder",
  "directoryPicker.loading": "Loading folders...",
  "directoryPicker.empty": "No subfolders here. Choose this folder, or go up.",
  "directoryPicker.noMatch": "No subfolder matches.",
  "directoryPicker.truncated": "Only the first folders are listed. Narrow down by opening one.",
  "directoryPicker.listFailed": "Could not list that folder.",
  "directoryPicker.goTo": "Go to {path}",
  // Enter opens a folder; the modifier chooses it outright. Stated in the footer
  // because a modifier no affordance mentions is a feature nobody finds.
  "directoryPicker.keys": "↑↓ navigate · ↵ open · ⌘↵ choose · Esc close",

  // Composer.
  "approval.fallbackTitle": "The agent needs permission",
  "approval.agentWants": "Waiting for your decision.",
  "approval.agentWantsTool": "Wants to use {tool}.",
  "approval.proposed": "Proposed",
  "approval.edited": "Edited - the agent will run this instead.",
  "approval.pendingCount": "{count} waiting",
  "approval.stale": "Someone already answered this.",
  // A question the agent asked, rather than permission it needs.
  "approval.question": "Waiting for your answer.",
  "approval.submit": "Send answer",
  "approval.incomplete": "Answer the required questions to send.",
  // An "Other" answer is the answer, not a note on one of the offered options, so
  // it is worded as a choice rather than as a comment field.
  "form.other": "Something else",
  "form.otherPlaceholder": "Type your answer",
  "form.notes": "Notes (optional)",
  // Shown in the composer while a request is dismissed but still open: the agent is
  // blocked and nothing else on screen would say so.
  "composer.blocked": "The agent is waiting on you.",
  "composer.blockedShow": "Show request",
  "composer.agent": "Agent",
  "composer.model": "Model",
  "composer.thinking": "Thinking",
  "composer.runnerDefault": "Runner default",
  // A control command opens a picker instead of sending a message. Short because it
  // is a badge in a dense list, not a sentence.
  "composer.setting": "setting",
  // What `/` answers when it has no commands to offer. Six distinct states, because
  // they need six different responses from the user: wait, install a CLI, wake a
  // machine, accept that this session cannot be driven at all, accept that this agent
  // has no commands, or retype.
  //
  // **None of these may be inferred from the empty list.** An empty capability surface
  // means "Sylph cannot reach it", never "there is nothing there" — and the two are
  // opposite for a session running in a terminal, which has a full command list that
  // is simply not ours to read. Each of these is the `control` facet's own reason.
  "composer.slash.connecting": "Connecting to the agent to load its commands...",
  "composer.slash.noAgent":
    "That agent's CLI is not installed on this session's machine, so it reports no commands.",
  "composer.slash.environmentUnreachable":
    "That machine is not connected, so its commands cannot be read and nothing can be sent.",
  "composer.slash.notOursToDrive":
    "This session is running in a terminal. Sylph can follow it, but this agent takes no commands from a second party.",
  "composer.slash.unavailable": "The agent could not be reached, so it reports no commands.",
  "composer.slash.none": "This agent reports no slash commands.",
  "composer.slash.noMatch": "No command matches.",
  // The menu's own region name, announced when a screen reader enters it.
  "composer.slash.regionLabel": "Agent commands",
  "composer.slash.retry": "Retry",
  "composer.agentDefault": "Agent default",
  "composer.configureEllipsis": "Configure...",
  // A refused config change. The agent's own words are carried through: it knows why
  // it declined and the user needs that, not a generic failure.
  "composer.setConfigFailed": "Could not apply that setting: {reason}",
  "composer.setConfigFailedGeneric": "Could not apply that setting.",
  "composer.noConfigOptions": "This agent reports nothing to configure here.",
  "composer.attachEllipsis": "Attach files...",
  "composer.saveToWorkspace": "Save attachments to workspace...",
  "composer.saveIntoWorkspace": "Save attachments into the workspace",
  "composer.saveHere": "Save here",
  "composer.stagedAttachments": "Staged attachments",
  "composer.placeholder": "Send a message...",
  "composer.placeholderBusy": "Waiting for the current turn to finish...",
  "composer.placeholderSteer": "Add to what the agent is doing...",
  "composer.placeholderQueue": "Queue this for after the current turn...",
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
  "settings.section.environments": "Machines & agents",
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
  "settings.section.config": "Configuration",
  "settings.config.failed": "Could not read the resolved configuration.",
  "settings.config.file.label": "Configuration file",
  "settings.config.file.hint":
    "Everything below is set here. Edit the file and restart; an environment variable overrides it.",
  "settings.config.file.absent":
    "No file yet — every value below is a default. It is created on first start.",
  "settings.config.resolved.label": "Resolved settings",
  "settings.config.resolved.hint":
    "What this server is actually using, and where each value came from.",
  "settings.config.column.setting": "Setting",
  "settings.config.column.value": "Value",
  "settings.config.column.source": "From",
  "settings.config.source.file": "config.toml",
  "settings.config.source.default": "default",
  "settings.config.missing": "missing",
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
    "An adopted session defaults to running without SOUL.md, because the transcript it already holds was produced without it — but that is settable per session, not fixed here.",

  // Settings: machines, and the agents each one can run.
  //
  // "Machine" in user-facing copy, `environment` on the wire. The wire name is right
  // for a thing that may be a laptop, an SSH host, or a sandbox; "machine" is what a
  // user calls the laptop on their desk, and this is the panel where they look for it.
  "settings.environments.label": "Machines",
  "settings.environments.hint":
    "Every paired machine, and the agent CLIs found on each. Which agents you can use is decided per machine, not globally.",
  "settings.environments.none":
    "No machines are paired, so there is nowhere to run a session. Pair one with `sylph-envagent pair`.",
  "settings.environments.connected": "connected",
  "settings.environments.offline": "offline",
  "settings.environments.noAgents": "no agent CLIs on its PATH",

  "settings.agents.default.label": "Default agent",
  "settings.agents.default.hint":
    "Used by Quick Start. Applies only where that agent is installed — a machine without it falls back to what it has.",
  "settings.agents.default.first": "First available agent",
  "settings.agents.models.label": "Default model per agent",
  "settings.agents.models.hint": "Applied as the `model` flag on new sessions.",
  "settings.agents.models.none":
    "No agents are available on any connected machine, so there is nothing to set a model for.",
  "settings.agents.models.noFlag": "declares no model flag",
  "settings.agents.models.runnerDefault": "Agent default",
  // Names one agent's model control. Distinct from the section label, which names
  // the whole list: the control and its container must not share a name.
  "settings.agents.models.forAgent": "{agent} model",

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
  "shortcut.adoptSession": "Adopt an existing session",
  "shortcut.quickStart": "Quick Start session",
  "shortcut.toggleLeftSidebar": "Toggle workspaces sidebar",
  "shortcut.toggleRightSidebar": "Toggle artifacts sidebar",
  "shortcut.openSettings": "Open settings",
  "shortcut.stopTurn": "Stop running turn",
  "shortcut.expandAll": "Expand all blocks",
  "shortcut.collapseAll": "Collapse all blocks",
  "shortcut.findInSession": "Find in session",

  // The delegation overview: one time axis across every delegation in a turn.
  //
  // `overview.row.*` are fragments of one sentence assembled by `describe()` and joined
  // with ", " — the row's accessible name. Fragments rather than one key because the
  // sentence's *parts* are conditional, and each is an independent clause in both
  // languages; `describe()`'s own comment fixes their order (identity, outcome, cost,
  // position) and that order holds in Chinese too.
  "overview.label": "Delegation overview",
  "overview.hide": "Hide {count} delegations",
  "overview.compare": "Compare {count} delegations",
  // "Compare" promises durations, so a record that measures none must not make it.
  // Opening is still worth it — the structure is there — so the verb changes.
  "overview.list": "List {count} delegations",
  "overview.unrecorded":
    "This session was read from the agent\u2019s own files, which record no per-event timing \u2014 so there is nothing here to time or compare. The structure below is complete.",
  "overview.orderLabel": "Order delegations by",
  "overview.order.structure": "structure",
  "overview.order.duration": "slowest first",
  // Two counted caveats, kept apart because "no figure" and "a weak figure" send a
  // reader to different places: the first looks like a bug, the second is read off the
  // ≥ sign.
  "overview.boundedAll":
    "Every row is a lower bound ({atLeast}) \u2014 a settled turn keeps each delegation\u2019s closing time, so its span is measured from the work it forwarded.",
  "overview.boundedSome":
    "{bounded} of {count} rows are a lower bound ({atLeast}) \u2014 a settled turn keeps each delegation\u2019s closing time, so its span is measured from the work it forwarded.",
  "overview.boundedOne":
    "{bounded} of {count} rows is a lower bound ({atLeast}) \u2014 a settled turn keeps each delegation\u2019s closing time, so its span is measured from the work it forwarded.",
  "overview.atLeast": "at least",
  "overview.untimed":
    "{untimed} of {count} cannot be timed at all: nothing was forwarded to measure from.",
  "overview.axisStart": "turn start",
  "overview.noAxis":
    "No delegation in this turn reports both a start and an end, so there is no time axis to compare against.",
  "overview.depth": "depth {depth}",
  "overview.slowest": "slowest",
  // The trailing figure on a row.
  "overview.timing.running": "running",
  "overview.timing.untimed": "untimed",

  // Status words. Not `turn.*`: these describe a trajectory, and "never finished" is
  // the honest reading of a delegation the server closed — nobody said it failed,
  // only that nobody said.
  "overview.status.running": "running",
  "overview.status.done": "finished",
  "overview.status.failed": "failed",
  "overview.status.abandoned": "never finished",

  // Accessible-name fragments, in the order `describe()` emits them.
  "overview.row.took": "took {elapsed}",
  "overview.row.ranAtLeastBeforeTurnEnd": "ran at least {elapsed} before its turn ended",
  "overview.row.ranAtLeastForwarded":
    "ran at least {elapsed}, measured from the work it forwarded",
  "overview.row.instant": "duration unknown, only its closing time was recorded",
  "overview.row.longest": "the longest here",
  "overview.row.startedIn": "started {offset} in",
  "overview.row.startedWithTurn": "started with the turn",
  "overview.row.nested": "nested {depth} deep",
  "overview.row.noInterior": "forwarded none of its own events",
  // "Yet" is load-bearing: a late outcome is written against the turn that owns the
  // trajectory, so this row can still acquire one. Only a restart makes it terminal.
  "overview.row.unreported": "outlived its turn, no outcome reported yet",
  "overview.row.activate": "activate to focus",
  // The separator between those fragments. In the catalogue because punctuation is a
  // locale property: Chinese uses a full-width comma, and a hardcoded ", " would put
  // an ASCII comma and a space into a Chinese sentence.
  "overview.row.separator": ", ",

  // What Sylph can do with this session, when it cannot drive it. Exhaustive over
  // `UnavailableReason`, so a reason added to the protocol is a compile error at the
  // component rather than a session rendering no explanation. Separate from
  // `composer.slash.*`, which answers the narrower "why is the slash menu empty" —
  // same causes, but that surface explains a missing list and this one explains a
  // session that cannot be sent to.
  "drive.label": "What Sylph can do with this session",
  "drive.environmentUnreachable.headline": "That machine is not connected.",
  "drive.environmentUnreachable.detail":
    "History reads from Sylph\u2019s own record, so the transcript is complete. Nothing can be sent until the machine is back.",
  "drive.notOursToDrive.headline": "Running in a terminal.",
  "drive.notOursToDrive.detail":
    "This agent offers no channel to a second party, so Sylph can follow the session but cannot send to it.",
  "drive.noAgent.headline": "That agent is not installed on this machine.",
  "drive.noAgent.detail":
    "History reads normally. Installing the agent\u2019s CLI there is what makes the session continuable.",
  "drive.handshakeFailed.headline": "The agent would not start.",
  "drive.handshakeFailed.detail": "History reads normally. Sending a message will try again.",

  // The in-session filter (`Cmd+F`). Filters turns rather than highlighting matches.
  "find.label": "Filter turns",
  // Region names a screen reader announces. Not decorative: these are how a
  // non-visual reader tells one part of a turn from another.
  "timeline.delegationPath": "Delegation path",
  "timeline.toolResult": "Tool result",
  "timeline.currentTurn": "Current turn",

  // Session header actions. The desktop pair and the phone menu name the same two
  // actions, so they share keys — a second spelling would let the two surfaces drift.
  "session.expandAll": "Expand all",
  "session.collapseAll": "Collapse all",
  "session.expandAllBlocks": "Expand all blocks",
  "session.collapseAllBlocks": "Collapse all blocks",
  "session.actions": "Session actions",
  "session.filterTurnsEllipsis": "Filter turns...",
  "session.copyWorkspacePath": "Copy workspace path",
  "session.refreshTranscript": "Refresh transcript",
  "sidebar.browseAllHistory": "Browse all history",
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
