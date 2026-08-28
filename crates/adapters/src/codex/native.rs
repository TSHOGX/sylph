//! The `codex` native store: `rollout-*.jsonl` under `~/.codex/sessions`.
//!
//! # Status
//!
//! Skeleton. [`adopt`], [`watch`], and [`enumerate`] are what `codex/mod.rs` calls, and
//! their bodies are the parser. What is already settled — and is the expensive part —
//! is *what the store actually contains*, measured over all 1258 rollouts on this
//! machine rather than read off `docs/agents.md`, which was written from a second-hand
//! description and is wrong in several places. Each measurement below is stated where
//! the code that acts on it will go, because each one is a branch a reader would
//! otherwise write from the document and each one fails silently.

use anyhow::{anyhow, Result};
use sylph_protocol::{EventStream, Fidelity, NativeId, NativeSummary, StoreFilter};

/// The fidelity a parsed `codex` session yields.
///
/// [`Fidelity::Blocks`] rather than [`Fidelity::Messages`], and it holds for **both**
/// eras though for different reasons, which is worth stating because `fidelity` is a
/// property of the *record* rather than of the agent (`docs/architecture.md`), so a
/// per-agent constant has to be true of every file the agent writes.
///
/// - A `paginated` file carries per-item timestamps, so an item is separately timeable.
/// - A `legacy` file carries only each record's outer timestamp — but those are distinct
///   per content record, measured within one turn at `12:15:08.073Z`, `12:15:08.951Z`,
///   `12:15:26.110Z`. So the granularity is still the item, not the message.
///
/// What is *not* recoverable in either era is anything within an item, which is what
/// separates this from [`Fidelity::Stream`]. A renderer may time an item and must not
/// time a token.
pub const NATIVE_FIDELITY: Fidelity = Fidelity::Blocks;

/// **Reasoning text is mostly unrecoverable from a rollout, and a parser must expect
/// that rather than emit empty thinking events.**
///
/// Measured over a 400-file sample: every one of 12045 `response_item/reasoning` records
/// carries `encrypted_content`, and only **967** also carry a plaintext `summary`. The
/// `item_completed` side is emptier still — `Reasoning.summary_text` and
/// `Reasoning.raw_content` are present as fields and empty in practice.
///
/// So for roughly 92% of reasoning records the content exists only as vendor ciphertext.
/// The honest projection is to emit the summary where there is one and **omit the event
/// entirely** where there is not: an empty `thinking` event renders as a blank block,
/// which reads as a rendering bug rather than as an absent record, and a journal makes
/// that permanent (`docs/session-protocol.md` § "`name` is absent when unknown" states
/// the same rule for a different field).
pub const REASONING_IS_MOSTLY_ENCRYPTED: &str =
    "reasoning content is ciphertext on ~92% of records; emit the summary or nothing";

/// Which content vocabulary one rollout file speaks.
///
/// **The discriminant is `session_meta.payload.history_mode`, not `cli_version`.**
/// `docs/agents.md` says the vocabulary changed "around cli_version 0.147"; measured
/// over 1258 rollouts, 0.147.0 appears as `paginated` on 42 files and as `legacy` on
/// 21, and there is a 0.150.1 file — the newest version in the store — writing
/// `history_mode: "legacy"` with zero `item_completed`. A version comparison therefore
/// picks the wrong parser on a file the vendor is still producing today.
///
/// **The two content streams are mutually exclusive per file**, which is the half of
/// this that a document-driven reader gets backwards. "Both must be read" is true of
/// the *store* and false of a *record*: a paginated file carries `item_completed` and no
/// `event_msg/agent_message`, and reading both would double-count every assistant
/// message.
///
/// # Why there is no `Overlapping` variant
///
/// A small number of files do carry both streams, and an earlier version of this enum
/// declared a third variant for them. It was unreachable: measured, **every one of those
/// files has no `history_mode` at all** — they predate the field, being the oldest
/// versions in the store — so they arrive here as [`Self::Legacy`] and the variant could
/// never be constructed. That is the declared-only-variant trap this very module
/// documents about `SubAgentActivity.kind`, reproduced one file away from the correction.
///
/// Deleting it is also the right answer rather than merely the tidy one, because
/// `Legacy` is *correct* for those files: `event_msg/agent_message` carries their content
/// and `item_completed` is an addition beside it, so a legacy reading loses nothing. The
/// shape to rely on is that `history_mode == "paginated"` is what makes `item_completed`
/// the content carrier — not a count, which drifts as the store grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `event_msg/agent_message` carries assistant text. The large majority, and the
    /// right reading for every file without a `history_mode`.
    Legacy,
    /// `event_msg/item_completed` carries it, keyed by `item.type`.
    Paginated,
}

