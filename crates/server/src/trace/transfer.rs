//! Explicit transfer budgets are independent of interactive read/cache limits.

#[derive(Debug, Clone)]
pub struct TransferLimits {
    pub max_jobs: usize,
    pub max_copy_bytes: u64,
    pub max_download_bytes: u64,
    pub max_buffered_bytes: u64,
    pub chunk_bytes: usize,
    pub lifetime_ms: i64,
}
impl Default for TransferLimits {
    fn default() -> Self {
        Self {
            max_jobs: 2,
            max_copy_bytes: 32 * 1024 * 1024,
            max_download_bytes: 256 * 1024 * 1024,
            max_buffered_bytes: 64 * 1024 * 1024,
            chunk_bytes: 256 * 1024,
            lifetime_ms: 120_000,
        }
    }
}
