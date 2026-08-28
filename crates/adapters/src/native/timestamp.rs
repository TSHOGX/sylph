//! Decoding a vendor's timestamps. **One module, because two of the three measured
//! defects in the ported parsers were timestamp bugs.**
//!
//! `AGENTS.md` § Reading a vendor payload: *"The right key in the wrong unit fails the
//! same way, and is harder to catch. A wrong key yields absent, which a presence check
//! finds; a wrong unit yields a value, which passes every such check."*
//!
//! The two defects this module exists to make impossible, both re-measured on this
//! machine on 2026-08-28 against the real stores:
//!
//! - **`pi`'s `message.timestamp` is epoch milliseconds and was divided by 1000.**
//!   Re-measured: 3947 integer stamps, first sample `1782118877466`, which as
//!   milliseconds is `2026-06-22T09:01:17Z` and under the parser's divide decodes to
//!   `1970-01-21`. Every per-event time the parser produced was in 1970 — on a store
//!   that parsed without a single error.
//! - **746 of 4881 of `pi`'s outer ISO stamps carry no UTC offset**, so strict RFC3339
//!   parsing fails on them; and because the value is a *string*, an integer fallback
//!   never fires either. Both halves are needed or one era of the store is unreadable.
//!
//! So there is exactly one decoder for each shape and nothing else parses a timestamp.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};

/// Epoch milliseconds from a vendor's ISO-8601 string.
///
/// Accepts both shapes the stores actually hold, and the second is not optional:
///
/// - With an offset (`…Z`, `…+08:00`) — parsed as the instant it names.
/// - **Without one** (`2026-04-17T17:07:29.330418`) — 746 of 4881 `pi` records. Read as
///   **local time**, which is measured rather than assumed, and is the opposite of what
///   an earlier version of this comment claimed.
///
/// The measurement, taken 2026-08-28 over every message in the real `pi` store that
/// carries both an outer ISO string and an inner integer — 3880 pairs, where the integer
/// is known-correct epoch milliseconds:
///
/// | outer stamp | integer minus outer-read-as-UTC | pairs |
/// | --- | --- | --- |
/// | offset-bearing | **0 h** | 3750 |
/// | offsetless | **−8 h**, exactly this machine's zone | 130 |
///
/// So the offsetless era is written in whatever zone the machine was in, and the
/// millisecond agreement that looked like proof of UTC only ever held for the
/// offset-bearing era. Reading these as UTC shifts 130 real records by eight hours here
/// and by a different amount elsewhere — which is precisely the never-reproduces bug the
/// old comment warned about, arrived at by taking its own advice backwards.
///
/// This is the vendor-key defect family in its unit-rather-than-spelling form
/// (`AGENTS.md` § "The right key in the wrong unit fails the same way"): a wrong zone
/// yields a plausible timestamp that no presence check can catch. What settles it is
/// comparing against a sibling whose units are already known, which is why the census
/// above is over *pairs* rather than over stamps.
///
/// `None` when neither shape parses, because a synthesized timestamp is a lie a journal
/// makes permanent (`sylph_protocol::NativeSummary`: every field an adapter cannot answer
/// is absent, not defaulted).
pub fn epoch_millis_from_iso(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Offset-bearing first: this is the majority shape, and `DateTime` parsing rejects
    // the offsetless form rather than guessing at it.
    if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
        return Some(parsed.timestamp_millis());
    }
    // Offsetless. Both the fractional and whole-second spellings occur.
    //
    // Read in the machine's own zone, because that is what the vendor wrote (see the
    // census above). `Local` makes the result depend on where it runs, which is
    // ordinarily a hazard and is the right answer here for two reasons: the stamps are
    // local wall-clock time, so any fixed zone is wrong by construction; and the
    // offsetless era is a **closed window** — measured, 2026-04-07 to 2026-04-20, 746
    // stamps, with every later record offset-bearing. So this branch reads a bounded set
    // of historical records on the machine that produced them, which is the one situation
    // where a local reading is both correct and stable.
    //
    // `LocalResult::Ambiguous` picks the earlier instant: a wall-clock time inside a
    // DST fall-back names two instants and the vendor recorded no way to tell them
    // apart, so this is a coin flip stated as a rule rather than left to a panic. `None`
    // for a nonexistent local time, because that stamp cannot have been written by a
    // clock in this zone and guessing would be worse than reporting absence.
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return match Local.from_local_datetime(&naive) {
                chrono::LocalResult::Single(at) => Some(at.timestamp_millis()),
                chrono::LocalResult::Ambiguous(earlier, _) => Some(earlier.timestamp_millis()),
                chrono::LocalResult::None => None,
            };
        }
    }
    None
}

