//! Full-environment streaming budgets; downloads and playback have independent policies.
use serde::Serialize;
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_jobs: usize,
    pub max_source_bytes: u64,
    pub max_output_bytes: u64,
    pub max_buffered_bytes: u64,
    pub max_admissions: usize,
    pub max_page_vm_steps: u64,
    pub frame_bytes: usize,
    pub chunk_bytes: usize,
    pub lifetime_ms: i64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_jobs: 2,
            max_source_bytes: 512 * 1024 * 1024,
            max_output_bytes: 768 * 1024 * 1024,
            max_buffered_bytes: 64 * 1024 * 1024,
            max_admissions: 100_000,
            max_page_vm_steps: super::evidence::export::PAGE_QUERY_WORK,
            frame_bytes: 2 * 1024 * 1024,
            chunk_bytes: 64 * 1024,
            lifetime_ms: 300_000,
        }
    }
}

impl Limits {
    /// Playback is an interactive stream, never an archive-sized browser buffer.
    pub fn playback() -> Self {
        Self {
            max_jobs: 1,
            max_source_bytes: 32 * 1024 * 1024,
            max_output_bytes: 48 * 1024 * 1024,
            max_buffered_bytes: 0,
            max_admissions: 10_000,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pacing {
    pub clock: &'static str,
    pub speed: u8,
    pub max_gap_ms: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReceiptTiming {
    pub admission_seq: u64,
    pub recorded_delta_ms: Option<i64>,
    pub delay_ms: u64,
    pub adjustment: &'static str,
}
impl Pacing {
    pub fn timing(&self, admission_seq: u64, previous: Option<i64>, current: i64) -> ReceiptTiming {
        let mut result = ReceiptTiming {
            admission_seq,
            recorded_delta_ms: None,
            delay_ms: 0,
            adjustment: "first",
        };
        if let Some(previous) = previous {
            result.recorded_delta_ms = current.checked_sub(previous);
            match result.recorded_delta_ms {
                None => result.adjustment = "unrepresentable_delta",
                Some(delta) if delta < 0 => result.adjustment = "backward_clock",
                Some(delta) => {
                    let scaled = (delta as u64).div_ceil(u64::from(self.speed));
                    result.delay_ms = scaled.min(self.max_gap_ms);
                    result.adjustment = if scaled > self.max_gap_ms {
                        "gap_clamped"
                    } else {
                        "recorded"
                    };
                }
            }
        }
        result
    }
}
