/**
 * The icon set.
 *
 * # Why hand-authored SVG and not a package
 *
 * The app previously drew its entire interface with Unicode dingbats — `⚙ ⚡ ⌕ ▾
 * ⌗ ⑂ ⚠ ⛶ ✕ ↗ ↪ ✻ ❯ ✎ ◆ ▤ ▦ ▩ ▧ ◈ ▪ ▫ ▮ ⎘ 📎` and more. That was a deliberate
 * trade to protect the first-load budget, and the budget reasoning was right, but
 * the conclusion was not: those characters are font data, so what they look like
 * is decided by the platform, not by this project. `📎` and `⚡` render as
 * full-colour emoji on iOS and macOS, several of the geometric shapes are absent
 * from the default CJK fallback chain and come out as `.notdef` boxes, and the
 * ones that do render are semantically arbitrary — `▩` for an image and `▧` for a
 * PDF are shapes with no relationship to their meaning.
 *
 * The real choice was never "dingbats or a 40 KB icon package". Thirty-odd paths
 * cost about 2 KB and are tree-shaken per route, which is inside the noise of the
 * budget the package would have blown.
 *
 * # Conventions
 *
 * A 24-unit grid drawn at 16px, stroked in `currentColor` at 1.75 — heavier than
 * Lucide's 2 at 24px, because a stroke thinner than ~1.5 device pixels dithers on
 * a non-retina display. `round` caps and joins throughout: mitred joins at this
 * size produce visible spikes at acute angles.
 *
 * Every icon is `aria-hidden`. None of them is ever the only thing carrying a
 * meaning: the control around it has a label, or a text node sits beside it. That
 * is one rule rather than a per-icon decision, so an icon can never become the
 * sole accessible name by accident.
 */

import type { SVGProps } from "react";

export interface IconProps extends Omit<SVGProps<SVGSVGElement>, "children"> {
  /** Edge length in px. 16 suits the dense chrome; 20 the touch controls. */
  size?: number;
}

/**
 * Shared frame.
 *
 * `shrink-0` is here rather than at each call site because an icon inside a flex
 * row with a long truncating label gets squashed to an ellipse otherwise — which
 * happened to the origin dot in the sidebar.
 */
function Icon({ size = 16, children, ...rest }: IconProps & { children: React.ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      className="shrink-0"
      {...rest}
    >
      {children}
    </svg>
  );
}

/* ── Shell chrome ─────────────────────────────────────────────────────────── */

/** Left pane toggle. The filled bar shows which side the pane is on. */
export function PanelLeftIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="4" width="18" height="16" rx="2.5" />
      <path d="M9 4v16" />
    </Icon>
  );
}

export function PanelRightIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="4" width="18" height="16" rx="2.5" />
      <path d="M15 4v16" />
    </Icon>
  );
}

/** Drawer opener on a phone, where a pane toggle has no pane to point at. */
export function MenuIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 7h16M4 12h16M4 17h16" />
    </Icon>
  );
}

export function SearchIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="10.5" cy="10.5" r="6.5" />
      <path d="m15.5 15.5 4 4" />
    </Icon>
  );
}

/**
 * Settings.
 *
 * Sliders rather than a gear: a gear needs eight teeth to read as one, and at
 * 16px those teeth merge into a blurred ring. Three sliders stay legible at any
 * size, and are what Linear and Raycast use for the same control.
 */
export function SettingsIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 7h10M18 7h2M4 17h2M10 17h10" />
      <circle cx="16" cy="7" r="2.25" />
      <circle cx="8" cy="17" r="2.25" />
    </Icon>
  );
}

export function PlusIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 5v14M5 12h14" />
    </Icon>
  );
}

export function XIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 6l12 12M18 6L6 18" />
    </Icon>
  );
}

export function CheckIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m5 13 4.5 4.5L19 7" />
    </Icon>
  );
}

/* ── Direction ────────────────────────────────────────────────────────────── */

export function ChevronRightIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m9 5 7 7-7 7" />
    </Icon>
  );
}

export function ChevronLeftIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m15 5-7 7 7 7" />
    </Icon>
  );
}

export function ChevronDownIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m5 9 7 7 7-7" />
    </Icon>
  );
}