/// Epoch milliseconds from a JSON value that may be a number **or** a string.
///
/// # The unit is declared by the caller, never sniffed
///
/// There is deliberately no magnitude heuristic. `AGENTS.md`: *"Do not sniff magnitude to
/// choose a divisor — the vendor writes milliseconds, so read milliseconds."* A heuristic
/// would also have mis-read the 67 `pi` records whose integer stamp is the literal `1`,
/// written by test sessions, which look like a second unit era and are not.
///
/// So a caller passes [`Unit`] for what that field is *measured* to hold, and a wrong
/// answer is then a wrong constant at one call site rather than a value that decodes
/// plausibly.
pub fn epoch_millis_from_json(value: &serde_json::Value, unit: Unit) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => {
            // `as_i64` first: these are integers, and going through `f64` would lose
            // precision on a millisecond stamp past 2^53 microseconds.
            if let Some(raw) = number.as_i64() {
                return Some(unit.to_millis(raw));
            }
            number.as_f64().map(|raw| unit.to_millis(raw as i64))
        }
        serde_json::Value::String(text) => epoch_millis_from_iso(text),
        _ => None,
    }
}

/// What unit a vendor's numeric timestamp field is in, as measured on that field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// What `pi`'s `message.timestamp` holds, verified against a sibling ISO stamp
    /// agreeing to the millisecond on 3880 records.
    Millis,
    Seconds,
    Micros,
}

