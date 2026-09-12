//! Turning an adapter's parse of a native store into journal rows.
//!
//! `adopt` and `watch` yield the **same** `SessionMessage` vocabulary a live turn
//! emits (`docs/session-protocol.md` § An adapter is a trait, with three modes), so this
//! module writes them through the same store, the same sequence allocator, and the same
//! batching writer the drain path uses. That is the whole point of the shape: the
//! journal, the history fold, the timeline, replay, and search cannot tell which mode
//! produced a given row, and the only visible difference is `fidelity` on the session.
//!
//! # What this is not
//!
//! It is **not** a second normalizer. Nothing here reads a vendor payload, knows a
//! vendor's spelling, or branches per agent — an adapter has already done all of that.
//! What is left is one question the adapter cannot answer, because it is a question
//! about Sylph's own schema rather than about the vendor: **which turn row does this
//! event belong to.**
//!
//! # A turn a vendor never bracketed must not be given a bracket it never had
//!
//! This is the constraint the whole module is shaped around, and it comes from the
//! stores themselves. `crates/adapters/src/codex/native.rs` pins it as
//! `an_interrupted_turn_is_not_given_a_bracket_it_never_had`: a real rollout holds
//! `turn_aborted` and **no** `task_started`, so the honest projection reports no `turn`
//! event at all. `pi` is the same shape from the other side — 44 real records close a
//! turn with no stated stop reason, so its parser emits no `turn`/`completed` for them.
//!
//! So an adopted stream may lack a turn open, a close, or both, in any combination, and
//! the resolution is a distinction that is easy to miss:
//!
//! - **A `turn` *event*** is a vendor fact. Nothing here synthesizes one. If the vendor
//!   recorded no bracket, no `turn` row appears in the transcript, and a reader sees
//!   exactly what the store held.
//! - **A `turns` *row*** is Sylph's own container — it is what `turn_id` on an event
//!   references and what `GET /sessions/{id}/turns` iterates. Every event needs one or
//!   it is unreachable from the transcript endpoint, which reads *per turn*. So a
//!   container is opened when content arrives with none open.
//!
//! Those are different objects and only the first is the vendor's to state. Conflating
//! them gives the choice between a synthesized stop reason and an invisible transcript,
//! and both are worse than an honestly-empty bracket around real content.
//!
//! # Where the turn boundaries come from
//!
//! One rule, applied in order, so a stream with brackets and a stream without both
//! land somewhere defensible:
//!
//! | Arriving | With a turn open | With none open |
//! | --- | --- | --- |
//! | `turn`/`started` | goes into it — the vendor's own bracket lands inside the container its content already opened | opens one |
//! | `turn`/`completed` | closes it, recording the vendor's own stop reason | opens one and closes it immediately, so the outcome is not orphaned |
//! | anything else | goes into it | opens one |
//!
//! The first row is the same call the bridge makes for a live
//! session, and for the same measured reason: a vendor's `turn`/`started` can arrive
//! *after* the payload it brackets, so treating it as the opener drops whatever preceded
//! it.
//!
//! # A turn's opener is decided by what follows it
//!
//! A re-read suppresses what a previous pass journaled by content
//! (`content_digest`), and there is exactly one event that cannot answer for itself
//! that way: a `turn`/`started`.
//!
//! **It survives in neither tier.** It is `Ephemeral`, so it reaches the journal and no
//! further; and `storage::history::consolidate` opens its `started`/`delta` arm with a
//! `stream_id` the event does not carry, so the fold never turns it into a block and
//! `delete_turn_events` then removes the row it was living in. Once its turn is
//! consolidated there is nothing left to compare a later pass against, so every pass
//! judges it new. Invisibly, too: every reader-facing query filters
//! `stability = 'stable'`, so the transcript is unaffected while an unchanged store
//! reports one event ingested and burns a sequence number per click.
//!
//! Neither of the two obvious answers works, and they fail in opposite directions:
//!
//! - **Widening the digest cannot reach it**, because the row is gone from both tiers
//!   rather than stored differently. Nothing is left to hash.
//! - **Treating an opener as a duplicate whenever no container is open** swallows a
//!   genuinely appended turn's bracket too. That loses no *content* — any event arriving
//!   with no container open opens one — so a turn count cannot see it. What it loses is
//!   the start: the container opens at the next event's stamp, so the turn silently
//!   begins late and any duration read from it measures the wrong interval.
//!
//! So the opener is **parked** rather than answered. It is written when a following event
//! proves new, and dropped when everything that follows is already held. The stream is
//! what decides, which is the one source that knows.
//!
//! **Making the tiers symmetric instead was rejected.** Having `consolidate` keep the
//! opener would remove the asymmetry rather than compensate for it, and it is the wrong
//! trade: `consolidate` is shared with live sessions, every history block is reported
//! `stable` by `api::sessions::block_to_event`, and history reads apply no stability
//! filter. A preserved opener would therefore become *visible* in the transcript the
//! moment a turn consolidated while staying invisible before it — breaking
//! `a_transcript_reads_the_same_before_and_after_consolidation` and changing what every
//! live session renders, to serve a re-read. Keeping it hidden there instead would mean
//! giving `blocks` a stability column and teaching every history read to filter it, which
//! pushes ephemerality into the tier whose whole contract is that it holds the record a
//! reader reads (`docs/storage.md` § Three tiers, because there are three workloads).
//!
//! # Every turn a pass opens is closed by the pass
//!
//! A `running` turn refuses the next message — the composer reads it and disables — so
//! an adopted session left with an open turn is a session nobody can talk to. `adopt` is
//! one pass over a file that is not being written, so there is nothing that could ever
//! close it later.
//!
//! The status is `completed` with **no** `error_code` and **no** stop reason recorded
//! anywhere a reader could mistake for the vendor's. `completed_at_ms` is the last
//! event's own timestamp rather than `now()`, because that is a real observation about
//! the transcript and `now()` is a fact about when someone clicked adopt.

use anyhow::{Context, Result};
use futures::StreamExt;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use sylph_protocol::{EventStream, SessionId, SessionMessage};

use crate::db::{JournalStore, JournalWriter};
use crate::events::{EventHub, SeqAllocator, StreamEvent};