export function ChevronUpIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="m5 15 7-7 7 7" />
    </Icon>
  );
}

export function ArrowUpIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 19V5M6 11l6-6 6 6" />
    </Icon>
  );
}

export function ArrowLeftIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M19 12H5M11 6l-6 6 6 6" />
    </Icon>
  );
}

/** Symlink, and any link that leaves the app. */
export function ExternalLinkIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M13 5h6v6M19 5l-8 8" />
      <path d="M18 14v4a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 18V7.5A1.5 1.5 0 0 1 6 6h4" />
    </Icon>
  );
}

/* ── Files and folders ────────────────────────────────────────────────────── */

/**
 * A closed folder.
 *
 * Folder and file are the distinction the tree exists to make, so they are
 * different *silhouettes*, not the same box with different infill: the tab along
 * the folder's top edge is visible at a glance, where an interior glyph is not.
 * Filled at low opacity so a directory reads as heavier than the files in it.
 */
export function FolderIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path
        d="M3.5 7.5A1.5 1.5 0 0 1 5 6h3.6a1.5 1.5 0 0 1 1.06.44l1 1A1.5 1.5 0 0 0 11.72 8H19a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5Z"
        fill="currentColor"
        fillOpacity="0.14"
      />
    </Icon>
  );
}

/**
 * An open folder: the front face swings out and to the right.
 *
 * The skew is exaggerated past realism, deliberately. This icon's whole job is to be
 * distinguishable from `FolderIcon` at 15px in a tree row — it *is* the
 * expanded/collapsed signal. The first version tilted the front face a few degrees,
 * which is what a file manager does at 32px and is invisible at 15px: side by side,
 * the two folders were indistinguishable.
 */
export function FolderOpenIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3.5 17.8V7.5A1.5 1.5 0 0 1 5 6h3.6a1.5 1.5 0 0 1 1.06.44l1 1A1.5 1.5 0 0 0 11.72 8h4.78A1.5 1.5 0 0 1 18 9.5v1.6" />
      <path
        d="M6.35 12.5A1.5 1.5 0 0 1 7.78 11.4h12.5a1.2 1.2 0 0 1 1.14 1.58l-1.7 5.1A1.5 1.5 0 0 1 18.29 19.2H4.7a1.2 1.2 0 0 1-1.14-1.58Z"
        fill="currentColor"
        fillOpacity="0.14"
      />
    </Icon>
  );
}

/** The base sheet every file icon is built from: corner fold, no infill. */
function FileFrame({ children }: { children?: React.ReactNode }) {
  return (
    <>
      <path d="M14 3.5H7A1.5 1.5 0 0 0 5.5 5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8Z" />
      <path d="M14 3.5V8h4.5" />
      {children}
    </>
  );
}

/** Plain text, and the fallback for anything unclassified. */
export function FileIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <path d="M8.5 12.5h7M8.5 16h4.5" />
      </FileFrame>
    </Icon>
  );
}

/** Source code: the angle brackets an editor puts on a code file. */
export function FileCodeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <path d="m11 12.5-2 2 2 2M14 12.5l2 2-2 2" />
      </FileFrame>
    </Icon>
  );
}

/** Markdown: a document whose content is prose. */
export function FileTextIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <path d="M8.5 11.5h7M8.5 14.5h7M8.5 17.5h4" />
      </FileFrame>
    </Icon>
  );
}

/** Spreadsheet or CSV: a grid, which is what the viewer will show. */
export function FileTableIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <path d="M8.5 12h7M8.5 15.5h7M12 12v7" />
      </FileFrame>
    </Icon>
  );
}

/**
 * Image: the mountain-and-sun every platform uses for a raster.
 *
 * The ridge spans the frame's full inner width and the sun sits above its shoulder.
 * The first version kept both inside a 7-unit box in the lower half, where at 16px
 * the ridge became a short diagonal and the icon read as a file with a slash through
 * it — i.e. as "blocked", the opposite of a preview.
 */
export function FileImageIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <circle cx="9.6" cy="12.4" r="1" />
        <path d="M7.5 17.6l2.8-3.1a.9.9 0 0 1 1.3 0l1.1 1.3 1.1-1.2a.9.9 0 0 1 1.3 0l1.4 1.5" />
      </FileFrame>
    </Icon>
  );
}

