/**
 * Preparing a workspace HTML file for display in a sandboxed frame.
 *
 * # The threat, and why this is not the whole defence
 *
 * These files may be agent-generated, so they are untrusted. The frame they render
 * in carries `sandbox=""` — an empty value, withholding every permission including
 * `allow-scripts` and `allow-same-origin` — which is what actually removes the
 * danger. Verified in a headless browser: with an empty sandbox, an inline
 * `<script>` does not run, `parent.document` is unreachable, and even
 * `document.cookie` inside the frame throws `SecurityError` rather than returning
 * the session token.
 *
 * This module adds defence in depth on top of that. It injects a restrictive CSP
 * so that network requests do not happen even though scripts cannot initiate
 * them, which stops a plain `<img src="https://tracker/x.gif">` from beaconing on
 * open and carrying a referrer. Neither layer depends on the other being correct.
 *
 * # What is deliberately not attempted
 *
 * No sanitizing, stripping, or rewriting of the markup. A hand-written sanitizer
 * is a permanent liability — the bypass list is endless and every miss is a
 * vulnerability — and it is unnecessary precisely because the sandbox removes
 * script execution wholesale rather than trying to enumerate what is dangerous.
 */

import type { Translate } from "@/lib/i18n";

/** What `buildSandboxDocument` produces. */
export interface SandboxDocument {
  /** The markup to hand to `srcdoc`. */
  html: string;
  /**
   * External references found in the source.
   *
   * The frame will not load them, so the rendering is not what a browser would
   * show. The viewer says so rather than presenting a silently incomplete page.
   */
  blockedExternal: number;
  /**
   * Workspace-relative references found in the source.
   *
   * These break for a structural reason: `srcdoc` gives the document a base URL
   * inherited from the parent, so `./chart.png` resolves against the app's own
   * origin, where nothing answers. Counted separately from external references
   * because the causes differ and the fix would too.
   */
  blockedRelative: number;
}

/**
 * The policy injected into every previewed document.
 *
 * `default-src 'none'` blocks every fetch — remote images, stylesheets, fonts,
 * frames. `style-src 'unsafe-inline'` keeps the inline `<style>` blocks that
 * generated reports actually use for their entire appearance; it permits no
 * network access, so it costs nothing here. `img-src data:` keeps already-inlined
 * images working. Scripts are absent from the policy entirely, so they are denied
 * by `default-src` — redundant with the sandbox, which is the point.
 */
const POLICY =
  "default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:";

const META = `<meta http-equiv="Content-Security-Policy" content="${POLICY}">`;

/**
 * Prepare raw HTML for a sandboxed frame.
 *
 * The CSP is inserted as early as the parse allows, but never before a doctype:
 * an element ahead of the doctype puts the document into quirks mode, which
 * changes the box model and visibly breaks layouts that were fine.
 */
export function buildSandboxDocument(raw: string): SandboxDocument {
  const references = countReferences(raw);

  return {
    html: injectPolicy(raw),
    ...references,
  };
}

/** Place the policy meta tag at the earliest valid position. */
function injectPolicy(raw: string): string {
  // Inside `<head>` is where a meta tag belongs, and it must precede any element
  // that could trigger a fetch.
  const head = raw.search(/<head\b[^>]*>/i);
  if (head !== -1) {
    const end = raw.indexOf(">", head);
    if (end !== -1) {
      return `${raw.slice(0, end + 1)}${META}${raw.slice(end + 1)}`;
    }
  }

  // No head: after `<html>` so the tag still lands inside the document element.
  const html = raw.search(/<html\b[^>]*>/i);
  if (html !== -1) {
    const end = raw.indexOf(">", html);
    if (end !== -1) {
      return `${raw.slice(0, end + 1)}${META}${raw.slice(end + 1)}`;
    }
  }

  // A doctype with no html tag. The policy goes after it, never before: leading
  // content ahead of the doctype triggers quirks mode.
  const doctype = raw.match(/^\s*<!doctype[^>]*>/i);
  if (doctype) {
    const end = doctype[0].length;
    return `${raw.slice(0, end)}${META}${raw.slice(end)}`;
  }

  // A bare fragment, which is common for generated snippets. Prepending is safe
  // because there is no doctype to displace.
  return `${META}${raw}`;
}

/**
 * Count references the frame will not load.
 *
 * A regex, not a parse. This drives a banner, not a security decision — the
 * sandbox and the CSP do the enforcing — so approximate counting is appropriate
 * and a DOM parse would cost a dependency for a number shown in a sentence.
 */
function countReferences(raw: string): {
  blockedExternal: number;
  blockedRelative: number;
} {
  let blockedExternal = 0;
  let blockedRelative = 0;

  // `src` and `href` on any element, single or double quoted.
  const pattern = /\b(?:src|href)\s*=\s*["']([^"']+)["']/gi;
  for (const match of raw.matchAll(pattern)) {
    const target = match[1].trim();
    if (target === "" || target.startsWith("#")) continue;
    // Already inline: nothing to load.
    if (/^(?:data:|about:|javascript:)/i.test(target)) continue;
    if (/^(?:[a-z][a-z0-9+.-]*:)?\/\//i.test(target)) {
      blockedExternal += 1;
      continue;
    }
    if (/^[a-z][a-z0-9+.-]*:/i.test(target)) {
      // `mailto:`, `tel:` and friends need no fetch and work as links.
      continue;
    }
    blockedRelative += 1;
  }

  return { blockedExternal, blockedRelative };
}

/**
 * One sentence describing what the preview is not showing, or `null`.
 *
 * Returning null rather than an empty string so the caller renders no banner at
 * all for a self-contained page, which is the common case.
 */
export function describeBlocked(document: SandboxDocument, t: Translate): string | null {
  const { blockedExternal, blockedRelative } = document;
  const parts: string[] = [];
  // The plural rule lives in the catalogue: English inflects both nouns, and a
  // language that does not simply gives the two keys the same string.
  if (blockedRelative > 0) {
    parts.push(
      t(blockedRelative === 1 ? "html.blockedFilesOne" : "html.blockedFiles", {
        count: blockedRelative,
      }),
    );
  }
  if (blockedExternal > 0) {
    parts.push(
      t(blockedExternal === 1 ? "html.blockedExternalOne" : "html.blockedExternal", {
        count: blockedExternal,
      }),
    );
  }
  if (parts.length === 0) return null;
  return t("html.blocked", { parts: parts.join(t("html.blockedJoin")) });
}
