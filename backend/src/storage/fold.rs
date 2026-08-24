//! Folding a block's deltas into offsets and timings.
//!
//! A delta is not independent data: every delta is a prefix-suffix of the same
//! final string, so storing each as a row costs about 20× the block it belongs
//! to. `docs/storage.md` works the arithmetic — a 2 KB assistant message arrives
//! as ~400 deltas, which is ~24 KB of rows against 3.2 KB folded.
//!
//! What is stored instead is the final text once, plus where each delta ended and
//! when it arrived. Reconstruction is exact rather than approximate: delta *n* is
//! `text[offsets[n-1]..offsets[n]]`, arriving at
//! `started_at_ms + cumsum(timings)[n]`.
//!
//! Offsets are **byte** offsets into UTF-8, because that is what a reconstructor
//! slices. A model emitting a multi-byte character in pieces puts a delta boundary
//! mid-codepoint, so [`unfold`] coalesces across such a boundary rather than
//! assuming every offset is a valid string edge — slicing there would panic.

/// A block's deltas, folded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    /// Cumulative byte offset at which each delta ended.
    pub offsets: Vec<u32>,
    /// Milliseconds between consecutive deltas. The first is measured from the
    /// block's start, so the arrival of delta *n* is a running sum.
    pub timings: Vec<u32>,
}

/// One delta as it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub text: String,
    pub at_ms: i64,
}

/// Fold a block's deltas against its final text.
///
/// Returns `None` when the deltas do not concatenate to `final_text`. That is not
/// expected — an adapter builds the completed event by accumulating its own
/// deltas, so they agree by construction — but an adapter that emits a tidied-up
/// final string would otherwise have its stream silently misreconstructed. Failing
/// the fold keeps the stable text and loses only delta-level replay for that block,
/// which is the safe direction to be wrong in.
pub fn fold(deltas: &[Delta], final_text: &str, block_started_ms: i64) -> Option<Folded> {
    if deltas.is_empty() {
        return None;
    }

    let mut offsets = Vec::with_capacity(deltas.len());
    let mut timings = Vec::with_capacity(deltas.len());
    let mut cursor = 0usize;
    let mut previous_ms = block_started_ms;

    for delta in deltas {
        cursor += delta.text.len();
        offsets.push(u32::try_from(cursor).ok()?);
        // Saturating rather than wrapping: a clock that steps backwards between
        // two deltas would otherwise encode as a ~4 billion millisecond gap and
        // stall a replay for weeks.
        let gap = delta.at_ms.saturating_sub(previous_ms).max(0);
        timings.push(u32::try_from(gap).unwrap_or(u32::MAX));
        previous_ms = delta.at_ms;
    }

    // The whole guarantee: what was streamed is what is stored.
    if cursor != final_text.len() {
        return None;
    }
    let rebuilt: String = deltas.iter().map(|d| d.text.as_str()).collect();
    if rebuilt != final_text {
        return None;
    }

    Some(Folded { offsets, timings })
}

/// Rebuild a block's deltas from its folded form.
///
/// Each returned delta is valid UTF-8. Where a stored boundary falls inside a
/// codepoint, the fragment is carried into the following delta and the two are
/// emitted as one — so a replay produces slightly fewer deltas than were stored,
/// at exactly the byte positions where emitting separately would have been
/// impossible.
pub fn unfold(text: &str, folded: &Folded, block_started_ms: i64) -> Vec<Delta> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(folded.offsets.len());
    let mut start = 0usize;
    let mut at_ms = block_started_ms;

    for (index, &end) in folded.offsets.iter().enumerate() {
        let end = (end as usize).min(bytes.len());
        at_ms += folded.timings.get(index).copied().unwrap_or(0) as i64;

        if end <= start {
            continue;
        }

        // Only emit at a real character boundary. Otherwise hold the fragment and
        // let the next offset complete it.
        if !text.is_char_boundary(end) {
            continue;
        }

        out.push(Delta {
            text: text[start..end].to_string(),
            at_ms,
        });
        start = end;
    }

    // A trailing fragment can only happen if the last offset was not a boundary,
    // which means the stored text and offsets disagree. Emitting the remainder is
    // better than dropping it.
    if start < bytes.len() {
        out.push(Delta {
            text: text[start..].to_string(),
            at_ms,
        });
    }

    out
}