/** PDF: a page with a label along the bottom, as in a viewer's sidebar. */
export function FilePdfIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <rect x="8.5" y="13" width="7" height="4.5" rx="1" fill="currentColor" fillOpacity="0.16" />
      </FileFrame>
    </Icon>
  );
}

/** A rendered web page. */
export function GlobeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17" />
      <path d="M12 3.5c2.2 2.4 3.4 5.4 3.4 8.5S14.2 18.1 12 20.5C9.8 18.1 8.6 15.1 8.6 12S9.8 5.9 12 3.5Z" />
    </Icon>
  );
}

export function FolderUpIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3.5 8.5A1.5 1.5 0 0 1 5 7h3.6a1.5 1.5 0 0 1 1.06.44l1 1A1.5 1.5 0 0 0 11.72 9H19a1.5 1.5 0 0 1 1.5 1.5v7A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5Z" />
      <path d="M12 16v-4M10 14l2-2 2 2" />
    </Icon>
  );
}

/* ── Composer and turn controls ───────────────────────────────────────────── */

export function PaperclipIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M18.5 11.5l-6.9 6.9a4 4 0 0 1-5.66-5.66l7.6-7.6a2.75 2.75 0 0 1 3.89 3.89l-7.6 7.6a1.5 1.5 0 0 1-2.12-2.12l6.9-6.9" />
    </Icon>
  );
}

/** Send. An arrow, not a paper plane: it points where the message goes. */
export function SendIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 19.5v-15M5.5 11 12 4.5 18.5 11" />
    </Icon>
  );
}

/** Stop a running turn. A filled square, the universal transport symbol. */
export function StopIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="6.5" y="6.5" width="11" height="11" rx="2" fill="currentColor" />
    </Icon>
  );
}

/**
 * Run an agent command: the ⌘ loop.
 *
 * Four separate circles plus a centre square, not one continuous path. The
 * single-path version drew all four arcs and their connecting strokes in one `d`,
 * and at 16px the arcs merged into the connectors — it read as an inkblot. Four
 * primitives stay legible because each is a closed shape with its own gap.
 */
export function CommandIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="9" y="9" width="6" height="6" />
      <circle cx="6.75" cy="6.75" r="2.25" />
      <circle cx="17.25" cy="6.75" r="2.25" />
      <circle cx="6.75" cy="17.25" r="2.25" />
      <circle cx="17.25" cy="17.25" r="2.25" />
    </Icon>
  );
}

/**
 * The model picker: a chip.
 *
 * Short pins, no interior circle. The first version had 3-unit pins and two
 * concentric shapes, which at 16px converged on a blob with radiating spokes — it
 * read as a flower. The pins are now 1.5 units: long enough to read as legs, short
 * enough not to dominate the body.
 */
export function ChipIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="7.5" y="7.5" width="9" height="9" rx="1.5" />
      <path d="M10.5 6v1.5M13.5 6v1.5M10.5 16.5V18M13.5 16.5V18M6 10.5h1.5M6 13.5h1.5M16.5 10.5H18M16.5 13.5H18" />
    </Icon>
  );
}

/** Reasoning effort. More sparkle, more thinking. */
export function SparklesIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M11 4.5l1.4 3.6L16 9.5l-3.6 1.4L11 14.5 9.6 10.9 6 9.5l3.6-1.4Z" />
      <path d="M17.5 14.5l.7 1.8 1.8.7-1.8.7-.7 1.8-.7-1.8-1.8-.7 1.8-.7Z" />
    </Icon>
  );
}

/* ── Timeline ─────────────────────────────────────────────────────────────── */

export function TerminalIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
      <path d="m7.5 10 2.5 2.5-2.5 2.5M13 15h3.5" />
    </Icon>
  );
}

export function PencilIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M16.4 4.6a2 2 0 0 1 2.83 2.83L8.5 18.15l-4 1.17 1.17-4Z" />
      <path d="m15 6 3 3" />
    </Icon>
  );
}

/** A file read. An eye: the agent looked, it did not change anything. */
export function EyeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M2.5 12S6 6.5 12 6.5 21.5 12 21.5 12 18 17.5 12 17.5 2.5 12 2.5 12Z" />
      <circle cx="12" cy="12" r="2.75" />
    </Icon>
  );
}

