# API — Files and browsing

Reading and writing within a session's workdir, and naming a directory for a new
session. `docs/api/README.md` holds the conventions every endpoint here assumes.

## Files

Browsing and managing a session's working directory, for the Artifacts pane. Reads
are unrestricted within the root. Writes are three narrow endpoints — create a
file (`POST .../files`), create a directory (`POST .../files/directory`), delete
one entry (`DELETE .../files`) — and each refuses its destructive default: an
upload will not overwrite unless asked, a mkdir will not replace, and a delete
will not recurse unless asked and will not touch the root at all.

There is still deliberately no rename and no write-in-place: the agent mutates
file *contents*, the browser manages the files around them.

**Why delete exists at all.** The pane is the only view of the workspace a user has
while a session runs, and a file browser that cannot remove the file it just showed you
pushes the user to a terminal. It is acceptable because the guards are enforced
server-side rather than as UI conventions — see the endpoint below — and the cost is worth
stating plainly: this is the one irreversible operation in the API, because there is no
trash to restore from.

**Path safety.** These are the only endpoints that turn a client string into a
filesystem read, so containment is enforced on the *canonical* path, never on the
string. The root is the session's `workdir` as recorded in the journal, so no
request can choose its own root; the joined path is canonicalized, which resolves
`..` and every symlink component, and is then required to still be inside the
canonical root. A symlink pointing outside the workspace is refused and is also
omitted from listings. See `crates/server/src/api/files.rs` for the threat model.

### `GET /api/v1/sessions/{id}/files?path=`

One directory level. `path` is relative to the workdir; omitted or empty means the
root. Lazy expansion is what keeps a repository with 100k files usable.

**Response 200:**
```json
{
  "root": "/Users/you/Workspace/sylph-web",
  "path": "crates/server/src",
  "entries": [
    { "path": "crates/server/src/api", "name": "api", "isDir": true, "modifiedAtMs": 1755600000000 },
    { "path": "crates/server/src/lib.rs", "name": "lib.rs", "isDir": false, "size": 5120, "modifiedAtMs": 1755600000000 }
  ],
  "truncated": false
}
```

Directories sort before files, then case-insensitively by name. `.git`,
`node_modules`, `target`, `.next`, and similar machine-generated directories are
never listed. `truncated` is true past 2000 entries in one directory.

### `GET /api/v1/sessions/{id}/files/content?path=`

One file's contents.

**Response 200:**
```json
{
  "path": "README.md",
  "absolutePath": "/Users/you/Workspace/sylph-web/README.md",
  "size": 2048,
  "encoding": "text",
  "mimeType": "text/markdown",
  "content": "# Sylph Web\n...",
  "truncated": false,
  "modifiedAtMs": 1755600000000
}
```

`encoding` is `text`, `image`, or `binary`, and is decided by whether the bytes
decode as UTF-8 — not by the extension. A `.log` holding binary output is
therefore reported as `binary` rather than rendered as mojibake, and a file with
no extension is still served as text when it is text. For `image`, `content` is
base64. For `binary`, `content` is absent.

Text over 1 MiB is truncated to a 256 KiB prefix with `truncated: true`. An image
over 1 MiB is not served at all — a partial image decodes to nothing — and comes
back as `binary` with `truncated: true` so the client can explain why.

Errors: `FILE_NOT_FOUND` (404) for a missing path, `POLICY_VIOLATION` (403) for
one outside the workdir, `INVALID_REQUEST` (400) when a directory is requested as
content or vice versa.

### `GET /api/v1/sessions/{id}/files/document?path=&sheet=&row=&rows=`

A parsed representation of a file that is neither plain text nor an image:
spreadsheets, CSV, Word documents, and PowerPoint decks. Parsing is server-side
so the browser receives structure rather than bytes it would need a megabyte of
JavaScript to interpret.

**Why the server parses.** The npm option (`xlsx`, SheetJS) is frozen at 0.18.5
with two unpatched High CVEs reachable on exactly this "parse a file we did not
write" path, and is 7.5 MB unpacked against a first-load budget with ~4 KB of
headroom. `calamine` and `quick-xml` do the same work in the backend, where the
cost is compile time rather than every page load.