/// What one pass over a native store produced.
///
/// Returned rather than logged because two of these fields are how a caller tells
/// "this session is empty" from "nothing in it could be read" — which look identical in
/// the journal and mean opposite things (`crate::native::JsonLines` draws the same
/// distinction one layer down).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// Events the adapter emitted, before any suppression.
    ///
    /// **Counted here rather than recomputed from `events + already_held`.** Those two
    /// account for every event the pass *decided* about, and a parked turn opener that
    /// nothing new followed is deliberately in neither: the pass declined to write it and
    /// cannot honestly claim a previous pass held it (see [`Self::already_held`]). Derived,
    /// `examined` would then under-report the store by however many brackets bracketed
    /// nothing — a number that reads as evidence about how much was looked at while being
    /// quietly short.
    ///
    /// Only events. A native record, a capability partial, and an unreadable item have
    /// their own counts.
    pub examined: usize,
    /// Events journaled.
    pub events: usize,
    /// Turn containers opened.
    pub turns: usize,
    /// Items the adapter reported as an error while reading.
    ///
    /// **Not fatal, and the stream continues past one.** An `Err` item is a failure
    /// reading the session rather than a failure *of* the session: a truncated native
    /// file, or `pi`'s `_rewriteFile()` truncate-rewrite observed mid-write
    /// (`sylph_protocol::EventStream`). Refusing the whole adopt over one would make a
    /// session unreadable for the duration of somebody else's write.
    pub read_errors: usize,
    /// Capability partials seen, which this path deliberately drops.
    ///
    /// A capability surface is **current state, not something that happened**
    /// (the bridge publishes it rather than journaling it), and an adopted
    /// session has no live link for the surface to describe. Recording a months-old
    /// permission mode as the session's current one would report a policy the session
    /// is not running under — which `AGENTS.md` records as its own class of defect,
    /// under a vendor's `success` not being a promise. Counted so the drop is visible
    /// rather than silent.
    pub capability_partials: usize,
    /// Complete native records retained for future re-projection.
    pub raw_records: usize,
    /// The best candidate for the opening ask this stream carried, if any.
    ///
    /// **This is how much of the opening ask survives an adapter, and it is not much.**
    /// The vocabulary has no `user` kind — the server owns a turn's `request_text` — so
    /// each parser decided separately what to do with the user's typed text, and they
    /// made two different calls:
    ///
    /// - `pi` maps a `user`-role text block to `status`, so the ask arrives here.
    /// - `claude` and `codex` drop it entirely, each with a comment saying the server
    ///   owns it.
    ///
    /// The server does not own it and cannot: `SessionMessage` has three variants and
    /// none carries user text, so for two of three agents the ask is not in the stream
    /// at all. `api::sessions::adopt` records what that costs a title and a transcript.
    ///
    /// # Which `status` event, and why the obvious filter was wrong
    ///
    /// Measured against a real adopted `pi` session: **`status`/`completed` with
    /// `is_error` false**. Filtering on a *phase-less* `status` — which reads as the
    /// natural "authored, not streamed" choice — selects `pi`'s own narration instead,
    /// because `change_event` emits model and thinking-level changes that way. The first
    /// real adopt produced the title *"Model set to openrouter stealth/ox-alpha"* for a
    /// session whose opening ask was `hi`.
    ///
    /// So the three `status` shapes `pi` emits are distinguished by what they carry, and
    /// only one of them is a person speaking:
    ///
    /// | Producer | Phase | `is_error` |
    /// | --- | --- | --- |
    /// | a `user`-role text block | `completed` | false |
    /// | `model_change` / `thinking_level_change` | *none* | false |
    /// | `extension_error` | `completed` | **true** |
    ///
    /// **This is an inference about one vendor's shape, not a fact any adapter states**,
    /// and it is the honest ceiling of what this field can be until an adapter carries
    /// the ask deliberately. It cannot mistake narration for an ask, and it can still
    /// pick up a `status`/`completed` a future adapter emits for something else — which
    /// would be a wrong title rather than a wrong transcript. The alternative is no
    /// fallback at all for the one agent where the text is present.
    pub opening_status: Option<String>,
    /// The last event timestamp seen, for closing a container honestly.
    pub last_event_ms: Option<i64>,
    /// Events this pass recognized as already journaled and did not write again.
    ///
    /// Zero on an `adopt` or `watch` pass, which suppress nothing: a first pass has
    /// nothing to have seen before, and a watcher's cursor only moves forward. It is
    /// non-zero only for a re-read ([`NativeIngest::reread`]), where it is the whole
    /// point — a pass that examined 412 events and added 37 is reporting that the other
    /// 375 were already held, which is what distinguishes it from a pass that failed.
    ///
    /// **The conclusion is reached two ways, and a dropped turn opener is the second.**
    /// Most events answer for themselves, by digest. A `turn`/`started` cannot — it
    /// survives in neither tier, so it is parked and judged by what follows it (see the
    /// module header). An opener dropped because a *held* event followed it was
    /// necessarily journaled by the pass that held that event, so it is counted here.
    ///
    /// **An opener dropped any other way is not.** One displaced by a second opener, or
    /// still parked at the end of the stream, bracketed no content at all — so this store
    /// may never have held it, and saying otherwise would report a suppression that never
    /// happened. Those are counted in [`Self::examined`] and nowhere else, which is why
    /// `examined` is counted rather than derived from this field.
    pub already_held: usize,
}

