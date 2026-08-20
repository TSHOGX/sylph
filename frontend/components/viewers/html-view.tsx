"use client";

/**
 * A workspace HTML file, rendered in a frame that can do nothing.
 *
 * # Why rendering by default is safe here
 *
 * These files may be agent-generated, so they are untrusted, and a page rendered
 * with our privileges could read the DOM and issue same-origin fetches carrying
 * the `sylph_session` cookie — which authorizes reads across the whole workdir.
 *
 * `sandbox=""` removes that capability rather than gating it. An empty value
 * withholds every permission, including `allow-scripts` and `allow-same-origin`.
 * Verified in headless Chromium before this was written: the markup renders and
 * inline CSS applies, while an inline `<script>` does not run, `parent.document`
 * is unreachable, and `document.cookie` inside the frame throws `SecurityError`.
 *
 * The attribute must stay exactly `sandbox=""`. Adding `allow-scripts` and
 * `allow-same-origin` together is the specific combination that lets the framed
 * document remove its own sandbox, and either one alone re-opens a path this
 * relies on being closed.
 *
 * # Why srcdoc
 *
 * The bytes already arrive through the authenticated JSON endpoint, so `srcdoc`
 * needs no second route serving `text/html` from our origin. A blob URL was
 * rejected for inheriting the creating origin — a latent footgun the day someone
 * adds `allow-same-origin` — and for needing revocation to avoid leaking.
 */

import { useMemo, useState } from "react";
import { buildSandboxDocument, describeBlocked } from "@/lib/sandbox-html";
import { CodeBlock } from "@/components/code-block";

export function HtmlView({
  html,
  name,
  /** Reset the frame when the file changes, rather than reusing its document. */
  path,
}: {
  html: string;
  name: string;
  path: string;
}) {
  const [rendered, setRendered] = useState(true);
  const sandboxed = useMemo(() => buildSandboxDocument(html), [html]);
  const blocked = describeBlocked(sandboxed);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 border-b border-gray-200 bg-gray-50 px-2 py-1">
        <span className="flex-1 text-[10px] text-gray-500">
          {rendered ? "Rendered without scripts or network access" : "Source"}
        </span>
        <button
          type="button"
          onClick={() => setRendered(!rendered)}
          className="rounded border border-gray-200 bg-white px-1.5 py-0.5 text-[10px] font-medium text-gray-700 hover:bg-gray-100 focus:outline-none focus-visible:ring-2 focus-visible:ring-gray-900"
        >
          {rendered ? "View source" : "View rendered"}
        </button>
      </div>

      {rendered && blocked && (
        // Said plainly rather than shown as a broken page: `srcdoc` resolves
        // relative URLs against the parent document, so workspace-relative
        // images cannot load, and the CSP blocks external ones.
        <p className="shrink-0 border-b border-amber-200 bg-amber-50 px-2 py-1 text-[10px] text-amber-900">
          {blocked}
        </p>
      )}

      {rendered ? (
        <iframe
          // Re-keyed on the path so opening another file starts a fresh document
          // rather than reusing one the user may have navigated inside.
          key={path}
          // Empty on purpose. See the module comment: this is the security
          // boundary, not a default to be relaxed.
          sandbox=""
          // A sandbox does not stop subresource referrers, and a clicked link can
          // still navigate the frame itself.
          referrerPolicy="no-referrer"
          srcDoc={sandboxed.html}
          title={`Rendered preview of ${name}`}
          className="min-h-0 w-full flex-1 border-0 bg-white"
        />
      ) : (
        <div className="min-h-0 flex-1 overflow-auto p-2">
          <CodeBlock code={html} language="html" maxLines={Number.MAX_SAFE_INTEGER} />
        </div>
      )}
    </div>
  );
}
