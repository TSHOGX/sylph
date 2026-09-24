//! Full-environment export budgets, independent of interactive reads and payload transfer.
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