The response is a union discriminated on `kind`. `grid` covers
`.xlsx/.xlsm/.xlsb/.xls/.ods/.csv/.tsv`; `blocks` covers `.docx/.pptx`.

**Response 200** (`kind: "grid"`):
```json
{
  "path": "reports/sales.xlsx",
  "absolutePath": "/Users/you/proj/reports/sales.xlsx",
  "size": 20480,
  "modifiedAtMs": 1755600000000,
  "kind": "grid",
  "grid": {
    "sheets": [{ "name": "Sales", "rows": 5000, "cols": 12 }],
    "sheet": 0,
    "row": 0,
    "rows": [
      [{ "v": "Region", "t": "text" }, { "v": "Q1", "t": "text" }],
      [{ "v": "APAC", "t": "text" }, { "v": "1200", "t": "number", "n": 1200 }]
    ],
    "hasHeaderRow": true,
    "truncated": true
  }
}
```

Each cell carries `v`, the string to display, and `t`, one of `text`, `number`,
`bool`, `date`, `formula`, or `error`. Empty cells are `null` rather than an
object, because sheets are sparse and the difference is most of the payload. `t`
drives alignment — numbers and dates right, text left — and `n` carries the raw
number for a `number` cell so the client need not re-parse a formatted string.

Number formatting and date conversion happen server-side: `v` for a date cell is
already an ISO-8601 string, never an Excel serial. Per-cell currency and percent
formats are **not** reproduced — `calamine` folds the number format into the value
and exposes no per-cell format string, so only date-vs-number survives the trip.

`sheet` selects a worksheet by index (default 0). `row` and `rows` window the
result: `rows` defaults to 200 and caps at 500, and an out-of-range window is
clamped rather than refused, with the response echoing what was actually served.
`sheets[]` always reports every sheet's true dimensions, so a client can say what
it is not showing. Windowing is not cosmetic — a 50 000 × 30 sheet takes ~600 ms
to materialize and would be a ~50 MB response.

**Response 200** (`kind: "blocks"`):
```json
{
  "path": "notes/roadmap.pptx",
  "absolutePath": "/Users/you/proj/notes/roadmap.pptx",
  "size": 41984,
  "modifiedAtMs": 1755600000000,
  "kind": "blocks",
  "blocks": {
    "sections": [
      { "title": "Roadmap", "index": 0, "notes": "Mention the CVE research." }
    ],
    "blocks": [
      { "type": "heading", "level": 1, "text": "Quarterly Report" },
      { "type": "paragraph", "spans": [{ "text": "Revenue grew ", "bold": false },
                                        { "text": "12%", "bold": true }] },
      { "type": "listItem", "level": 0, "ordered": false, "text": "APAC led growth" },
      { "type": "table", "rows": [["Region", "Revenue"], ["APAC", "1200"]] }
    ],
    "truncated": false
  }
}
```

Word and PowerPoint share this shape: a slide is a section whose blocks follow it,
so one renderer serves both. Block `type` is `heading`, `paragraph`, `listItem`,
or `table`.

A structured block list is returned rather than Markdown or HTML. Markdown would
mean escaping document prose containing `*`, `_`, or `|` on the way out and
re-parsing it on the way in — two chances to corrupt text that was never Markdown.
HTML would reintroduce an `innerHTML` path into a frontend whose Markdown renderer
is built specifically to avoid one.

PowerPoint slide order comes from `p:sldIdLst` in `ppt/presentation.xml`, resolved
through `ppt/_rels/presentation.xml.rels`. Sorting slide filenames is wrong:
`slide10.xml` sorts before `slide2.xml`, and a deleted-then-re-added slide breaks
the correspondence entirely. Speaker notes come from the per-slide
`ppt/notesSlides/` part and appear as `sections[].notes`. Shape geometry is not
modelled; reading order is what a preview needs.

