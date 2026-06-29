//! Pool metrics snapshot (extended in story 04.03).

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolMetrics {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub active_workers: usize,
}
