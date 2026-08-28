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
/// [`Fidelity::Blocks`] rather than [`Fidelity::Messages`]: the store flushes at item
/// completion, and a turn's items are finer than its messages — measured, 18 distinct
/// appends over a 26.5 s turn with the first at 0.1 s. So a renderer may time an item
/// and must not time a token.
pub const NATIVE_FIDELITY: Fidelity = Fidelity::Blocks;

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
/// the *store* and false of a *record*: 72 files hold `item_completed` and no
/// `event_msg/agent_message`, 1122 hold the reverse, and only **10 of 1258** hold both
/// — all of them cli_version 0.104–0.115, the oldest rather than the newest. Reading
/// both in a `paginated` file double-counts every assistant message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// `event_msg/agent_message` carries assistant text. 1122 files.
    Legacy,
    /// `event_msg/item_completed` carries it, keyed by `item.type`. 72 files.
    Paginated,
    /// Both are present. 10 files, all cli_version 0.104–0.115.
    ///
    /// A distinct variant rather than a coin flip, because the right behaviour is to
    /// prefer one and say which, not to discover the overlap per record.
    Overlapping,
}

impl Era {
    /// Read the era off a `session_meta` payload's `history_mode`.
    ///
    /// Absent on 1154 of 1258 files, which are all pre-`history_mode` and legacy — so
    /// absence is [`Era::Legacy`] rather than an error, and that is measured rather
    /// than assumed.
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