**Limits.** A file over 25 MiB is refused before parsing — `MAX_PREVIEW_BYTES`
(1 MiB) is the wrong bound here, since a legitimate spreadsheet is routinely
several MiB. Because an OOXML container is attacker-influenced input, a declared
part over the per-part cap is refused from the zip header before decompressing,
and the read is *also* bounded: a 199 KB archive can declare a 200 MB part, and
the declared figure is itself untrusted.

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403), and `INVALID_REQUEST`
(400) for a directory, an out-of-range sheet index, an extension this endpoint
does not handle, a file that is too large, or a corrupt, encrypted, or truncated
container. A client that receives `INVALID_REQUEST` for a mislabelled file may
retry once against `/files/content`, which decides text-vs-binary from the bytes.

### `GET /api/v1/sessions/{id}/files/raw?path=&download=`

The file's bytes, whole: images, HTML, PDF, and any file whose bytes are text.

**This is the route "open in a new tab" uses, and why it serves text.** The pane's
preview is bounded on purpose — 1 MiB read, 256 KiB of text returned — because a
side panel cannot usefully render a 40 MiB log and pushing one through JSON stalls
the pane. But "the preview is truncated" must not mean "you cannot see this file",
so the same file is reachable here in full and rendered by the browser's own
viewer, which streams, finds, zooms, and prints better than anything this pane
would reimplement. Nothing is truncated on this route.

`download=true` forces `Content-Disposition: attachment`. It is a separate
parameter from the type allowlist because the two answer different questions: the
allowlist decides what is *safe to render*, this decides what the user *asked
for*. Without it, Download on a PNG would open a tab showing the image.

This is what the image viewer uses, replacing base64-through-JSON. Base64 costs
33% inflation, a JSON parse before first paint, no progressive decode, and no
HTTP caching, and cannot serve a PDF at all. The route is not a weakening of the
model: it sits behind the same auth middleware and the same canonical-path
containment check as the two endpoints above.

**Response 200:** the raw bytes, with headers that are load-bearing rather than
decorative. Serving workspace HTML from this origin without them lets a script in
an agent-generated file read the `sylph_session` cookie — measured, not
hypothetical.

| Header | Value | Why |
| --- | --- | --- |
| `Content-Type` | allowlisted type, else `text/plain` for text, else `application/octet-stream` | Three tiers, never a guess — see below |
| `X-Content-Type-Options` | `nosniff` | Stops an octet-stream or `text/plain` payload being sniffed into `text/html` |
| `Content-Security-Policy` | `sandbox` | Forces an opaque origin however the response is loaded, so even direct navigation reaches neither DOM nor cookie |
| `Content-Disposition` | `inline` for renderable types, else `attachment`; always with the filename | A type we will not render is a download; without the filename it saves as `raw` |
| `ETag` / `Cache-Control` | mtime+size / `no-cache` | Re-viewing revalidates, so an agent's edit is still noticed |

**The three content-type tiers**, narrowest first:

1. An allowlisted type (`image/png`, `application/pdf`, `text/html`, …) is served
   as itself, because the browser has a real viewer for it.
2. Anything else whose bytes *are text* is served as `text/plain; charset=utf-8` —
   never as its own `text/…` label. A tab showing source is the point of this
   route, and `text/plain` cannot execute whatever the extension claimed.
3. Everything else downloads.

Tier 2 asks the bytes, not the extension: a prefix is checked for a NUL and for
UTF-8 validity. An extension table would need an entry per language to keep
`.svelte` or `.rb` from downloading and would still be wrong about a `.log` full
of binary — the same reason `/files/content` decides text-vs-binary by decoding.

`.svg` is deliberately never served as `image/svg+xml`: at that type a script
inside it runs in our origin, whereas the same file inside an `<img>` cannot. It is
text, so it lands in tier 2 and a tab shows its *source*. That is duller than
rendering the drawing and is the intended trade — the pane renders SVG through the
`/files/content` base64 path, inside an `<img>`, so nothing needs this route to
render it.

The filename in `Content-Disposition` is taken from the *canonical server-side
path*, never from the request, and is sent in both RFC 6266 spellings: the ASCII
`filename=` fallback plus the percent-encoded `filename*=UTF-8''`. Both, because a
CJK filename cannot be expressed in the plain parameter at all — a bare
`filename="报告.pdf"` is an invalid header value, which would have dropped the
disposition entirely.

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403) — including a symlink
resolving outside the workdir — and `INVALID_REQUEST` (400) for a directory.