/** A file written or created. */
export function FilePlusIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <FileFrame>
        <path d="M12 11.5v5M9.5 14h5" />
      </FileFrame>
    </Icon>
  );
}

/**
 * Delegated work: a subagent, a task, a fork count.
 *
 * One icon for all three on purpose — they are the same relationship seen from
 * different surfaces, and giving them separate glyphs would imply otherwise.
 */
export function BranchIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="7" cy="6" r="2.25" />
      <circle cx="7" cy="18" r="2.25" />
      <circle cx="17" cy="10" r="2.25" />
      <path d="M7 8.25v7.5M9.25 6h3.25A4.5 4.5 0 0 1 17 10v-.25" />
    </Icon>
  );
}

/**
 * Fallback for a tool whose name matches no known shape: a wrench.
 *
 * One outline whose head carries an open notch. The first attempt drew a thin
 * diagonal body with a separate tick, which collapsed into a sliver at 16px and read
 * as a dagger — the notch that makes a wrench a wrench was the first detail to go.
 */
export function ToolIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M15.6 4.8a4.6 4.6 0 0 0-4.1 6.3l-6.2 6.2a1.7 1.7 0 0 0 2.4 2.4l6.2-6.2a4.6 4.6 0 0 0 6.3-4.1l-2.8 2.8-3.2-.6-.6-3.2Z" />
    </Icon>
  );
}

/** An unpaired tool result: output arriving without its call. */
export function ReplyIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M8 5v6.5a2 2 0 0 0 2 2h9" />
      <path d="m16 10.5 3.5 3-3.5 3" />
    </Icon>
  );
}

/* ── Status ───────────────────────────────────────────────────────────────── */

export function AlertTriangleIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M10.6 4.8a1.6 1.6 0 0 1 2.8 0l7 12.2a1.6 1.6 0 0 1-1.4 2.4H5a1.6 1.6 0 0 1-1.4-2.4Z" />
      <path d="M12 9.5v4M12 16.6v.01" />
    </Icon>
  );
}

export function ArchiveIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="4.5" width="18" height="4" rx="1.5" />
      <path d="M4.5 8.5v9A1.5 1.5 0 0 0 6 19h12a1.5 1.5 0 0 0 1.5-1.5v-9" />
      <path d="M10 12h4" />
    </Icon>
  );
}

export function PinIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 15.5V21" />
      <path d="M8 4.5h8l-1 6 2.5 2.5v1H6.5v-1L9 10.5Z" />
    </Icon>
  );
}

export function RefreshIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M19.5 12a7.5 7.5 0 1 1-2.6-5.7" />
      <path d="M19.5 4.5V7h-2.5" />
    </Icon>
  );
}

export function MaximizeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9 4.5H6A1.5 1.5 0 0 0 4.5 6v3M15 4.5h3A1.5 1.5 0 0 1 19.5 6v3M19.5 15v3a1.5 1.5 0 0 1-1.5 1.5h-3M9 19.5H6A1.5 1.5 0 0 1 4.5 18v-3" />
    </Icon>
  );
}

export function CopyIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M15 9V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h3" />
    </Icon>
  );
}

/** Overflow menu. Vertical, so it reads as "more for this row". */
export function MoreIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="12" cy="5.5" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="18.5" r="1.4" fill="currentColor" stroke="none" />
    </Icon>
  );
}

/* ── Session origin ───────────────────────────────────────────────────────── */

/**
 * Origin markers.
 *
 * Two different *shapes*, filled, not two colours of the same dot: origin decides
 * identity and lifecycle policy, so it has to survive greyscale and colour
 * blindness (WCAG 1.4.1). The circle is a Sylph session, the diamond a native one
 * whose originating agent's identity is preserved.
 */
export function OriginSylphIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="12" cy="12" r="5" fill="currentColor" stroke="none" />
    </Icon>
  );
}

export function OriginNativeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 6.5l5.5 5.5L12 17.5 6.5 12Z" fill="currentColor" stroke="none" />
    </Icon>
  );
}

/** A turn is running. Filled triangle, matching the transport convention. */
export function RunningIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M8.5 6.5 18 12l-9.5 5.5Z" fill="currentColor" stroke="none" />
    </Icon>
  );
}