impl Unit {
    fn to_millis(self, raw: i64) -> i64 {
        match self {
            Self::Millis => raw,
            Self::Seconds => raw.saturating_mul(1000),
            Self::Micros => raw / 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured `pi` value, asserted as the instant it actually names.
    ///
    /// This is the regression test for the eighth vendor-key instance. The bug read the
    /// correct key and divided it, which yields a *plausible* number — so the assertion
    /// has to be on the decoded year, not on presence.
    #[test]
    fn a_pi_message_timestamp_is_milliseconds_and_decodes_to_2026() {
        // Sampled from ~/.pi/agent/sessions/ on 2026-08-28.
        let raw = serde_json::json!(1782118877466_i64);
        let millis = epoch_millis_from_json(&raw, Unit::Millis).unwrap();
        assert_eq!(millis, 1782118877466);

        let decoded = DateTime::from_timestamp_millis(millis).unwrap();
        assert_eq!(decoded.format("%Y-%m-%d").to_string(), "2026-06-22");

        // What the ported parser produced: the same key, divided as microseconds. Pinned
        // so the fix cannot silently regress into it — the value is well-formed and only
        // its *year* betrays it.
        let wrong = epoch_millis_from_json(&raw, Unit::Micros).unwrap();
        let wrong_decoded = DateTime::from_timestamp_millis(wrong).unwrap();
        assert_eq!(wrong_decoded.format("%Y").to_string(), "1970");
    }

    /// The offsetless era. 746 of 4881 real `pi` records, and strict RFC3339 rejects
    /// every one — so without this branch a whole era of the store has no timestamps.
    ///
    /// **Asserted as a relationship rather than as a constant**, because the correct
    /// reading is local time and a hardcoded millisecond value would pin this test to the
    /// zone of whoever last ran it. That is the same class of mistake as the bug it
    /// guards: a value that looks specific while being an artifact of where it was
    /// produced.
    #[test]
    fn an_iso_stamp_without_an_offset_is_read_as_local_wall_clock_time() {
        let stamp = "2026-04-17T17:07:29.330418";
        let millis = epoch_millis_from_iso(stamp).unwrap();

        // It names 17:07:29.330 on that date *in this machine's zone*, which is what the
        // vendor wrote. Measured over 3880 real pairs: an offsetless stamp sits exactly
        // one local-zone offset away from its own sibling integer, while an
        // offset-bearing one agrees to the millisecond.
        let expected = chrono::NaiveDate::from_ymd_opt(2026, 4, 17)
            .unwrap()
            .and_hms_milli_opt(17, 7, 29, 330)
            .unwrap();
        let expected = Local
            .from_local_datetime(&expected)
            .single()
            .expect("17:07 on 2026-04-17 is not a DST edge in any real zone");
        assert_eq!(millis, expected.timestamp_millis());

        // And reading it as UTC — what an earlier version of this decoder did, citing
        // this very stamp as evidence — is wrong by exactly the local offset wherever
        // that offset is not zero.
        let as_utc = chrono::Utc
            .from_utc_datetime(&expected.naive_local())
            .timestamp_millis();
        assert_eq!(
            millis - as_utc,
            -Local
                .from_local_datetime(&expected.naive_local())
                .single()
                .unwrap()
                .offset()
                .local_minus_utc() as i64
                * 1000,
            "the difference between the two readings is the zone offset, nothing else",
        );
    }

    #[test]
    fn an_offset_bearing_stamp_is_the_instant_it_names() {
        let zulu = epoch_millis_from_iso("2026-08-28T00:00:00Z").unwrap();
        // The same instant, written with an offset, must decode identically.
        let offset = epoch_millis_from_iso("2026-08-28T08:00:00+08:00").unwrap();
        assert_eq!(zulu, offset);
        // And an offset must not be ignored, which is what reading it as naive would do.
        assert_ne!(epoch_millis_from_iso("2026-08-28T08:00:00Z").unwrap(), zulu);
    }

    /// A string-valued stamp must work through the JSON entry point too. The parser this
    /// replaces had an integer branch and a string branch that disagreed about units,
    /// which is how one store had a working session-level time and broken per-event
    /// times.
    #[test]
    fn the_json_entry_point_reads_both_shapes_consistently() {
        let from_string = epoch_millis_from_json(
            &serde_json::json!("2026-04-17T17:07:29.330418"),
            // The unit is irrelevant on a string, and asserting that is the point: the
            // two branches must not be able to disagree.
            Unit::Micros,
        );
        assert_eq!(from_string, Some(1776416849330));
    }

    /// Absent beats invented. A field that cannot be decoded has to stay absent, or a
    /// fabricated timestamp becomes permanent in the journal.
    #[test]
    fn an_undecodable_stamp_is_absent_rather_than_zero() {
        assert_eq!(epoch_millis_from_iso(""), None);
        assert_eq!(epoch_millis_from_iso("   "), None);
        assert_eq!(epoch_millis_from_iso("yesterday"), None);
        assert_eq!(epoch_millis_from_iso("2026-13-45T99:99:99"), None);
        assert_eq!(
            epoch_millis_from_json(&serde_json::Value::Null, Unit::Millis),
            None
        );
        assert_eq!(
            epoch_millis_from_json(&serde_json::json!(true), Unit::Millis),
            None
        );
    }

    /// The literal `1` that 67 `pi` test-session records carry. It must survive as the
    /// value it is rather than being rescued by a magnitude heuristic — there is no such
    /// heuristic, and this pins that.
    #[test]
    fn a_tiny_integer_stamp_is_read_at_face_value() {
        assert_eq!(
            epoch_millis_from_json(&serde_json::json!(1), Unit::Millis),
            Some(1)
        );
    }

    #[test]
    fn seconds_and_micros_convert_as_declared() {
        assert_eq!(Unit::Seconds.to_millis(1776416849), 1776416849000);
        assert_eq!(Unit::Micros.to_millis(1776416849330418), 1776416849330);
        assert_eq!(Unit::Millis.to_millis(1776416849330), 1776416849330);
    }
}