/// Pack offsets or timings for storage: little-endian `u32`, no header.
///
/// No length prefix because the BLOB's own length gives it, and no varint because
/// these arrays are read whole and never scanned — 4 bytes flat is simpler than a
/// format that has to be decoded to be skipped.
pub fn pack(values: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(values.len() * 4);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// Unpack what [`pack`] wrote. A trailing partial value is ignored rather than
/// treated as zero, since a zero offset would corrupt every delta after it.
pub fn unpack(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deltas(parts: &[(&str, i64)]) -> Vec<Delta> {
        parts
            .iter()
            .map(|(text, at_ms)| Delta { text: text.to_string(), at_ms: *at_ms })
            .collect()
    }

    /// The property everything rests on: fold then unfold returns exactly what
    /// arrived, text and timing both.
    #[test]
    fn folding_and_unfolding_reproduces_every_delta_and_its_arrival() {
        let original = deltas(&[("I'll ", 1000), ("start ", 1034), ("by ", 1085), ("reading.", 1097)]);
        let text = "I'll start by reading.";

        let folded = fold(&original, text, 1000).expect("must fold");
        assert_eq!(folded.offsets, vec![5, 11, 14, 22]);
        assert_eq!(folded.timings, vec![0, 34, 51, 12]);

        assert_eq!(unfold(text, &folded, 1000), original);
    }

    /// The size claim from `docs/storage.md`, asserted rather than asserted-in-prose.
    #[test]
    fn folding_costs_eight_bytes_per_delta_rather_than_a_row() {
        let text = "x".repeat(2048);
        let original: Vec<Delta> = (0..400)
            .map(|i| Delta { text: "x".repeat(5), at_ms: 1000 + i * 10 })
            .collect();
        let text = &text[..2000];

        let folded = fold(&original, text, 1000).expect("must fold");
        let stored = pack(&folded.offsets).len() + pack(&folded.timings).len();
        assert_eq!(stored, 400 * 8);
        // Against ~60 bytes of row overhead per delta, before the text itself.
        assert!(stored < 400 * 60 / 2);
    }

    /// A multi-byte character split across two deltas is the case that makes naive
    /// slicing panic. The boundary is held and the pieces emit as one delta.
    #[test]
    fn a_codepoint_split_across_two_deltas_is_coalesced_rather_than_slicing_mid_character() {
        // "好" is three bytes; the first delta carries only its first byte.
        let text = "a好b";
        let folded = Folded {
            offsets: vec![1, 2, 4, 5],
            timings: vec![0, 10, 10, 10],
        };

        let replayed = unfold(text, &folded, 0);

        // Offsets 2 and 4 fall inside the character, so they cannot be emitted
        // separately. What matters is that the concatenation is intact.
        let rebuilt: String = replayed.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(rebuilt, text);
        for delta in &replayed {
            assert!(!delta.text.is_empty());
        }
    }

    #[test]
    fn multibyte_text_folds_and_unfolds_on_its_own_boundaries() {
        let original = deltas(&[("你好", 100), ("，世界", 150)]);
        let text = "你好，世界";
        let folded = fold(&original, text, 100).unwrap();
        assert_eq!(unfold(text, &folded, 100), original);
    }

    /// Deltas that disagree with the completed text must not fold. Reconstructing
    /// from a mismatched pair would hand a client text the agent never streamed.
    #[test]
    fn deltas_that_do_not_rebuild_the_final_text_refuse_to_fold() {
        let original = deltas(&[("hel", 0), ("lo", 10)]);
        assert!(fold(&original, "hello there", 0).is_none(), "short of the final text");
        assert!(fold(&original, "hi", 0).is_none(), "longer than the final text");
        // Same length, different bytes: the length check alone would pass this.
        assert!(fold(&original, "world", 0).is_none());
    }

    #[test]
    fn a_block_with_no_deltas_has_nothing_to_fold() {
        // A tool result arrives complete, with no streaming at all.
        assert!(fold(&[], "complete value", 0).is_none());
    }

    #[test]
    fn a_single_delta_carrying_the_whole_block_folds() {
        let original = deltas(&[("all at once", 500)]);
        let folded = fold(&original, "all at once", 500).unwrap();
        assert_eq!(folded.offsets, vec![11]);
        assert_eq!(folded.timings, vec![0]);
        assert_eq!(unfold("all at once", &folded, 500), original);
    }

    /// A clock stepping backwards must not encode as a four-billion-millisecond
    /// gap, which would stall a replay for weeks.
    #[test]
    fn a_backwards_clock_between_deltas_encodes_as_no_gap() {
        let original = deltas(&[("a", 1000), ("b", 900)]);
        let folded = fold(&original, "ab", 1000).unwrap();
        assert_eq!(folded.timings, vec![0, 0]);
    }

    #[test]
    fn packing_round_trips_and_ignores_a_truncated_tail() {
        let values = vec![0u32, 1, 4096, u32::MAX];
        assert_eq!(unpack(&pack(&values)), values);

        let mut truncated = pack(&values);
        truncated.push(0xAB);
        assert_eq!(
            unpack(&truncated),
            values,
            "a partial trailing value must not decode as a zero offset"
        );
        assert!(unpack(&[]).is_empty());
    }

    /// An empty delta is dropped on replay rather than emitted as a frame carrying
    /// nothing, which a client would render as a spurious update.
    #[test]
    fn a_zero_length_delta_is_not_replayed_as_an_empty_frame() {
        let text = "ab";
        let folded = Folded { offsets: vec![1, 1, 2], timings: vec![0, 5, 5] };
        let replayed = unfold(text, &folded, 0);
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].text, "a");
        assert_eq!(replayed[1].text, "b");
        // The dropped delta's time still accumulates, so later arrivals stay put.
        assert_eq!(replayed[1].at_ms, 10);
    }

    #[test]
    fn an_offset_past_the_text_is_clamped_rather_than_panicking() {
        // Defensive: a corrupted row must not take down a replay.
        let folded = Folded { offsets: vec![2, 99], timings: vec![0, 0] };
        let replayed = unfold("abc", &folded, 0);
        let rebuilt: String = replayed.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(rebuilt, "abc");
    }
}
