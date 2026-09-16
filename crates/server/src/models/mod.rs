pub mod session;

pub use session::*;

/// Epoch milliseconds now, as every row in this module stores time.
///
/// `chrono` rather than `SystemTime`, matching what the store binds. The crate has it
/// already, and it is the only clock the operational store's `*_at_ms` columns ever see.
///
/// One copy. It was written out three times in this crate and inlined at seven more sites,
/// which is ten places to change if the clock ever has to be injectable — and a test that
/// wants to control time cannot, today, for exactly that reason.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AttachmentData {
    pub name: String,
    #[serde(rename = "type")]
    pub mime_type: String,
    pub data: String,
}