### `POST /api/v1/sessions/{id}/files`

Write one file into the session's working directory.

```json
{ "path": "docs/assets", "name": "diagram.png", "data": "<base64>", "overwrite": false }
```

`path` is the destination *directory*, relative to the workdir; empty means the
root. `data` is base64 with no `data:` prefix — the same encoding turn attachments
use, so a client has one encoder. `name` is reduced to a single path component, so
a separator in it cannot rebuild a path.

**Response 201:**
```json
{ "path": "docs/assets/diagram.png", "absolutePath": "/Users/you/p/docs/assets/diagram.png", "size": 20480 }
```

**Why writing is allowed here at all.** Reading and writing are not symmetric
risks, so this does not inherit the read endpoints' justification. The argument is
narrower: the agent driving this session already writes anywhere under `workdir`
— that is what a coding agent does — so an upload confined to the same subtree
grants no capability the session did not already have. What it must not become is
a general file-writing service, so the root is the journalled `workdir` and never
a client-supplied directory, the destination goes through the same containment
check as reads, and an existing file is refused unless `overwrite` is explicitly
true. Per-file size shares the 8 MiB attachment ceiling.

**`overwrite` does not follow a symlink out of the workdir.** Containment for a
write is established on the *parent directory*, because the file usually does not
exist yet and canonicalization needs an existing path — which leaves the last
component unresolved. A pre-existing symlink there is inside the root by name and
points wherever it likes, and a plain write follows it, so `overwrite` was a write
outside the workdir arriving through the one component the check could not see.
An existing entry is now inspected with `symlink_metadata` and, when it is a link,
resolved and re-checked. A link resolving *inside* the workdir still writes through
to its target, because that is a write the agent itself can plainly make and
refusing every link would be stricter than the stated line; a dangling link is
refused, since where the write would land cannot be established and creating its
target would be inventing one.

`DELETE` was already safe from this by construction — unlinking a link never
touches its target — and its implementation asserts that a final escaping symlink
is "only ever unlinked, never opened or written to". That invariant held for
`DELETE` and not for this endpoint; it now holds for both. `POST .../directory`
refuses an existing target outright, so it was never reachable.

Errors: `POLICY_VIOLATION` (403) outside the workdir, including through a symlink
at the destination, `INVALID_REQUEST` (400) for an existing file without
`overwrite`, `ATTACHMENT_INVALID` (400) for malformed base64 or an oversized
payload.

### `POST /api/v1/sessions/{id}/files/directory`

Create one directory.

```json
{ "path": "docs", "name": "drafts" }
```

`path` is the *parent*, relative to the workdir; empty means the root.

**Response 201:**
```json
{ "path": "docs/drafts", "absolutePath": "/Users/you/p/docs/drafts" }
```

Its own route rather than a mode of `POST .../files`: one takes a body of bytes and
the other a name, and a handler switching on which field is present is how an
upload with a typo'd key silently becomes a mkdir.

Non-recursive, and it refuses an existing path rather than succeeding quietly, so
"New folder" twice reports the collision instead of looking like a no-op.

`name` is **rejected, not rewritten**, when it is unusable — a separator, `.`,
`..`, a control character, over 255 bytes. Deliberately not the upload sanitizer,
which strips non-ASCII: that is right for a filename derived from an upload, where
the name is incidental, and wrong for a folder the user is naming, where `文档`
silently becoming `attachment` is data loss with no visible cause.

Errors: `INVALID_REQUEST` (400) for a bad or taken name, `POLICY_VIOLATION` (403)
outside the workdir.

### `DELETE /api/v1/sessions/{id}/files?path=&recursive=`

Delete one file or one directory. **Response 204**, no body.

This is the only irreversible operation in the API. There is no trash to restore
from, because reaching the platform's trash means either a new dependency or
spawning `osascript`, and a browser page that can invoke a host process is a larger
concession than the delete itself. The guards are therefore the safety, and they
hold for any client, not only this project's UI:

- **The root is refused** with `403`. `path=""`, `"."`, and `"/"` all normalize to
  the workdir, and a client bug that sent one would otherwise erase the project.
  No legitimate request means "delete the working directory".
- **A non-empty directory needs `recursive=true`**, else `400`. Deleting one stray
  file and deleting a subtree are different intentions. An empty directory needs no
  flag, because there is nothing to lose.
- **A symlink is unlinked, never followed.** This one needs care: the read path
  canonicalizes, which resolves a link to its target, so deleting `link` would
  delete the *directory it points at*. So this endpoint canonicalizes the
  **parent** and appends the final component verbatim. Containment still holds —
  every `..` and every symlinked directory in the path is resolved and checked —
  and a final component that is itself an escaping symlink is only ever unlinked.

The UI sends `recursive` for any directory and states in one dialog that the
contents go too. Confirm-fail-confirm would be two dialogs for one decision, and
Finder does not ask twice either; the backend guard stays as protection against a
client bug rather than as a step in the flow.

Errors: `FILE_NOT_FOUND` (404), `POLICY_VIOLATION` (403) for the root or an escape,
`INVALID_REQUEST` (400) for a non-empty directory without `recursive`. Note the code is
`POLICY_VIOLATION`, not `FORBIDDEN` — that is the HTTP status name, and a client
branching on it never matches (`Error::Forbidden` → `POLICY_VIOLATION`,
`crates/server/src/error.rs:229`).

## Machine-level browsing

Choosing a directory to bind as a workspace. Separate from the session file API because
the root differs: that one is rooted at a session's `workdir`, this at an environment's
`browse_roots`.

**Why it exists.** A session cannot be created without a directory, and nothing in a
browser can name one on the machine that will run the agent. `<input type="file">` hides
real paths, `~` cannot be expanded client-side, and the machine that matters is the
envagent's rather than the browser's.

**Whose filesystem.** An environment's, and the answer is per environment: the roots are
`browse_roots` on that machine, and canonicalization happens in the envagent that holds
the path, since no other process can resolve a symlink it cannot see
(`docs/configuration.md` § `browse_roots` bounds where agents run owns the bound and the
check). A listing is therefore a bridge round trip, not a local `readdir`.

Today it is still a local `readdir` in the server, against the server's own configuration
(`crates/server/src/api/fs.rs`). That is the same in-process seam Milestone 1 removes, and
`docs/roadmap.md` § Milestone 2 carries the exit gate that closes it: *no remote path
check happens in the server.* Until then this endpoint answers correctly only for an
environment on the server's own machine, which is the only kind that can be paired.

Narrower than the session file API in three ways: only directories are listed, so file
names stay undiscoverable; there is no content endpoint, so reading bytes stays
session-scoped; and a symlink leaving the root is omitted from listings rather than
refused on access. Dotfiles *are* listed, since `~/.sylph` is a legitimate workspace the
product points at.

### `GET /api/v1/fs/roots`

```json
{ "roots": [{ "name": "Home", "path": "/Users/you" }], "home": "/Users/you" }
```

`home` is the OS-user home on the environment's machine, so a client can abbreviate paths
as `~/…` without guessing. A configured root that no longer exists is filtered out rather
than returned as an unopenable node.

### `GET /api/v1/fs/tree?path=`

One directory level, subdirectories only. Omitted or empty `path` means the first
configured root, so a client's first request needs no prior knowledge of them.

```json
{
  "path": "/Users/you/Workspace",
  "parent": "/Users/you",
  "entries": [{ "path": "/Users/you/Workspace/sylph-web", "name": "sylph-web", "modifiedAtMs": 1755600000000 }],
  "truncated": false
}
```

`parent` is **absent** when `path` is itself a configured root. The server
withholds it rather than the client deciding where to stop, so a "go up"
affordance can never offer a path the next request would refuse.

Errors: `POLICY_VIOLATION` (403) for a path outside every root, `FILE_NOT_FOUND`
(404) for one that does not exist.

