# Unified Trace Reading and Interaction

Status: **Normative reading and interaction contract.**

Implementation and acceptance gates live in
[roadmap.md](roadmap.md#unified-trace-foundation-review).

## Authority and objective

[unified-trace-foundation.md](unified-trace-foundation.md) owns the semantic objects,
identity, delivery evidence, history selection, and lifecycle. This document owns how
people read and inspect those facts. [unified-trace-storage.md](unified-trace-storage.md)
owns persistence, projection publication, and consistent reads;
[unified-trace-conformance.md](unified-trace-conformance.md) owns the shared evidence
corpus. UI tests consume those contracts rather than infer vendor meaning again.

The interface lets a reader answer, in order: what was requested, what this agent did,
what it answered, which work continues, and what evidence supports a surprising result.
A child agent receives the same reader as its parent. The interface does not require a
complete tree, trustworthy timestamps, or model-request visibility to remain useful.

The existing [ui-layout.md](ui-layout.md) and
[frontend-architecture.md](frontend-architecture.md) govern unrelated workspace surfaces
and remain descriptions of the current implementation. Adoption replaces their trace
reading sections with pointers here. Existing tool-card containment, turn groups,
frontend payload classifiers, and terminal-turn refetch rules do not constrain this
design. In particular, terminal execution and fully caught-up observation are separate.

## Information architecture

The addressable subject is an `AgentRef`, accompanied by a history selection and an
optional object anchor. Session references and embedded references are transported as
the foundation defines them. The browser neither creates substitute session IDs nor
resolves vendor identity itself. A server resolution of an existing address preserves
the reader's selection and reports any changed canonical target.

There are three workspace areas:

| Area | Primary question | Content |
| --- | --- | --- |
| Navigation | Which conversation do I want? | Workspaces, sessions, search, recent agent locations |
| Reader | What happened in this agent's conversation? | Attributed inputs, authored content, work summaries, execution boundaries |
| Inspector | What supports this detail? | Agents directory, selected operation/resource/context/metric, files |

The reader header names the focused agent, its environment, and the selected history.
An optional nickname is display text; an unnamed child says `Unnamed agent` with a
short identity suffix available for disambiguation. A root's title is not silently
reused as a child's name. A breadcrumb describes recorded spawn ancestry, with a
separate origin link for fork/context lineage. An `Origins` inspection action pages the
retained lineage assertions, including unresolved references and multiple origins.
A row says when origin navigation is unavailable, offers it only for authorized target
metadata, and links to its retained proof. It never chooses one of several assertions
as the original actor or turns context lineage into spawn ancestry. Long ancestry collapses into a bounded,
searchable navigation surface. It is never loaded recursively to open the reader.

The header offers `Find in this agent`, `Agents`, and `History`. `Details` opens
record coverage and measurements. The current control target remains visible beside
the composer even when an inspector is open. Neither changing inspection focus nor
closing a panel starts, stops, or redirects execution.
If exactly one observed activation lacks a terminal boundary while the agent's other
activations have terminated, its own-execution summary retains that unresolved observation.
Missing live coverage never turns it into active execution or lets an earlier completed
answer stand in for its outcome.

URLs retain the agent reference, explicit noncurrent history selection, and object
anchor. Browser Back restores the preceding reader and scroll anchor. Panel widths,
disclosures, and scroll offsets are local view state; they do not enter the record.
A shared link omits Subject authorization and transient request tokens.

### Desktop composition

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ Sylph      Find conversations                                      New task │
├──────────────┬──────────────────────────────────────┬────────────────────────┤
│ Workspaces   │ Build indexing                       │ Agents   Details Files │
│              │ Main agent · Current history         ├────────────────────────┤
│ sylph        │ Find   Agents   History     Details   │ Find agents…           │
│  Indexing    ├──────────────────────────────────────┤ All / Active / Needs me│
│  Import fix  │ You                                  │                        │
│              │ Make Chinese symbols searchable.    │ Index analysis         │
│              │                                      │ Completed · 2 runs     │
│              │ I will inspect the tokenizer first. │                        │
│              │                                      │ Query audit            │
│              │ › Work · 8 operations · 2 agents     │ Working · partial trace│
│              │                                      │                        │
│              │ Chinese symbols now match…           │ Open alongside  ↗      │
│              │                                      │                        │
│              │ Completed · Child work continues     │                        │
│              ├──────────────────────────────────────┤                        │
│              │ Send to Main agent                  │                        │
│              │ [message…]                  [Send]  │                        │
└──────────────┴──────────────────────────────────────┴────────────────────────┘
```

Navigation is optional and resizable. Prose uses a centered reading measure of
56–78 characters; code and tables may use the full reader width. The inspector has a
420 px minimum for an agent reader and can grow to 640 px. Alongside inspection is
offered only when the main reader retains at least 520 px after visible navigation
and dividers. When space is insufficient, inspection becomes a full reading surface
with Back. Width is measured from available container space, including browser zoom.

The inspector has stable `Agents`, `Details`, and `Files` destinations. An explicitly
opened child replaces its Agents content with that child's reader and an `All agents`
return link. Opening an operation uses Details with an owner link and a back stack.
An existing pinned detail may be retained in a bounded tab list; opening new transient
details replaces the transient tab. No auto-opened tab is created for each tool or child.

The main reader and companion reader have independent drafts, find scope, disclosure
state, selected history, and scroll anchors. Entity payloads may be shared when their
revision is identical. Only the main reader offers the composer by default; a child
offers `Open conversation` to become the main target before composing. This keeps
simultaneous observation from making message routing ambiguous.

### Mobile and reflow

At widths below 768 px the primary reader fills the available viewport. Workspace
navigation uses a drawer. Child conversations, operation results, history selection,
and context inspection use full-height surfaces with an explicit title and Back;
their long content is not compressed into a half-height sheet. A short file preview
may use a sheet with an expand action, following the existing file interaction model.

```text
┌──────────────────────────────┐   ┌──────────────────────────────┐
│ Menu   Main agent     Agents │   │ Back   Query audit      More │
│ Current history       Find  │   │ Child of Main agent          │
├──────────────────────────────┤   ├──────────────────────────────┤
│ You                         │   │ Task from Main agent         │
│ Check query correctness.    │   │ Check Unicode boundaries.    │
│                             │   │                              │
│ › Work · 2 agents           │ → │ › Work · 5 operations       │
│                             │   │                              │
│ Main agent's answer…        │   │ Query audit's answer…        │
├──────────────────────────────┤   ├──────────────────────────────┤
│ Send to Main agent          │   │ Open conversation            │
│ [message…]           [Send] │   │ Back to parent               │
└──────────────────────────────┘   └──────────────────────────────┘
```

The shell uses dynamic viewport height, safe-area padding, and a single primary scroll
container per reading surface. The keyboard shrinks the visible reader; it does not
push the composer below the viewport. A focused textarea uses at least 16 px text.
Coarse-pointer actions have at least 44 by 44 CSS px hit areas. Every hover action has
an always-reachable menu or button. Reading works at 320 CSS px without page-level
horizontal scrolling; wide code/table content owns its horizontal scroll.

## The conversation reader

### Reading units and authorship

The server supplies bounded reading units referencing semantic objects. The browser
draws those units without interpreting vendor JSON, tool names, text prefixes, or a
parent's latest status. Presentation grouping is reproducible query work; expansion is
the reader's choice. A group cannot assign ownership or create an activation.

| Recorded fact | Reading treatment |
| --- | --- |
| Human message admitted to this agent | Attributed input referencing the exact submitted revision when retained, the admitted representation, and attachment parts |
| Assignment or message from another agent | `Task from…` or `Message from…`, with source-agent link when readable |
| Harness context or known automatic trigger | Compact labeled context/trigger entry; details preserve content and delivery evidence |
| Unknown input author or purpose | `Input · source unknown`, with readable content; never styled as `You` |
| Progress content | Ordinary prose at slightly lower emphasis, readable in place |
| Final content | Full prose in the reading flow, attached to its activation when known |
| Unclassified prose | Full readable content; absence of a phase does not hide it |
| Operation interval | Bounded work summary linking to its operations and independently running resources |
| Measurement observation | Updated Details measurement view, without inserting a conversational paragraph |
| Context replacement or history selection | Labeled boundary with context/history inspection actions |

Human input uses a restrained surface and explicit author label. Other-agent input
uses a small routing label rather than borrowing the human bubble. Assistant prose
stays on the canvas. Unknown authorship does not receive a human avatar. Dense metadata
is secondary text with sufficient contrast; unknown content does not become faint.
Disputed message metadata does not hide an independently resolved content revision.
The reading projection keeps fields on which the retained alternatives agree, leaves
disputed authorship and purpose unresolved, and does not attach a disputed production
scope to an activation. The underlying declarations remain conflicted; presentation
does not manufacture a replacement fact or select a competing author.

An authored answer and its delivery notification are distinct recorded occurrences.
The child reader shows the child's answer; the parent shows the result delivery it
actually received, linking back to that answer only where evidence proves the relation.
If the notification contains a shortened representation, the parent keeps that text.
The interface does not copy the child's complete answer into the parent's transcript.

Several evidence updates for the same delivery occurrence keep one reading address.
Its evidence drawer explains acceptance, admission, and known model inclusion. Two
deliveries of the same message to one recipient remain distinct occurrences, even when
their bytes are identical. Queued revisions do not replace the immutable revision already
dispatched. Retained submitted human text stays visible unchanged; any native transformed
representation is separately inspectable. When several recipients receive one authored
message, each reader shows its own delivery rather than inventing several authors.

Live and native observations that lack equivalence proof remain separately attributable.
Their source labels explain similar-looking content without asserting duplicate execution.
An explicit source-surface filter may narrow inspection with coverage visible; it uses
the same reader and never silently deduplicates by text, time, or tool name.

### Activations and work

A known activation boundary gives a quiet separator with trigger, execution outcome,
and a duration only when supported. One activation can contain several input deliveries
and inference operations. A follow-up to an existing child appears in that same child
conversation, with another activation only when the source establishes one.

Content without a proven activation remains on the agent's ordered history. It may be
introduced by `Execution boundary unavailable`; the interface does not construct a fake
turn so that a layout component can mount. A date separator is view organization and
never stands in for an execution boundary.

Work summaries state the actual scope: operation count, observed agent interactions,
requests requiring action, and still-running resources where known. Counts come from
the server over the selected history and authorization scope. `2 agents` counts agent
identities; `5 assignments` counts interaction occurrences. Repeated activity events do
not inflate either. The parent activation's status is its own outcome.

Completed operation runs begin collapsed. Active work shows its current bounded summary
and the most recent observable progress; it does not expand every operation body.
Opening Work shows an ordered, virtualized operation list. Nested execution becomes
drill-down within the inspector instead of recursive unbounded indentation. A reader's
explicit disclosure choice survives updates and completion. Completion does not collapse
selected text or remove the currently focused control.

Pending human requests remain reachable in a compact action strip outside a collapsed
work group. A request names its agent and affected operation, if known. A child failure
may produce `Completed · 1 operation failed` on a successful parent activation, never a
red failed parent. Unknown child execution is an observation limitation, not a failure
count.

### Status, time, and uncertainty

| Evidence | User-facing language and behavior |
| --- | --- |
| Current execution is observed active | `Working`, with restrained motion unless reduced motion is requested |
| Own terminal outcome is success | `Completed`; failed child/tool observations remain separate |
| Cancellation/interruption is established | Name the outcome and recorded reason; do not imply successful completion |
| Start was observed but observation ended without a close | `Last observed working · outcome unknown`, with last observation time |
| No execution boundary is available | `Execution status unavailable`, without a spinner |
| Explicit close conflicts with another claim | `Conflicting outcome evidence`; inspect the competing claims |
| Environment or control link is unavailable | Separate reachability/control notice; retained conversation remains readable |
| Semantic projection trails durable evidence | `Updating history`; show last readable state and retryable fault details |
| Source provides partial operation/child content | Scope the notice to the affected content, not the whole conversation |

The Details view exposes coverage by dimension: content, lifecycle, ordering, context,
internal operations, and measurements. These are not a single percentage or an ordered
quality badge. `Unavailable` names the source limitation or read failure when known.
An observed empty result and an unavailable result have different empty states.

Elapsed duration, vendor-reported duration, and server-observed duration have explicit
labels. Wall-clock alignment is drawn only for compatible known endpoints and clock
domains. A reported duration alone supports a duration label, not placement on a shared
timeline. Unknown endpoints render a gap rather than a zero-length bar. Progress can
show `Waiting for agent` without estimating a completion time. Parallel work is never
summed into a wall-clock total.

### Long reading and live arrival

The reader has explicit `Following latest` and anchored-reading behavior. Scrolling
up, selecting text, using Find, or choosing a historical object pauses following.
New content creates a `New activity` affordance; it does not move the current paragraph.
Selecting it moves to the latest consistent view and resumes following. A terminal
activation does not close the agent's trace subscription: other work or late evidence
may continue to arrive.

Before any structural update or prepend, capture the first visible stable object
address and its pixel offset. Resolve that address in the next consistent view and
restore the offset after layout. Height changes in Markdown, images, and expanded
details use that same anchor. Focus and a text selection are stronger than tail-follow
intent. Native scroll anchoring and programmatic anchoring must not both compensate.

Paused reading pins its snapshot. A correction offers `History updated` without silently
rewriting the visible paragraph. Accepting an update swaps a coherent window and keeps
the anchor. When the anchor is outside the new selected history, retain access to its
old history and explain the change before offering the nearest new location. A forced
restart from an expired snapshot uses this same recovery flow; it does not jump to the
tail or clear a composer draft.

If a new interpretation withdraws the anchored object entirely, the reader explains
that the interpretation changed and offers retained supporting evidence or an explicitly
identified replacement. It never guesses a replacement by similar text or reports that
the whole conversation disappeared.

## Agents, interactions, and persistent resources

The Agents directory is a server-paged, server-searchable catalogue of proven agent
identities within a named scope. It supports all, observed active, requiring input,
completed, and outcome-unknown filters. The filter is a recorded condition, not an
inference from child counts. Search covers the whole authorized catalogue and reports
its scope. Expanding a directory does not fetch child conversations.

The default scope is the main reader's agent and its recorded spawn descendants. Opening
a companion reader preserves that directory scope; promoting a child to the main reader
changes the default scope to that child. Parent navigation remains available through the
recorded ancestor link. Counts distinguish the focused agent from descendant totals.
The focused row is labeled separately from spawn descendants. Directory pages move
forward, with an explicit `First page` action. Empty work-limited search pages continue
automatically while preserving the scope; visible pages wait for navigation. If a count
is not yet complete, a separately cancellable metadata-only scan finishes it without
loading child bodies or blocking directory interaction. A changed query or closed
directory cancels that scan. A replacement window lease preserves the exact snapshot,
and Back restores the returned view and directory position.

Each row exposes name or neutral fallback, vendor, recorded relation, own latest known
execution state, and coverage when it affects opening. The default order is stable
spawn/source order with the server's deterministic tie-breaker; activity sorting is an
explicit option. Rows do not jump between sections while the user navigates them.
Counts are exact for the served scope and cut where the projection can establish them;
unknown completeness is labeled separately. An unauthorized target contributes neither
an entry nor a count. A visible source message may retain its literal recipient text,
but no hidden target metadata is resolved from it.

`Open alongside` is a reading action. `Open conversation` makes the child the main reader.
Both restore previous local reading state when revisited. Back returns to the original
object or directory row. Sibling communication is shown as an interaction link, not as
spawn ownership. Forks have their own lineage navigation and are not counted as spawned
children merely because they share context.

A spawn operation that failed remains an inspectable failed operation. An unresolved
child reference appears in that operation as `Child identity not established`, with
available evidence. It never creates a blank agent directory row. Once explicit
evidence resolves it, the same operation gains the child link. Late equivalence of two
references updates navigation consistently without treating the second reference as a
newly spawned agent.

Goals, terminals, background code cells, and schedules are resources. Their inspector
shows state, creating/using operations, and related activations. Parent completion does
not make the resource completed. A goal continuation labels the triggering input; a
sleep call appears as waiting work. A schedule registration and its observed firings are
separate rows; a future time or a sleep does not become a recurring-schedule widget.

Resource details expose a bounded page of retained state observations, with a continuation
at the same view. Registration, firing, cancellation, goal updates, yield and close keep
separate rows and supporting claim/source references. Comparable source coordinates establish source order; opaque snapshot identities and
source-less claims keep explicitly labeled presentation order. Grouping independent sources does not establish a shared timeline or
one current state. An absent state is unknown, not `Available` or `Running`. State detail
payloads load only on explicit expansion. Creator/using operations and their known
activations remain linked independently of resource state.

A message revision opens its own immutable representation and a bounded page of payload
parts. The submitted revision and recorded wrapper can both be inspected through their
representation links. A revision detail never silently substitutes the selected message's
newest body, and pagination does not download bodies before expansion. An append record
exposes its exact segment with an explicit predecessor link and segment label; it does
not present the delta as a complete historical message or eagerly compose every prefix.

## Operation and payload inspection

An operation header shows its supplied semantic kind, owner agent, activation when
known, lifecycle, bounded input summary, and result-availability summary. The server
provides the classification; the browser does not reinterpret a tool named `Agent` or
a vendor `success` field. An unknown kind uses a generic operation viewer with evidence
and structured content available.

An observed invocation remains an inspectable Work item when its producer is unknown
or unresolved. The row reports unknown ownership and opens that operation's retained
inputs, results and evidence. Its history entry remains the retention anchor; the
containing reader supplies neither an owner nor an activation. Unknown ownership is
not a grouping key for combining separate invocations.

The result surface distinguishes these representations:

| Representation | Label | Meaning |
| --- | --- | --- |
| Runtime observation | `Runtime output` | Bytes or structured result observed from the process/service |
| Immediate caller result | `Returned to caller` | Representation returned to the invoking tool/code cell |
| Recorded conversation content | `Recorded conversation result`; `Included in model context` only with inclusion evidence | An admitted representation, plus any proven inclusion in a named inference/checkpoint |

`Returned to caller` is selected initially when available because it answers what the
invocation returned. Otherwise the first available representation is selected with its
own label. Unavailable representations remain explained in the representation chooser;
selection never falls back silently while preserving the old label. Equal content may
share a payload, but provenance and occurrence remain distinct. `Compare representations`
fetches only explicitly selected bodies and names truncation or transformation evidence.

Representation choices come from the authorized result catalogue at the retained view,
not from whichever result occurrences fit in an initial page. Each representation has
its own bounded occurrence pages and cursor scope. Switching representations unmounts
the preceding page and its body controls; comparison mounts only the explicitly chosen
representations. Missing result bytes remain different from no observed result. An
unfinished availability probe is reported as pending with an explicit continuation,
never as absence. With no observed results and no pending probe, show the empty-result
explanation rather than an empty chooser. Initial selection prefers an established
available caller result, then another established available representation; a user's
explicit selection is not silently replaced by later catalogue discovery.

Progress, yield, and final results are ordered occurrences, not updates overwriting a
single result box. An operation that yields a resource handle links to the continuing
resource. A later poll or stdin write has its own return under that resource's operation
history. The resource's stdout history is not reconstructed by blindly concatenating
poll responses: overlapping or replacement snapshots follow the server's manifest.

Inputs, output channels, result occurrences, nested operations, and Evidence are separate
disclosures or tabs. Only the active body view fetches bytes. Nested tool calls belong
to their invoking operation and real owning agent; they do not appear in Agents unless
independent agent identity is proven. Orphan results remain readable with a missing-call
notice rather than being dropped.

Large content opens at a bounded preview with byte length when known, the represented
range, completeness, and `Load more` or `Download observed content`. Copy identifies its
scope: `Copy visible text` versus `Copy complete observed content`. Copy-all and export
are explicit bounded/streamed jobs; they do not first load an entire session into React.
An absent length says unknown. No button promises a complete original for a truncated
observation.

`Copy visible text` copies the currently displayed decoded source range without interface
labels. `Copy complete observed content` is an explicit text transfer with progress and
cancellation; it preserves UTF-8 across chunks and changes the clipboard only after a
complete successful transfer. A gap, invalid text, revoked access or exceeded budget
leaves the clipboard unchanged. Download preserves each observed interval and its coverage
in a ZIP manifest. Where the browser offers a streaming file writer, bytes go directly to
that transactional writer; otherwise a separately bounded download buffer is used. Neither
path grows the reader's payload cache or stores the complete body in React state. Only one
copy/download job per browser workspace runs at a time; ordinary reading remains usable.

Text ranges preserve UTF-8 boundaries and document the returned range when adjusted.
Structured values use paged object/array exploration. Opening it explicitly starts a
cancellable index build with progress and resolved limits. Ready exploration shows one
container's immediate children, bounded key/value previews, duplicate member occurrences
and links into nested containers. Navigation moves focus to the explorer heading and names the current container.
Back restores the preceding container/page; bounded
history and page replacement keep navigation independent of total JSON size. Large
scalar values remain exact-byte range reads. Malformed or incomplete JSON retains the
ordinary byte view with an explicit structural-unavailability reason.
Binary content retains its media
type, dimensions/duration when known, and safe preview/download behavior. Filesystem
paths resolve against the actual target environment and recorded working directory;
they do not inherit the parent reader's file context.

An edit shows proposed input and observed result separately. A diff claims an applied
change only when the captured before/after or explicit result proves it. Where only an
edit request exists, use `Proposed edit`. Resource output or a historical diff is never
replaced by reading the present-day file. Opening the current file is a separate action.

Evidence inspection exposes source, position, supporting claims, and correction history
on demand. It is a diagnostic surface, not metadata repeated below every paragraph.
The inspector opens one bounded page of claim previews and retains only one selected
claim detail. Supporting observations, related objects and explicit correction links
are paged within that detail. A source body is read only after an explicit action and
is displayed as a bounded exact byte range, with hexadecimal display when that range
is not valid UTF-8. Changing the claim or access scope clears its source range. Historical
claims are labeled as retained interpretations, not collectively presented as current
truth. Both preceding and later explicit corrections remain reachable.
Conflicting claims are displayed together with their scopes and why resolution remains
unknown. All captured content is untrusted data: Markdown cannot execute HTML or unsafe
links, structured trees cannot invoke tools, and file previews follow the existing
content isolation rules in [ui-layout.md](ui-layout.md).

## Context, forks, and history selection

The default is the source-established current history selection. History lists retained
selections with the native cause where known: fork, revert, branch change, or unknown
selection. The UI does not call a selection newest/current merely because its file was
modified last. When the source provides competing heads without proving a selected one,
the reader says so and lets the user choose a view without rewriting native state.

Selecting older history is read-only inspection. The composer becomes `Return to current
conversation to send`; browsing an old head does not dispatch into it or perform a vendor
revert. A real revert/fork command, if supported later, remains a distinct control action
with its own accepted result. Sharing a history link retains the selection.

History and model context are separate destinations. History preserves recorded work,
including a discarded suffix when retained. Context shows checkpoints, included message
references, replacement summaries, and known exclusions. Inherited content is attributed
to its original owner and labeled `Inherited context`; it is not counted as the child's
own execution. A child with only inherited context and no observed local work gets that
specific state, not an empty completed activation.

Related-object links name the target's containing agent. Inspecting an object in another
agent keeps the retained view and clears the previous agent's history selection instead
of applying that selection to the foreign object. Back restores the preceding inspection
scope and draft. This reading navigation does not change the main control target.

A compaction boundary offers `View context change`. Exact replacement membership is
listed only where recorded. Otherwise the summary stays readable beside `Exact included
items unavailable`. The ordinary conversation retains earlier work. Context preview is
not an executable reconstruction claim.

An object opened through search may be outside the current selection. Its result badge
states `Other history`; opening it pins the corresponding selection and offers `Return
to current history`. Metrics likewise label `Selected history`, `This activation`, or
the explicit recorded scope. Child totals, inherited usage, cumulative snapshots, and
reverted branches are not added to produce a more impressive total.

## Sending and required input

The composer binds to the main reader's current agent and its server-provided control
availability for the current Subject. Embedded readability is not proof of independent
control. An unavailable control displays the server reason and preserves reading and
draft editing where useful. Authorization is refreshed independently of cached trace
facts. No client-side capability cache substitutes for the authorization check.

Actions use the server's exact typed route and supported action/lease selection. A
Session control target must address the focused agent; an embedded control route must
retain its native child selector. The embedded reference's anchor Session is never a
fallback recipient for send, stop, or steer.

Submitting creates a local pending indication tied to the idempotent control request;
it clears the draft only after durable acceptance, without clearing newer edits. The
pending queue is distinct from delivered conversation content. Labels distinguish
`Queued`, `Accepted by receiver`, `Admitted as input`, `Included in model context`,
`Rejected`, and `Delivery unconfirmed` according to the actual independent evidence.
These observations are not forced into a universal linear ladder. A queue receipt alone
does not place a human message into a fabricated activation.

Only editable queue revisions offer edit/cancel with the displayed revision. Consumed
input is immutable. An ambiguous delivery does not offer an automatic resend that could
execute the same request twice; it offers status recovery and an explicit new submission
with the uncertainty visible. Stop/steer actions name the target agent and affected
execution, so an inspector cannot redirect the keyboard stop action to another agent.

An accepted command awaiting outbox export remains visible as `Accepted · history
updating` in the operational queue. Its fixed command/observation identity is the
correlation for the eventual trace entry. The UI does not create a speculative admitted
message or retry execution because projection/export is delayed.

Approvals and questions show the declared operation, options, and resolution state.
One client's resolution refreshes all observers. An expired/abandoned request remains
inspectable and loses its answer controls. Focus moves to a newly opened request only
through an explicit action; a background child question creates a discoverable badge and
polite notification instead of stealing the composer.

## Bounded query and browser state contract

This is a consumer contract for the endpoints in
[unified-trace-storage.md](unified-trace-storage.md); the final API schema owns exact
route spellings and configured maxima.

| Query | Required result | Bound |
| --- | --- | --- |
| Reader bootstrap | Agent header, selected history, current view token, one reading window, availability | Summary items and encoded bytes; no descendant body fan-out |
| Reading window | Forward/backward or around-object page, stable object refs, order, summaries, body revisions | Items and bytes, with a resumable cursor |
| Operations | One explicit owner/group or resource operation window | Items, bytes, relation depth |
| Agents | Authorized directory page, search/filter scope, counts and completeness | Rows, bytes, bounded server work |
| Relations/context/history | Selected relation page or checkpoint membership | Rows, bytes, depth; no recursive entire graph |
| Payload | Named immutable representation/revision and returned range | Bytes and decode/expansion work |
| Changes | Changes and removals through a consistent view revision, or restart | Items and bytes with continuity evidence |
| Search | Scoped hits, snippets, object anchors, history membership, indexed-through cut | Hits, snippet bytes, query work |

The reusable ViewLease pins projection generation, evidence cut, view revision, and
expiration. A scoped cursor adds agent, history selection, filters, representation, and
ordering. Related queries use `at_snapshot` to derive another scope at the same lease;
a history cursor is not reused as a relations cursor. Header, history, and inspected
details can therefore share one cut without requiring one unbounded bootstrap response.

A body request pins the immutable revision reachable through its readable `via_ref`
HistoryEntry/Delivery/Operation/Resource/MessageRevision anchor and selected history. This authorization anchor
is separate from original authorship: retained copied context can be read through its
authorized recipient without granting access to the origin environment. A valid anchor
cannot authorize its newer body under an old snapshot. A newer counter cannot invalidate a multi-megabyte body. Two windows using
different views are never merged as if they were one complete list. Authorization and
deletion epochs are evaluated on each read and cannot be frozen by an old view token.

A change set becomes visible only at its completed revision boundary. The client stages
bounded changes and then swaps the affected window coherently. If the change set exceeds
the negotiated bound, the server requires a fresh around-anchor window; the browser
does not accumulate an unbounded catch-up buffer. Removed/moved entries carry their old
membership so loaded windows can remove them. Unloaded changes are invalidations, not
permission to fetch every changed body or ancestor.

Bootstrap and subscription use the server's replayable change cursor to close the
subscribe/read race. Duplicate changes are idempotent. Transport reconnection resumes
the cursor when retained and refreshes around the anchor on an explicit restart.
Clean connection EOF is a transport event; it is not evidence of activation completion.
The client reports connectivity, evidence admission progress, projection progress, and
search indexing lag as different conditions where they affect the selected action.

Browser state is partitioned by server, authorization scope, agent reference, selected
history, generation, and view revision. Immutable payload bytes may be shared by content
identity only after an authorized reference resolves them; knowledge of a hash grants no
read access. Reauthentication clears protected caches and subscriptions. Hidden readers
release requests and subscriptions while retaining only bounded lightweight anchors,
drafts, and disclosure choices. LRU eviction must not discard the focused editor or an
unsent draft.

The application multiplexes visible trace interests through a bounded subscription
manager. A per-agent connection for every discovered descendant is prohibited. Expanding
an operation subscribes only to the necessary visible summary/body revisions. Body
updates are coalesced and range-based where the source supports appends; replacements
use a new immutable revision. No hidden resource downloads output on every update.

Search runs on the server over the named authorized scope. `Find in this agent` does
not search only loaded rows. Hits explain partial index coverage and can reach content
beyond current windows through an around-object read. Browser Find still works on the
mounted text; the product search is discoverable without hijacking browser-reserved
shortcuts. Large payload search returns excerpts and locations rather than transferring
the whole payload.

Search pins its `(generation, view_revision, evidence_cut)` checkpoint with retained
membership, snippets, and body revisions. Opening a hit uses that view or explicitly
offers current state; it does not silently combine an old snippet with the newest body.
If storage pressure invalidates the index/result job, the result list shows indexing
and reacquires its scope instead of retaining apparently working dead links.

## Loading, errors, and accessible navigation

| State | Behavior |
| --- | --- |
| First page pending | Stable header/row skeleton, no invented empty-history message |
| Settled empty local history | Explain whether only inherited context, no observed local content, or no matching filter |
| Page fetch failed | Preserve existing window, show retry at failed boundary |
| Environment disconnected | Read retained data; show source freshness and control limitation separately |
| Payload unavailable/truncated/redacted | Name the recorded availability; retain metadata and available ranges |
| Partial decode or unknown semantic kind | Render preserved generic content and a scoped diagnostic |
| Conflicting claims | Visible uncertainty with evidence inspection; no automatic winner in the browser |
| Projection rebuild failed | Keep prior generation readable, expose fault and progress through Details |
| Snapshot/cursor expired | Reload around stable anchor with draft/focus preservation |
| Access removed | Stop protected reads, clear relevant cache, show access state without leaking hidden metadata |
| Deleted content | Explain deletion where authorized, remove body/preview and keep navigation functional |
| Interpretation withdrawn | Explain changed interpretation, preserve draft/navigation, and offer evidenced replacement or source inspection |

The semantic document outline consists of workspace heading, reader heading, activation
headings when known, and content headings beneath them. Long prose uses actual headings,
lists, tables, and links. Operation lists use lists with separate buttons, not nested
interactive rows. Directories with filtering use the appropriate combobox/listbox
pattern only when they are a single selection widget; ordinary navigation remains links.

Tab follows visible controls. Enter/Space opens a disclosure; Arrow keys navigate only
inside widgets whose ARIA pattern defines them. Inspector tabs use manual activation so
arrowing across tabs does not start network reads. A resize separator supports keyboard
steps and announces its value. Escape dismisses the top overlay, then restores its
opener. Main and inspector readers have distinct accessible landmark names and a
keyboard action to move between them. Browser Back remains functional.

Virtualization offers an explicit paged reading mode with the same server windows for
assistive technology and predictable reading navigation. There is no automatic detection
of screen readers. Mounted items expose position information only when the total and
position are established. A focused item is retained until focus leaves it; paging or
eviction cannot remove its DOM node. A short live region announces meaningful outcomes,
required input, and new-activity availability, coalescing updates rather than reading
every token or cumulative usage snapshot.

WCAG 2.1 AA is the minimum. Normal text reaches 4.5:1, large text and UI boundaries
reach the applicable 3:1 requirements, and status always has text or shape in addition
to color. Forced colors preserve focus and selected states. Reduced motion removes
pulsing/animated transitions. Reflow, 200% text sizing, and 400% zoom are tested with
long code, multilingual prose, and a visible composer. VoiceOver and NVDA passes cover
the same primary reading and child-navigation tasks; axe alone is insufficient.

## Visual system and implementation boundaries

The visual hierarchy uses spacious prose and compact, quiet work summaries. Surfaces
separate navigation, reading, and detailed output; borders are reserved for boundaries
that aid scanning. Saturated status color is local to the fact it describes. A single
failed command does not tint the surrounding conversation. Active focus is clearly
different from execution activity.

Use the semantic color roles, system/CJK font stacks, theme behavior, and safe-area
rules owned by [ui-layout.md](ui-layout.md). The reader's default body size is 16 px
with approximately 1.6 line height; operation summaries use 14 px; code uses at least
13 px and remains zoomable. Labels essential to understanding authorship, time, or
coverage are never decoration-sized. Dense and comfortable preferences alter spacing
without changing content coverage or hiding uncertainty.

Markdown, tables, diffs, and structured results are progressive views over bounded
content. Parsing and measuring large bodies happen outside input-critical render work.
Images reserve known dimensions; unknown dimensions receive a bounded placeholder.
Complex previews and graph/timing analysis load on explicit demand. A complete agent
graph visualization is an optional analysis view, never a prerequisite for navigation.

Implement against the installed Next.js documentation before writing route or data-flow
code. Reuse established virtual-list, accessibility, parsing, and streaming primitives
when their behavior fits measured requirements. An old bundle-size decision does not
justify duplicating semantic inference or extending an unsafe bespoke parser. New
dependencies require a measured route/bundle and interaction assessment as described in
[frontend-architecture.md](frontend-architecture.md).

## Performance and acceptance

These are initial acceptance targets, not measurements of the current app. The harness
records configured API limits and browser cache limits with each run; tests assert
against those read-back values. A changed target requires evidence and an explicit spec
change, not a larger fixture timeout.

| Budget | Initial target and purpose |
| --- | --- |
| Automatic reader bootstrap | At most 256 KiB decoded JSON across trace reads before interaction; retained bodies are excluded from bootstrap |
| Mounted trace rows | At most 200 across simultaneously visible readers/lists, including overscan; focused rows count toward the bound |
| Decoded trace caches | At most 16 MiB metadata and 32 MiB payload cache per browser workspace; previews/media and user-started download buffers are separately bounded |
| Retained hidden view state | At most 10 recent agent locations; drafts persist independently of view LRU |
| Interaction responsiveness | p95 below 100 ms from local disclosure/navigation input to visible feedback, excluding network wait |
| Main-thread blocking | No task over 50 ms caused by processing an individual bounded trace update in the acceptance workload |
| Reading position | At most 2 CSS px anchor displacement after prepend, resize, late correction, or generation refresh once layout settles |
| Streaming fairness | A continuously producing visible agent cannot prevent another visible agent's updates or a control request from progressing |

The acceptance workload includes the pinned long histories described in the foundation,
sanitized variants with several thousand operations and many repeated child activations,
and scale fixtures at ten times those structural sizes. It also includes a 20 MiB
single output, multibyte text across ranges, repeated identical log lines, overlapping
resource polls, images, and rich Markdown. A tiny two-agent fixture cannot establish
directory or memory behavior. Cold and warm runs are reported separately on documented
desktop and mobile browser profiles, with controlled network/CPU throttling. Target
latency is measured from browser performance entries and interaction marks, not a
server-only stopwatch.

For a fixed visible window, increasing total retained history or sibling count by ten
must not increase automatic body requests, mounted rows, or resident cache bounds.
The storage workload also increases superseded temporal versions per object/order range
and reads an older snapshot. A bounded returned page is insufficient if producing it
scans every obsolete version. Query-budget exhaustion must provide an explicit recovery
or resumable-work response, never a false empty page or complete count.
The harness records returned bytes, request fan-out, active subscriptions, rows, heap
after repeated navigation, main-thread tasks, and anchor displacement independently.
Explicit full export is a separate streamed workload with cancellation and progress.
Details offers `Export environment evidence` with the environment and retained cut shown
before any bytes are requested. The scope includes other sessions and source inventories,
since shared raw captures cannot be split into a lossless single-agent transcript. The
browser validates the SHA-256 completion footer before committing a transactional file
or bounded buffered download. It freezes the displayed export scope during the job;
ordinary reader caches never contain the full archive. Interrupted or invalid streams
leave the output uncommitted. This evidence archive is separate from paced playback.

Details also offers `Play recorded observations`, naming the environment and retained cut.
Playback displays raw observation previews in admission order, without assigning vendor
roles or interpreting vendor payloads in the browser. Start is explicit; speed is selected
before starting. Pause/resume preserves the remaining gap, Stop cancels immediately, and
neither controls agent execution. The scope and pacing policy stay fixed during the run.
The UI labels receipt timing, every clamped or discontinuous gap, source/capture clocks,
preview truncation and its bounded visible window. Progress is announced without making
individual frames a live region or stealing focus. Buffered bodies and older rows never
enter ordinary reader caches. Failure or access loss clears provisional previews, and
verified completion is distinct from a stopped or expired run.

Required behavioral acceptance cases:

| Scenario | Observable promise |
| --- | --- |
| Parent, child, and grandchild reading | Same content rules; Back restores the original object and draft |
| Child receives follow-ups | One agent entry, multiple proven activations; no count from notification frequency |
| Human input interleaves with harness/agent input | Human consumed bytes and attribution remain intact |
| Completed parent with child/process still active | Parent outcome remains completed; ongoing work remains reachable |
| Start without close and lost observation | Outcome unknown is distinct from working, failed, and unavailable control |
| Runtime, caller, and included-model content differ | Each representation is accessible with correct labels and completeness |
| Yield, nested operations, poll, and later final result | Operation occurrences and resource lifetime stay distinct |
| Revert, inherited prefix, and compaction | Selected reading changes without copying ownership or erasing retained history |
| Late identity equivalence or correction during paging | Stable links resolve; coherent page windows have no omissions or duplicates |
| Generation replacement while selecting text | Reader receives update affordance; draft, focus, and anchor survive acceptance |
| Search hits an unloaded or inactive-history object | Correct around-object window and history label, without whole-trace download |
| Unknown result kind or malformed payload part | Other content remains usable; unavailable interpretation is visible |
| Offline, reconnect, replay gap, and access revocation | Correct cache treatment; no command retry from a trace reconnect |
| Long directory and repeated inspection | Whole-scope search, bounded rows, stable selection, subscriptions released |
| Keyboard and assistive reading | All primary tasks work without pointer or automatic focus theft |

Semantic fixtures first pass the foundation's reduction conformance. Browser tests then
assert rendered promises and interaction behavior, without reimplementing the adapter in
test expectations. New tests are proven by targeted faults: for example, substitute a
parent outcome for a child outcome, merge different view revisions, or pair operation
results by tool name and observe the intended failure. Run complete affected suites
after the final implementation edit under the repository's normal launcher.

## Delivery order

Implementation follows the foundation and storage contracts, then proceeds in vertical
slices: one bounded agent reader; attributed deliveries and outcome/coverage; operation
and resource inspection; agent navigation; context/history selection; and generation,
accessibility, and scale verification throughout. Each slice consumes the same shared
query protocol. No temporary vendor-aware frontend renderer is retained as a second
interpretation path. Progress and remaining implementation gates belong only in
[roadmap.md](roadmap.md).