impl Era {
    /// Read the era off a `session_meta` payload's `history_mode`.
    ///
    /// Absent on most files, which are all pre-`history_mode` and legacy — so absence is
    /// [`Era::Legacy`] rather than an error, and that is measured rather than assumed.
    pub fn of(history_mode: Option<&str>) -> Self {
        match history_mode {
            Some("paginated") => Self::Paginated,
            _ => Self::Legacy,
        }
    }
}

/// **From 0.147 the `spawn_agent` brief is ciphertext**, so a delegation's name is not
/// recoverable from the parent's own record.
///
/// `docs/agents.md` does not mention this at all, and it is the most consequential fact
/// for a delegation card: `docs/session-protocol.md` § "`name` is absent when unknown"
/// requires the name to be **omitted** rather than guessed, because a journal makes
/// every guess permanent and a row written with a placeholder is afterwards
/// indistinguishable from one that carried a real name.
pub const BRIEF_IS_CIPHERTEXT: &str =
    "the spawn_agent brief is encrypted from 0.147; a delegation's name must be absent";

/// Which way a `SubAgentActivity` record's edge points.
///
/// **`agent_thread_id` is the *other party*, not the child.** In a parent's rollout it
/// names the child; in the child's own rollout a `kind: "interacted"` record names the
/// **parent** (`agent_path: "/root"`). So mapping it to `span` unconditionally inverts
/// the edge for every child-side record — which produces a tree that is not merely
/// incomplete but backwards, and nothing downstream can detect that.
///
/// **`kind` is `started` or `interacted` only.** `docs/agents.md` lists `interrupted`
/// and `completed`; both occur **zero** times in 1258 rollouts (measured: `started` 9,
/// `interacted` 33). That is the declared-only-event trap in its usual form, and the
/// consequence is concrete: **a codex delegation has no close record in the store**, so
/// a parsed delegation's end has to come from the turn that contains it rather than
/// from a record of its own.
///
/// `CollabAgentToolCall` is not lineage either, though the document presents it as
/// such: both real instances carry `receiver_thread_ids: []` and `agents_states: {}`
/// with `tool: "wait"`, and its `status: "completed"` is the *wait's* — the paired
/// output reads `{"message":"Wait timed out.","timed_out":true}`. It marks a barrier,
/// not a delegation.
pub fn delegation_edge(_activity: &serde_json::Value, _own_thread: &str) -> Option<String> {
    None
}

/// Parse everything the store already holds for one session, once.
pub fn adopt(_id: &NativeId) -> Result<EventStream> {
    Err(anyhow!(
        "codex `adopt` is not wired yet: the rollout parser is the remaining half of \
         step 2 in docs/roadmap.md § Implementation order"
    ))
}

/// Parse, then follow the rollout as it grows.
pub fn watch(_id: &NativeId) -> Result<EventStream> {
    Err(anyhow!(
        "codex `watch` is not wired yet: it is `adopt` plus a tail, and lands with the \
         parser"
    ))
}

/// Which native sessions this machine's store holds.
///
/// **`thread_history_*.sqlite` is a projection of the rollouts, not a replacement.**
/// `thread_history_projection_state` tracks a per-thread `next_rollout_byte_offset` and
/// `thread_turns` points back into the file, and the census settles it: 1258 rollouts
/// against 73 projected threads, so a reader that consulted only SQLite would miss 94%
/// of the sessions on this machine. The JSONL is authoritative.
///
/// The projection is still worth preferring *where a thread is present*, because it is
/// the only place carrying turn-level `status`, `duration_ms`, `started_at`, and
/// `completed_at` — facts a rollout scan would have to reconstruct by pairing
/// `task_started` with `task_complete` and doing arithmetic. So this reads the
/// projection first and falls back to walking files for every thread it does not hold.
pub fn enumerate(_filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
    Err(anyhow!(
        "codex `enumerate` is not wired yet: it reads the rollout store and the \
         thread-history projection, and lands with the parser"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The era discriminant, and why it is not the version.
    #[test]
    fn an_absent_history_mode_is_legacy_rather_than_an_error() {
        // 1154 of 1258 files carry no `history_mode` at all.
        assert_eq!(Era::of(None), Era::Legacy);
        assert_eq!(Era::of(Some("legacy")), Era::Legacy);
        assert_eq!(Era::of(Some("paginated")), Era::Paginated);
    }

    /// An unrecognized value reads as legacy rather than panicking.
    ///
    /// Deliberate: the vendor has added a mode before and the older vocabulary is the
    /// one every pre-`history_mode` file speaks, so an unknown mode degrades to the
    /// reading that is right for the majority rather than refusing the file.
    #[test]
    fn an_unknown_history_mode_degrades_to_legacy() {
        assert_eq!(Era::of(Some("something-new")), Era::Legacy);
    }
}