/// Content identity for one event, for recognizing a record a previous pass journaled.
///
/// # Why content and not a vendor id
///
/// There is no vendor id to use. `Adapter::adopt` restarts at the beginning of the store
/// and exposes no cursor, so a re-read re-emits every event the first pass saw; and of the
/// three agents only `claude` emits [`SessionMessage::NativeRecord`] at all, so the raw
/// tier's `(session, source, version)` key cannot answer for `codex` or `pi`, whose native
/// parsers emit projected events exclusively. Content identity is what is available for all
/// three (`docs/environments.md` § Re-reading a session Sylph already holds).
///
/// # What is hashed, and what is deliberately not
///
/// Every field that decides what a reader sees, and nothing that a second pass would
/// legitimately assign differently. `seq` is excluded because the server allocates it per
/// write, `arrival_at_ms` because it is this server's clock at ingest, and the turn id
/// because a container is opened by the pass. Including any of them would make every event
/// unique and suppress nothing — the failure mode that reads as "dedup is implemented" while
/// duplicating the transcript.
///
/// `recorded_at_ms` *is* included: it is the vendor's own stamp, so it is stable across
/// passes, and two vendor records that differ only in when they happened are two events.
///
/// # The collision this accepts
///
/// Two genuinely distinct events with identical content in one session hash the same, so the
/// second is suppressed. That loses one repeated line; the alternative duplicates the entire
/// transcript on every re-read. `docs/environments.md` records the trade and why the
/// transcript wins.
fn content_digest(event: &StreamEvent) -> String {
    // A JSON array rather than concatenated fields: `("a", "bc")` and `("ab", "c")` have
    // one concatenation and two array encodings, and a delimiter chosen for text is a
    // delimiter that appears in text.
    // `data` is normalized through the journal's own round trip rather than hashed as
    // given. `StreamEvent::to_row` writes `None` as the string `"{}"` and
    // `events::data_from_column` reads `"{}"` back as `None`, so `Some({})` in and `None`
    // out are the same stored row. Hashing the two differently would make an event carrying
    // an empty object unsuppressable — new on every pass, one copy per click, while
    // everything around it deduplicated correctly.
    let data = event
        .data
        .as_ref()
        .map(|data| data.to_string())
        .and_then(|column| crate::events::data_from_column(&column));
    let canonical = serde_json::json!([
        event.kind,
        event.phase,
        event.stream_id,
        event.span,
        event.interior_state.map(|state| state.to_string()),
        event.name,
        event.text,
        data,
        event.is_error,
        event.stability,
        event.recorded_at_ms,
    ]);
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

/// What journaling one event concluded.
///
/// Two outcomes rather than a `Result`, because the only failure that ends a pass is the
/// session row being gone — a sequence or write failure skips the one event and the pass
/// continues, which is the same answer the live drain gives.
enum Journaled {
    /// Written, or skipped with the failure logged. Keep reading the stream.
    Continued,
    /// The session row has been deleted under the pass. Nothing further can be attributed.
    SessionGone,
}

/// When an event happened, preferring the vendor's clock over the server's.
///
/// One function rather than the expression inline at each use, because the pass reads it
/// for three separate decisions — the report's last stamp, a container's start, and a
/// close — and a turn whose start and end came from different clocks is a duration
/// spanning two machines (`docs/session-protocol.md` § Time axes are independent).
fn event_time(event: &StreamEvent) -> i64 {
    event.recorded_at_ms.unwrap_or(event.arrival_at_ms)
}

/// Whether this event is a vendor's own opening bracket.
///
/// The narrow test is deliberate. The class of event consolidation cannot preserve is
/// wider — any `started` or `delta` phase with no `stream_id` is dropped at
/// `storage::history::consolidate`'s first arm — but `turn`/`started` is the only member a
/// native parse emits, and the only one the container rule treats as an opener. Widening
/// this to the whole class would park events that carry text, and dropping one of those
/// would lose content rather than a bracket.
fn opens_a_turn(event: &StreamEvent) -> bool {
    event.kind == "turn" && event.phase.as_deref() == Some("started")
}

/// Writes a parsed native session into the journal.
///
/// Holds the same three collaborators the drain path holds, and they are **passed in
/// rather than constructed**: a second [`SeqAllocator`] over one session would hand out
/// numbers a live link is also handing out, and `UNIQUE(session, seq)` would refuse one
/// of the two writes — intermittently, under concurrency, which is the worst way to
/// find out (`crate::events::seq`).
#[derive(Clone)]
pub struct NativeIngest {
    store: JournalStore,
    writer: JournalWriter,
    seq: SeqAllocator,
    /// Optional live projection for `watch`. Adopt is a finite operation and its
    /// caller can read the journal after the response; a watch must also wake an
    /// already-open session trace as records arrive.
    hub: Option<EventHub>,
    history: crate::storage::HistoryStore,
}

impl NativeIngest {
    pub fn new(
        store: JournalStore,
        writer: JournalWriter,
        seq: SeqAllocator,
        history: crate::storage::HistoryStore,
    ) -> Self {
        Self {
            store,
            writer,
            seq,
            hub: None,
            history,
        }
    }

    /// Attach the session trace projection used by the running server.
    ///
    /// Kept as a builder rather than changing [`Self::new`] so the ingest unit tests can
    /// exercise the storage path without constructing a hub, and so callers that only
    /// need a one-shot adopt do not pay for a live registry they will never use.
    pub fn with_hub(mut self, hub: EventHub) -> Self {
        self.hub = Some(hub);
        self
    }

    /// Drain a native session's stream into the journal.
    ///
    /// Runs to the end of the stream. For `adopt` that is the end of one pass; for
    /// `watch` it is however long the adapter follows the file, which is why a caller
    /// that wants a response before then spawns this rather than awaiting it.
    pub async fn drain(
        &self,
        session_id: &SessionId,
        stream: EventStream,
    ) -> Result<IngestReport> {
        self.drain_suppressing(session_id, stream, None).await
    }

    /// Parse the vendor's store again for a session already held, ingesting only what
    /// the journal lacks.
    ///
    /// One pass, on demand. It starts no follower and does not touch the session's
    /// `fidelity`, which belongs to the source that wrote the record
    /// (`docs/api/sessions.md` § `POST /api/v1/sessions/{id}/reread`).
    ///
    /// # The suppression set is read once, before the pass
    ///
    /// Both tiers are read, because a turn's events live in whichever one consolidation
    /// last left them — reading only the journal would treat every consolidated event as
    /// new and duplicate the whole of an older transcript, which is the failure this
    /// exists to prevent and the one a short-lived test session cannot observe.
    ///
    /// It is a snapshot rather than a set updated as the pass writes, and that is
    /// deliberate: within one pass the vendor's own repeats are its record, and
    /// re-checking against rows this pass just wrote would suppress them. What a previous
    /// pass held is the question being asked.
    pub async fn reread(
        &self,
        session_id: &SessionId,
        stream: EventStream,
    ) -> Result<IngestReport> {
        let held = self.held_digests(session_id).await?;
        let report = self
            .drain_suppressing(session_id, stream, Some(held))
            .await?;
        // A pass that added nothing is a success, and it needs a line: "ingested nothing"
        // and "could not read the store" are indistinguishable in a journal and are
        // opposite facts. `AGENTS.md` states the rule under Rules That Keep Costing Time — a
        // subsystem whose correct behaviour is "nothing happened" needs a report, or a
        // working pass cannot be told from a broken one.
        info!(
            "Re-read {}: examined {} event(s), ingested {}, already held {}, {} read error(s)",
            session_id,
            report.examined,
            report.events,
            report.already_held,
            report.read_errors
        );
        Ok(report)
    }

    /// Content digests of every event this session already holds, across both tiers.
    async fn held_digests(
        &self,
        session_id: &SessionId,
    ) -> Result<std::collections::HashSet<String>> {
        let mut held = std::collections::HashSet::new();
        // **Ephemeral rows included**, which `turn_events_after` excludes by design. A
        // `turn`/`started` derives `Ephemeral`, so reading through the reader-facing query
        // left every one of them out of this set — and a re-read then re-added one per pass,
        // in the tier a transcript is assembled from. `session_events` is the unfiltered
        // read, and it is one query for the session rather than one per turn.
        for row in self.store.session_events(session_id).await? {
            held.insert(content_digest(&StreamEvent::from_row(&row)));
        }
        // The other tier. A turn's events live in whichever one consolidation last left
        // them, so reading only the journal would treat every consolidated event as new and
        // duplicate the whole of an older transcript — the failure a short-lived test session
        // cannot observe, because nothing has been folded yet.
        //
        // Every turn, not a page of them: a bound here would silently stop suppressing on
        // the oldest turns of a long session, which is the history least recoverable
        // elsewhere.
        for turn in self.store.list_turns(session_id, i64::MAX).await? {
            if let Ok(blocks) = self.history.turn_blocks(&turn.id).await {
                for block in &blocks {
                    held.insert(content_digest(&crate::api::sessions::block_to_event(block)));
                }
            }
        }
        Ok(held)
    }

    /// The body of [`Self::drain`] and [`Self::reread`].
    ///
    /// `suppress` is `None` for a first pass, which has nothing to have seen before, and
    /// `Some` for a re-read. One function rather than two so the container, sequence, and
    /// settle rules have one derivation: a second copy of them would diverge the moment
    /// either grew a case, and the tiers would then disagree about the same transcript
    /// depending only on which entry point wrote it.
    async fn drain_suppressing(
        &self,
        session_id: &SessionId,
        mut stream: EventStream,
        suppress: Option<std::collections::HashSet<String>>,
    ) -> Result<IngestReport> {
        let mut report = IngestReport::default();
        // The container currently accepting events, with the timestamp of the event that
        // opened it. `None` between turns and before the first one.
        //
        // The start is carried rather than re-read, because it is the vendor's own stamp
        // and `close_adopted_turn` records both ends in one statement — a turn with an
        // end and no beginning is what a duration gets computed from.
        let mut open_turn: Option<(String, i64)> = None;
        // A `turn`/`started` seen during a suppressing pass, waiting for the stream to say
        // whether its turn is new. Always `None` on a first pass, which suppresses nothing
        // and so writes every opener where it arrives.
        let mut deferred_opener: Option<StreamEvent> = None;

        while let Some(item) = stream.next().await {
            let message = match item {
                Ok(message) => message,
                Err(error) => {
                    report.read_errors += 1;
                    // `debug` after the first, following the degraded-dependency rule: a
                    // file being rewritten under us produces a run of these, and one
                    // line per record is the 2339-line flood `AGENTS.md` records.
                    if report.read_errors == 1 {
                        warn!(
                            "Could not read part of {}'s native session: {:#}",
                            session_id, error
                        );
                    } else {
                        debug!(
                            "Further read error on {} (#{}): {:#}",
                            session_id, report.read_errors, error
                        );
                    }
                    continue;
                }
            };

            let event = match message {
                SessionMessage::Event(event) => event,
                SessionMessage::Capabilities(_) => {
                    report.capability_partials += 1;
                    continue;
                }
                // No native parser emits one, verified across all three; and a request
                // read back from a store is one somebody already answered, so raising it
                // would put a dialog on screen for a decision made months ago. Counted
                // as a read anomaly rather than silently skipped, so a parser that starts
                // emitting them is visible here rather than invisible.
                SessionMessage::Request(_) => {
                    report.read_errors += 1;
                    warn!(
                        "Ignoring a pending request read from {}'s native store: a request \
                         in a recorded transcript was already answered",
                        session_id
                    );
                    continue;
                }
                SessionMessage::NativeRecord(record) => {
                    if let Err(error) = self
                        .history
                        .put_native_record(&session_id.encode(), &record)
                        .await
                    {
                        if let Some((turn_id, opened_at)) = open_turn.take() {
                            let at = report.last_event_ms.unwrap_or(opened_at);
                            self.settle(&turn_id, true, opened_at, at).await;
                        }
                        return Err(error).with_context(|| {
                            format!(
                                "could not retain native record {}:{} for {}",
                                record.source, record.version, session_id
                            )
                        });
                    }
                    report.raw_records += 1;
                    // Raw native records do not open or close a turn container.
                    continue;
                }
                SessionMessage::TurnInput(_) => {
                    report.read_errors += 1;
                    warn!(
                        "Ignoring native turn-input fact from {} until adapter provenance is enabled",
                        session_id
                    );
                    continue;
                }
            };

            let wire = StreamEvent::from_protocol(&event);
            report.examined += 1;
            report.last_event_ms = Some(event_time(&wire));

            // The opening ask, for whatever the parser managed to carry. See
            // `IngestReport::opening_status` for why this is `completed` and not
            // phase-less — the phase-less shape is the vendor's own narration, and
            // filtering on it titled a real session "Model set to openrouter
            // stealth/ox-alpha" when its opening ask was "hi".
            if report.opening_status.is_none()
                && wire.kind == "status"
                && wire.phase.as_deref() == Some("completed")
                && !wire.is_error
            {
                let text = wire.text.trim();
                if !text.is_empty() {
                    report.opening_status = Some(text.to_string());
                }
            }

            // An event a previous pass already journaled. Counted and skipped **before a
            // container is opened or a sequence allocated**, so a re-read that finds
            // nothing new adds no empty turn and consumes no sequence numbers — an
            // otherwise-invisible cost that would accumulate one dead turn per click.
            //
            // A turn's opener is the one event no digest can answer for, so it is parked
            // here and decided by what follows it. See the module header, § A turn's
            // opener is decided by what follows it.
            if let Some(held) = &suppress {
                if held.contains(&content_digest(&wire)) {
                    report.already_held += 1;
                    // A held event belongs to a turn a previous pass already recorded, so
                    // a parked opener is that turn's bracket and was journaled by that
                    // same pass. This is the one drop site that *knows* the opener was
                    // held, which is why it is the only one that says so.
                    if deferred_opener.take().is_some() {
                        report.already_held += 1;
                    }
                    continue;
                }
                if opens_a_turn(&wire) {
                    // Parked, not written. Whether this is the opener of a turn already
                    // held or of one the vendor has just appended is not decidable from
                    // the event: both are byte-identical but for the vendor's stamp, and
                    // the held one survives in neither tier to compare against.
                    //
                    // A second opener arriving while one is parked displaces it, which is
                    // sound for the same reason: unheld content between them would have
                    // emitted the first already, so a displaced opener bracketed nothing.
                    // It is counted as examined and nothing else — claiming it was already
                    // held would assert a previous pass wrote a bracket around no content,
                    // which this pass has no evidence for.
                    deferred_opener = Some(wire);
                    continue;
                }
                // Not held, so this event is new work — which is the proof a parked opener
                // was waiting for. Journaled **first**, so the container opens at the
                // vendor's own start stamp rather than at this event's, which is the
                // silently-shortened turn a suppressed opener produces.
                if let Some(opener) = deferred_opener.take() {
                    match self
                        .journal(session_id, opener, &mut report, &mut open_turn)
                        .await
                    {
                        Journaled::Continued => {}
                        Journaled::SessionGone => return Ok(report),
                    }
                }
            }

            match self
                .journal(session_id, wire, &mut report, &mut open_turn)
                .await
            {
                Journaled::Continued => {}
                Journaled::SessionGone => return Ok(report),
            }
        }

        // An opener the rest of the stream never justified — the vendor has bracketed a
        // turn and written nothing into it yet. Dropped rather than written, because
        // writing it opens the dead empty turn per click that parking exists to avoid, and
        // it is not a loss: a `turn`/`started` is reader-facing in neither tier, and the
        // next pass to find real content after it opens a container at that content's own
        // stamp.
        //
        // Counted as examined and nothing else. It is not `already_held` — this store may
        // never have held it — and it is not ingested. A report that claimed either would
        // be asserting something this pass did not observe.
        drop(deferred_opener);

        // Whatever the pass opened and the vendor never closed. Not a bracket: no `turn`
        // event is written, and the row records no stop reason. See the module header.
        if let Some((turn_id, opened_at)) = open_turn {
            let at = report.last_event_ms.unwrap_or(opened_at);
            self.settle(&turn_id, false, opened_at, at).await;
        }

        Ok(report)
    }

    /// Journal one event: resolve its container, allocate its sequence, write it,
    /// publish it, and close the container if it was the vendor's own close.
    ///
    /// Extracted from [`Self::drain_suppressing`]'s loop because a parked opener is
    /// journaled from a second call site (§ A turn's opener is decided by what follows
    /// it), and the container, sequence, and settle rules must have one derivation — two
    /// copies would diverge the moment either grew a case, and the pass would then treat
    /// an opener differently from the event that justified it.
    async fn journal(
        &self,
        session_id: &SessionId,
        mut wire: StreamEvent,
        report: &mut IngestReport,
        open_turn: &mut Option<(String, i64)>,
    ) -> Journaled {
        let at = event_time(&wire);
        let closes = wire.kind == "turn" && wire.phase.as_deref() == Some("completed");

        let (turn_id, opened_at) = match &open_turn {
            Some((id, opened_at)) => (id.clone(), *opened_at),
            None => match self.open_container(session_id, at).await {
                Some(id) => {
                    report.turns += 1;
                    *open_turn = Some((id.clone(), at));
                    (id, at)
                }
                // The session row is gone — adopt racing a delete. Dropping is the
                // same answer the live drain gives, and for the same reason: an
                // unattributed write is worse than a missing one.
                None => return Journaled::SessionGone,
            },
        };
        wire.arrival_turn_id = Some(turn_id.clone());

        match self.seq.next(session_id).await {
            Ok(seq) => wire.seq = seq,
            Err(error) => {
                warn!(
                    "Could not allocate a sequence number for {}: {}",
                    session_id, error
                );
                return Journaled::Continued;
            }
        }

        // Loud on failure, and the wording is deliberate. A published-but-unjournaled
        // event is the defect `AGENTS.md` records as costing a turn that streamed
        // perfectly and came back empty on reload. Here nothing was published, so the
        // loss is quieter and still a loss: the transcript is missing a row the store
        // held.
        if let Err(error) = self
            .writer
            .append(&wire.to_row(session_id, &turn_id), wire.is_stable())
            .await
        {
            warn!(
                "Adopted event seq {} kind '{}' on {} was not journaled: {}",
                wire.seq, wire.kind, session_id, error
            );
            return Journaled::Continued;
        }
        report.events += 1;

        // A watched native file is a live source just like an adapter link. Publish
        // after the writer accepts the row so a reconnect can always find the same
        // event in the durable tier; the bounded trace projection covers the window
        // before consolidation.
        if let Some(hub) = &self.hub {
            hub.publish_trace(&session_id.encode(), wire.clone()).await;
        }

        if closes {
            // The vendor stated an outcome, so the container closes on the vendor's
            // own terms. `finish_turn_at` records the status; the stop reason is
            // already in the journaled `turn` event above, where a reader finds it.
            self.settle(&turn_id, wire.is_error, opened_at, at).await;
            *open_turn = None;
        }
        Journaled::Continued
    }

    /// Open a container for events the vendor did not bracket.
    ///
    /// `origin` is `user`, which is true of an adopted transcript — a person typed into a
    /// terminal — and it carries a known cost worth stating: `request_text` is empty
    /// because no adapter carries the user's text, and `docs/database-schema.md` names
    /// `origin: user` with an empty `request_text` as exactly the ambiguity `origin`
    /// exists to prevent, being indistinguishable from a user who sent nothing.
    ///
    /// `agent` is not the honest alternative. It means *the agent spoke unprompted*, and
    /// `settle_turn` branches on it to tell "no waiter because there never was one" from
    /// "no waiter because a stop already claimed it" — so marking every adopted turn
    /// `agent` would misreport who was speaking in order to work around a missing field.
    /// The ambiguity is reported rather than encoded around; `api::sessions::adopt`
    /// states it as a finding.
    async fn open_container(&self, session_id: &SessionId, at_ms: i64) -> Option<String> {
        let turn = crate::models::Turn {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.clone(),
            status: "running".to_string(),
            origin: "user".to_string(),
            // Empty because no adapter carries it. See this function's doc comment.
            request_text: String::new(),
            command_invocation_id: None,
            idempotency_key: None,
            error_code: None,
            error_message: None,
            // The vendor's own timestamp, not `now()`: a turn stamped now would order
            // an adopted session's turns after every live one and give it a duration of
            // zero.
            created_at_ms: at_ms,
            started_at_ms: Some(at_ms),
            completed_at_ms: None,
            stopped_at_ms: None,
        };

        match self.store.create_turn(&turn).await {
            Ok(created) => Some(created.id),
            Err(error) => {
                warn!(
                    "Could not open a turn for adopted session {}: {}",
                    session_id, error
                );
                None
            }
        }
    }

    /// Flush this turn's buffered rows, then record its terminal state.
    ///
    /// `failed` when the vendor's own close reported an error, `completed` otherwise.
    /// **No `error_code`**, in either case: the codes are the published enum a client
    /// branches on (`docs/api-v1.openapi.json`), and they describe what went wrong
    /// *now* — a historical failure the vendor recorded is in the journaled `turn` event
    /// with the vendor's own text, which is where a reader finds what happened.
    ///
    /// # The flush is ordered before the settle, and the order is the point
    ///
    /// `JournalWriter` holds ephemeral rows in a ~50 ms window, and
    /// `consolidatable_turns` selects on **settled** turns — so a turn that is settled
    /// while a window is still in memory is immediately eligible for a fold that cannot
    /// see those rows, and the fold silently produces a block whose replay is missing its
    /// tail (`crate::db::writer` § The ordering rule). The sweeper runs on a timer, so the
    /// race is real rather than theoretical: an adopted pass settles every turn it opens,
    /// which is a burst of newly-consolidatable turns.
    ///
    /// Flushing per turn rather than once at the end of the pass, because the window that
    /// matters is the one belonging to the turn being settled, and a long `watch` may
    /// settle many turns before the pass is anywhere near over.
    async fn settle(&self, turn_id: &str, failed: bool, started_at_ms: i64, at_ms: i64) {
        self.writer.flush_turn(turn_id).await;
        let status = if failed { "failed" } else { "completed" };
        if let Err(error) = self
            .store
            .close_adopted_turn(turn_id, status, started_at_ms, at_ms)
            .await
        {
            warn!("Could not close adopted turn {}: {}", turn_id, error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylph_protocol::{Event, EventKind, Fidelity, Phase};

    /// A store on disk, because migrations run on connect and `:memory:` would leave
    /// every query hitting a table that does not exist. The tempdir is returned so it
    /// outlives the pool — dropping it deletes the database out from under it.
    async fn harness() -> (NativeIngest, JournalStore, SessionId, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        let writer = JournalWriter::new(store.clone());
        let seq = SeqAllocator::new(store.clone());
        let history = crate::storage::HistoryStore::open(
            dir.path().join("history.db").to_str().unwrap(),
            crate::storage::cas::BlobStore::new(dir.path().join("blobs")),
        )
        .await
        .expect("history");
        let ingest = NativeIngest::new(store.clone(), writer, seq, history);

        let id = crate::fixtures::test_session_id("adopted-1");
        // `events` has a foreign key onto `sessions`, so the row has to exist before
        // anything can be attributed to it.
        store
            .create_session(&crate::models::Session {
                id: id.clone(),
                workdir: "/tmp".to_string(),
                title: None,
                inject_identity: false,
                fidelity: Fidelity::Blocks,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .expect("session row");

        (ingest, store, id, dir)
    }

    fn stream(events: Vec<Event>) -> EventStream {
        Box::pin(futures::stream::iter(
            events
                .into_iter()
                .map(|event| Ok(SessionMessage::Event(event))),
        ))
    }

    fn event(kind: EventKind, phase: Option<Phase>, at_ms: i64) -> Event {
        Event::new(kind, phase, at_ms)
    }

    /// **The property the whole module exists for.** A store that recorded no turn
    /// bracket must not gain one.
    ///
    /// `claude`'s measured shape rather than a hypothetical: its native parser emits
    /// **zero** `turn` events, verified over a real 13623-event session, so every adopted
    /// `claude` transcript arrives exactly like this. `codex` produces the same shape for
    /// an interrupted turn, which its own
    /// `an_interrupted_turn_is_not_given_a_bracket_it_never_had` pins one layer down.
    ///
    /// Two assertions pulling in opposite directions, which is the point: the content must
    /// be reachable, so a container exists; and no `turn` event may appear, because that is
    /// the vendor's to state and it did not.
    #[tokio::test]
    async fn a_stream_with_no_turn_bracket_gets_a_container_and_no_turn_event() {
        let (ingest, store, id, _dir) = harness().await;

        let mut assistant = event(EventKind::Assistant, Some(Phase::Completed), 1000);
        assistant.text = "answer".to_string();
        let report = ingest
            .drain(&id, stream(vec![assistant]))
            .await
            .expect("ingest");

        assert_eq!(report.events, 1);
        assert_eq!(report.turns, 1, "content must be reachable from a turn");

        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(turns.len(), 1);

        let events = store
            .turn_events_after(&turns[0].id, -1)
            .await
            .expect("events");
        assert!(
            !events.iter().any(|event| event.kind == "turn"),
            "the vendor recorded no bracket, so none may be synthesized; got {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_watched_native_stream_publishes_its_events_to_the_session_trace() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = JournalStore::new(dir.path().join("journal.db").to_str().unwrap())
            .await
            .expect("store");
        let hub = EventHub::new();
        let id = crate::fixtures::test_session_id("watched-1");
        store
            .create_session(&crate::models::Session {
                id: id.clone(),
                workdir: "/tmp".to_string(),
                title: None,
                inject_identity: false,
                fidelity: Fidelity::Blocks,
                flags_json: "{}".to_string(),
                status: "idle".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            })
            .await
            .expect("session row");
        let ingest = NativeIngest::new(
            store.clone(),
            JournalWriter::new(store.clone()),
            SeqAllocator::new(store),
            crate::storage::HistoryStore::open(
                dir.path().join("history.db").to_str().unwrap(),
                crate::storage::cas::BlobStore::new(dir.path().join("blobs")),
            )
            .await
            .expect("history"),
        )
        .with_hub(hub.clone());
        let mut trace = hub.attach_trace(&id.encode(), -1).await.live;

        let mut assistant = event(EventKind::Assistant, Some(Phase::Completed), 1000);
        assistant.text = "watched output".to_string();
        ingest
            .drain(&id, stream(vec![assistant]))
            .await
            .expect("ingest");

        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), trace.recv())
            .await
            .expect("trace did not receive the watched event")
            .expect("trace channel closed");
        assert!(observed
            .arrival_turn_id
            .as_deref()
            .is_some_and(|id| !id.is_empty()));
        assert_eq!(observed.text, "watched output");
        assert_eq!(observed.source, "agent");
    }

    /// A container the vendor never closed is still settled, and settled honestly.
    ///
    /// A `running` turn refuses the next message — the composer reads it and disables — so
    /// an adopted session left with one open is a session nobody can talk to, and `adopt`
    /// is one pass over a file nothing will write again. `completed` with **no**
    /// `error_code`, because nothing failed: the vendor simply never said how it ended.
    #[tokio::test]
    async fn an_unclosed_container_is_settled_without_claiming_an_outcome() {
        let (ingest, store, id, _dir) = harness().await;

        ingest
            .drain(
                &id,
                stream(vec![event(
                    EventKind::Assistant,
                    Some(Phase::Completed),
                    500,
                )]),
            )
            .await
            .expect("ingest");

        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(
            turns[0].status, "completed",
            "an open turn wedges the session: the composer refuses a message while one is live"
        );
        assert_eq!(turns[0].error_code, None, "nothing failed");
        assert_eq!(turns[0].error_message, None);
    }

    /// Both stamps come from the vendor's record, never from the clock.
    ///
    /// A turn stamped `now()` would report a months-old transcript as having happened when
    /// somebody clicked adopt — one timestamp for the whole session, a duration of zero,
    /// and a position at the top of every recency-ordered listing. A turn with an end and
    /// no beginning is the shape a duration gets computed from, so both are asserted.
    #[tokio::test]
    async fn a_turns_stamps_come_from_the_vendor_and_not_the_clock() {
        let (ingest, store, id, _dir) = harness().await;
        let opened = 1_600_000_000_000;
        let closed = 1_600_000_050_000;

        let mut close = event(EventKind::Turn, Some(Phase::Completed), closed);
        close.data = Some(serde_json::json!({"stopReason": "end_turn"}));
        ingest
            .drain(
                &id,
                stream(vec![
                    event(EventKind::Assistant, Some(Phase::Completed), opened),
                    close,
                ]),
            )
            .await
            .expect("ingest");

        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(turns[0].started_at_ms, Some(opened));
        assert_eq!(turns[0].completed_at_ms, Some(closed));

        // And the events kept theirs, so a waterfall over the result means something.
        let events = store
            .turn_events_after(&turns[0].id, -1)
            .await
            .expect("events");
        assert!(events.iter().all(|event| event.created_at_ms >= opened));
        assert!(events.iter().any(|event| event.created_at_ms == closed));
    }

    /// A vendor's own bracket segments the transcript, so a multi-turn store reads as
    /// multiple turns.
    ///
    /// `codex`'s measured shape: a real rollout adopted as 3 turns of 413, 927, and 3
    /// events. Without this the whole session would be one container, which is what
    /// `claude` currently gets for want of a `turn` event.
    #[tokio::test]
    async fn a_vendors_own_close_segments_the_transcript() {
        let (ingest, store, id, _dir) = harness().await;

        let report = ingest
            .drain(
                &id,
                stream(vec![
                    event(EventKind::Assistant, Some(Phase::Completed), 100),
                    event(EventKind::Turn, Some(Phase::Completed), 200),
                    event(EventKind::Assistant, Some(Phase::Completed), 300),
                    event(EventKind::Turn, Some(Phase::Completed), 400),
                ]),
            )
            .await
            .expect("ingest");

        assert_eq!(report.turns, 2);
        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|turn| turn.status == "completed"));
    }

    /// A vendor's `turn`/`started` arriving **after** content lands inside the container
    /// that content already opened, rather than opening a second one.
    ///
    /// The same rule the bridge follows for a live session, and
    /// for the same measured reason: a vendor's bracket can arrive after the payload it
    /// brackets, so treating it as the opener drops whatever preceded it. Here the cost
    /// would be an empty first turn beside a bracketed second one.
    #[tokio::test]
    async fn a_late_turn_start_does_not_open_a_second_container() {
        let (ingest, store, id, _dir) = harness().await;

        let report = ingest
            .drain(
                &id,
                stream(vec![
                    event(EventKind::Notice, None, 100),
                    event(EventKind::Turn, Some(Phase::Started), 150),
                    event(EventKind::Assistant, Some(Phase::Completed), 200),
                    event(EventKind::Turn, Some(Phase::Completed), 300),
                ]),
            )
            .await
            .expect("ingest");

        assert_eq!(report.turns, 1, "one container, not two");
        assert_eq!(store.list_turns(&id, 10).await.expect("turns").len(), 1);
    }

    /// A failed close is recorded as failed, because the vendor said so.
    ///
    /// The agent's own error flag decides this and not the transport: a turn that ran and
    /// reported failure is a completed exchange a user needs to see. The **event** carries
    /// the vendor's sentence; the row carries the status and no `error_code`, because the
    /// published codes describe what is wrong *now* rather than what a store recorded.
    #[tokio::test]
    async fn a_vendor_reported_failure_closes_the_container_as_failed() {
        let (ingest, store, id, _dir) = harness().await;

        let mut close = event(EventKind::Turn, Some(Phase::Completed), 900);
        close.is_error = true;
        close.text = "the provider answered 404".to_string();
        ingest
            .drain(&id, stream(vec![close]))
            .await
            .expect("ingest");

        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(turns[0].status, "failed");
        assert_eq!(turns[0].error_code, None);

        let events = store
            .turn_events_after(&turns[0].id, -1)
            .await
            .expect("events");
        let turn_event = events
            .iter()
            .find(|event| event.kind == "turn")
            .expect("one");
        assert!(turn_event.is_error);
        assert!(
            turn_event.text.contains("404"),
            "the vendor's own sentence is what says why"
        );
    }

    /// A read error does not end the pass, and the events after it still land.
    ///
    /// An `Err` item is a failure *reading* the session rather than a failure of it:
    /// `pi`'s `_rewriteFile()` truncate-rewrites with no lock, so a reader can observe a
    /// half-written file and the correct response is to keep reading. Refusing the whole
    /// adopt over one would make a session unreadable for the duration of somebody else's
    /// write.
    #[tokio::test]
    async fn a_read_error_is_counted_and_the_pass_continues() {
        let (ingest, _store, id, _dir) = harness().await;

        let items: Vec<anyhow::Result<SessionMessage>> = vec![
            Ok(SessionMessage::Event(event(
                EventKind::Assistant,
                Some(Phase::Completed),
                10,
            ))),
            Err(anyhow::anyhow!("truncated line")),
            Ok(SessionMessage::Event(event(
                EventKind::Assistant,
                Some(Phase::Completed),
                20,
            ))),
        ];
        let report = ingest
            .drain(&id, Box::pin(futures::stream::iter(items)))
            .await
            .expect("ingest");

        assert_eq!(report.read_errors, 1);
        assert_eq!(
            report.events, 2,
            "the event after the error must still land"
        );
    }

    /// Raw retention is ordered before projection.  If the warm tier refuses a source
    /// record, adopt stops at that record instead of journaling normalized events whose
    /// only source is missing.
    #[tokio::test]
    async fn a_raw_retention_failure_stops_adopt_before_later_projection() {
        let (ingest, store, id, _dir) = harness().await;
        let history = ingest.history.clone();
        let first = sylph_protocol::NativeRecord {
            source: "root.jsonl".to_string(),
            version: 1,
            raw_json: Some(br#"{"value":"first"}"#.to_vec()),
            payload: serde_json::json!({"value":"first"}),
            blobs: Vec::new(),
        };
        history
            .put_native_record(&id.encode(), &first)
            .await
            .expect("seed collision");
        let conflicting = sylph_protocol::NativeRecord {
            raw_json: Some(br#"{"value":"different"}"#.to_vec()),
            payload: serde_json::json!({"value":"different"}),
            ..first
        };
        let mut assistant = event(EventKind::Assistant, Some(Phase::Completed), 20);
        assistant.text = "must not be projected".to_string();
        let items: Vec<anyhow::Result<SessionMessage>> = vec![
            Ok(SessionMessage::NativeRecord(conflicting)),
            Ok(SessionMessage::Event(assistant)),
        ];

        let error = ingest
            .drain(&id, Box::pin(futures::stream::iter(items)))
            .await
            .expect_err("raw retention failure must fail the adopt pass");
        assert!(error.to_string().contains("root.jsonl:1"));
        assert!(
            store.list_turns(&id, 10).await.expect("turns").is_empty(),
            "a failed source record must not leave a plausible adopted turn behind"
        );
    }

    /// The opening ask is the first `status`/`completed`, not the first phase-less one.
    ///
    /// **A regression test for a defect this handler shipped and a real adopt caught.**
    /// Filtering on a phase-less `status` reads as the natural choice — authored rather
    /// than streamed — and it selects `pi`'s own narration, because `change_event` emits
    /// model and thinking-level changes that way. The first real adopt titled a session
    /// *"Model set to openrouter stealth/ox-alpha"* whose opening ask was `hi`.
    #[tokio::test]
    async fn the_opening_ask_is_the_users_status_and_not_the_vendors_narration() {
        let (ingest, _store, id, _dir) = harness().await;

        // Exactly the order a real pi session produces.
        let mut narration = event(EventKind::Status, None, 10);
        narration.text = "Model set to openrouter stealth/ox-alpha".to_string();
        let mut thinking_level = event(EventKind::Status, None, 11);
        thinking_level.text = "Thinking level set to off".to_string();
        let mut ask = event(EventKind::Status, Some(Phase::Completed), 20);
        ask.text = "hi".to_string();

        let report = ingest
            .drain(&id, stream(vec![narration, thinking_level, ask]))
            .await
            .expect("ingest");

        assert_eq!(report.opening_status.as_deref(), Some("hi"));
    }

    /// An `extension_error` shares the ask's shape and must not become the title.
    ///
    /// `pi` emits it as `status`/`completed` too, distinguished only by `is_error` — so
    /// this is the one remaining way the filter above could pick up something a person did
    /// not say.
    #[tokio::test]
    async fn an_error_status_is_not_mistaken_for_the_opening_ask() {
        let (ingest, _store, id, _dir) = harness().await;

        let mut failure = event(EventKind::Status, Some(Phase::Completed), 10);
        failure.text = "extension error".to_string();
        failure.is_error = true;
        let mut ask = event(EventKind::Status, Some(Phase::Completed), 20);
        ask.text = "fix the login bug".to_string();

        let report = ingest
            .drain(&id, stream(vec![failure, ask]))
            .await
            .expect("ingest");
        assert_eq!(report.opening_status.as_deref(), Some("fix the login bug"));
    }

    /// A capability partial is dropped rather than journaled, and the drop is counted.
    ///
    /// A capability surface is current state, not something that happened, and an adopted
    /// session has no live link for it to describe. Recording a months-old permission mode
    /// as current would report a policy the session is not running under.
    #[tokio::test]
    async fn a_capability_partial_is_dropped_and_counted() {
        let (ingest, store, id, _dir) = harness().await;

        let items: Vec<anyhow::Result<SessionMessage>> = vec![
            Ok(SessionMessage::Capabilities(Default::default())),
            Ok(SessionMessage::Event(event(
                EventKind::Assistant,
                Some(Phase::Completed),
                10,
            ))),
        ];
        let report = ingest
            .drain(&id, Box::pin(futures::stream::iter(items)))
            .await
            .expect("ingest");

        assert_eq!(report.capability_partials, 1);
        assert_eq!(report.events, 1);

        let turns = store.list_turns(&id, 10).await.expect("turns");
        let events = store
            .turn_events_after(&turns[0].id, -1)
            .await
            .expect("events");
        assert!(
            !events.iter().any(|event| event.kind == "capabilities"),
            "a stale surface must not be journaled as current state"
        );
    }

    /// An empty store yields no session-shaped debris: no turn, no events.
    ///
    /// A container opened for a stream that turns out to be empty would be a turn with
    /// nothing in it, which reads in the transcript as an exchange that happened and said
    /// nothing — the silence `AGENTS.md` names as the hardest outcome to debug.
    #[tokio::test]
    async fn an_empty_stream_opens_nothing() {
        let (ingest, store, id, _dir) = harness().await;
        let report = ingest.drain(&id, stream(vec![])).await.expect("ingest");

        assert_eq!(report, IngestReport::default());
        assert!(store.list_turns(&id, 10).await.expect("turns").is_empty());
    }

    /// A digest survives the journal round trip, so a re-read recognizes what it wrote.
    ///
    /// # The asymmetry this pins
    ///
    /// `data` has two derivations that disagree about one value. `StreamEvent::to_row`
    /// writes `None` as the string `"{}"`, and `data_from_column` reads `"{}"` back as
    /// `None` — so `Some(json!({}))` goes in and `None` comes out. Digesting the two
    /// differently would make such an event unsuppressable: every re-read would see it as
    /// new and add it again, one copy per click, while every other event deduplicated
    /// correctly and the mechanism looked sound.
    ///
    /// Asserted on the digest rather than on ingest counts, because this is a property of
    /// the identity function and a count would only show it after a fixture happened to
    /// produce the value. No current adapter emits `Some({})`; the round trip is what has to
    /// hold, not the set of shapes today's parsers reach.
    #[test]
    fn a_digest_is_the_same_before_and_after_the_journal_round_trip() {
        let session = crate::fixtures::test_session_id("digest-1");
        let cases = vec![
            ("absent data", None),
            ("an empty object", Some(serde_json::json!({}))),
            ("a populated object", Some(serde_json::json!({"a": 1}))),
        ];

        for (label, data) in cases {
            let mut event = Event::new(EventKind::Assistant, Some(Phase::Completed), 1000);
            event.text = "answer".to_string();
            event.data = data;
            event.recorded_at_ms = Some(1000);

            let incoming = StreamEvent::from_protocol(&event);
            let round_tripped = StreamEvent::from_row(&incoming.to_row(&session, "turn-1"));

            assert_eq!(
                content_digest(&incoming),
                content_digest(&round_tripped),
                "{label} digests differently after a journal round trip, so a re-read \
                 would re-add it on every pass"
            );
        }
    }

    /// A re-read over a **consolidated** turn adds nothing and opens no container.
    ///
    /// The condition the deferral exists for, reproduced at the layer it lives rather than
    /// through the API: the turn is folded first, which destroys the `turn`/`started` row
    /// outright — `consolidate` drops it for want of a `stream_id` and
    /// `delete_turn_events` removes what it was living in. So the opener the second pass
    /// sees is in neither tier and no digest can recognize it.
    ///
    /// `turns` is the assertion that matters. An opener written here would open a second
    /// container holding nothing, one per click, which is invisible to a transcript
    /// because every reader-facing query filters `stability = 'stable'`.
    #[tokio::test]
    async fn an_opener_whose_turn_was_consolidated_is_not_ingested_again() {
        let (ingest, store, id, _dir) = harness().await;

        let stored = || {
            let mut assistant = event(EventKind::Assistant, Some(Phase::Completed), 1001);
            assistant.stream_id = Some("stored-a1".to_string());
            assistant.text = "read from the store".to_string();
            vec![
                event(EventKind::Turn, Some(Phase::Started), 1000),
                assistant,
                event(EventKind::Turn, Some(Phase::Completed), 1002),
            ]
        };

        let first = ingest.drain(&id, stream(stored())).await.expect("adopt");
        assert_eq!(first.events, 3, "the whole bracket lands on a first pass");
        assert_eq!(first.turns, 1);

        // Fold it, which is what settle does on the real path and what removes the opener
        // from both tiers.
        let turns = store.list_turns(&id, 10).await.expect("turns");
        crate::storage::history::consolidate_turn(&store, &ingest.history, &turns[0].id)
            .await
            .expect("consolidate");
        assert!(
            !store
                .session_events(&id)
                .await
                .expect("journal")
                .iter()
                .any(|event| event.kind == "turn" && event.phase.as_deref() == Some("started")),
            "the opener must be gone from the journal for this test to be about anything"
        );

        let second = ingest.reread(&id, stream(stored())).await.expect("reread");

        assert_eq!(second.events, 0, "an unchanged store adds nothing");
        assert_eq!(
            second.turns, 0,
            "a re-ingested opener opens a dead container per click"
        );
        assert_eq!(second.examined, 3, "all three were looked at");
        assert_eq!(
            store.list_turns(&id, 10).await.expect("turns").len(),
            1,
            "the session still holds exactly the turn the adopt pass opened"
        );
    }

    /// A turn the vendor appended keeps its own opener, and so its own start.
    ///
    /// The opposite mistake from the test above, and the one a turn count cannot catch:
    /// suppressing an opener loses no content, because any event with no container open
    /// opens one — the container simply starts at the *reply's* stamp instead of the
    /// vendor's bracket. Asserted on the start stamp for that reason.
    #[tokio::test]
    async fn an_appended_turns_opener_survives_a_reread() {
        let (ingest, store, id, _dir) = harness().await;

        let mut first_reply = event(EventKind::Assistant, Some(Phase::Completed), 1001);
        first_reply.stream_id = Some("stored-a1".to_string());
        first_reply.text = "read from the store".to_string();
        let held = vec![
            event(EventKind::Turn, Some(Phase::Started), 1000),
            first_reply.clone(),
            event(EventKind::Turn, Some(Phase::Completed), 1002),
        ];
        ingest.drain(&id, stream(held.clone())).await.expect("adopt");
        let turns = store.list_turns(&id, 10).await.expect("turns");
        crate::storage::history::consolidate_turn(&store, &ingest.history, &turns[0].id)
            .await
            .expect("consolidate");

        // The same store, plus a whole new turn the vendor bracketed after the first.
        let mut appended_reply = event(EventKind::Assistant, Some(Phase::Completed), 2001);
        appended_reply.stream_id = Some("stored-a2".to_string());
        appended_reply.text = "appended afterwards".to_string();
        let mut grown = held;
        grown.push(event(EventKind::Turn, Some(Phase::Started), 2000));
        grown.push(appended_reply);
        grown.push(event(EventKind::Turn, Some(Phase::Completed), 2002));

        let report = ingest.reread(&id, stream(grown)).await.expect("reread");
        assert_eq!(report.events, 3, "the appended turn is new work");
        assert_eq!(report.turns, 1, "and it is its own container");

        // `list_turns` orders newest-first, so the appended turn is the head.
        let turns = store.list_turns(&id, 10).await.expect("turns");
        assert_eq!(turns.len(), 2, "the adopted turn and the appended one");
        assert_eq!(
            turns[0].started_at_ms,
            Some(2000),
            "the opener was suppressed, so the turn starts at its reply instead"
        );
        assert_eq!(turns[0].completed_at_ms, Some(2002));
    }

    /// An opener nothing follows opens no turn, and is not reported as suppressed.
    ///
    /// The vendor has bracketed a turn and written nothing into it yet. Writing the
    /// bracket would leave a container with no content — the debris
    /// `an_empty_stream_opens_nothing` rejects one case earlier — and counting it as
    /// already held would claim a suppression that never happened, since this store may
    /// never have held it. It is examined and nothing else.
    #[tokio::test]
    async fn an_opener_nothing_follows_opens_no_turn() {
        let (ingest, store, id, _dir) = harness().await;

        let report = ingest
            .reread(
                &id,
                stream(vec![event(EventKind::Turn, Some(Phase::Started), 1000)]),
            )
            .await
            .expect("reread");

        assert_eq!(report.events, 0);
        assert_eq!(report.turns, 0, "a bracket around nothing opens nothing");
        assert_eq!(report.examined, 1);
        assert_eq!(
            report.already_held, 0,
            "this store never held it, so nothing was suppressed"
        );
        assert!(store.list_turns(&id, 10).await.expect("turns").is_empty());
    }

    /// Sequence numbers are dense and monotonic across a pass, because they back
    /// `UNIQUE(session, seq)` and order the session's whole stream.
    #[tokio::test]
    async fn sequence_numbers_are_dense_across_a_pass() {
        let (ingest, store, id, _dir) = harness().await;

        let events: Vec<Event> = (0..6)
            .map(|n| event(EventKind::Assistant, Some(Phase::Completed), 100 + n))
            .collect();
        ingest.drain(&id, stream(events)).await.expect("ingest");

        let turns = store.list_turns(&id, 10).await.expect("turns");
        let mut seqs: Vec<i32> = store
            .turn_events_after(&turns[0].id, -1)
            .await
            .expect("events")
            .iter()
            .map(|event| event.seq)
            .collect();
        seqs.sort();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);
    }
}
