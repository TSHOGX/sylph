pub mod control_request;
pub mod event;
pub mod request;
pub mod session;
pub mod turn;

pub use control_request::*;
pub use event::*;
pub use request::*;
pub use session::*;
pub use turn::*;

/// Epoch milliseconds now, as every row in this module stores time.
///
/// `chrono` rather than `SystemTime`, matching what the store binds. The crate has it
/// already, and it is the only clock the journal's `*_at_ms` columns ever see.
///
/// One copy. It was written out three times in this crate and inlined at seven more sites,
/// which is ten places to change if the clock ever has to be injectable — and a test that
/// wants to control time cannot, today, for exactly that reason.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Serialize a [`sylph_protocol::SessionId`] as its encoded string, and read it back.
///
/// Used by every child row that names its session — [`Turn`], [`Event`],
/// [`PendingRequest`]. One module rather than three `#[serde(with)]` bodies, because
/// the encoding has exactly one producer and one reader by design
/// (`sylph_protocol::SessionId::encode`), and three copies of the call is three
/// chances for one to drift into hand-assembling the string.
pub mod session_id_string {
    use serde::{Deserialize, Deserializer, Serializer};
    use sylph_protocol::SessionId;

    pub fn serialize<S: Serializer>(id: &SessionId, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&id.encode())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SessionId, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        SessionId::decode(&encoded).map_err(serde::de::Error::custom)
    }
}
