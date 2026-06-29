//! Pool configuration and cache key types.

use std::path::PathBuf;

use edger_core::WorkerRef;

/// Pool sizing and ephemeral controls (design WorkerPool::new params).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    pub max_size: usize,
    pub ephemeral_concurrency: usize,
    pub ephemeral_queue_limit: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 32,
            ephemeral_concurrency: 4,
            ephemeral_queue_limit: 8,
        }
    }
}

/// LRU cache key — stable worker identity (dir + name + version).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WorkerCacheKey {
    pub dir: PathBuf,
    pub name: String,
    pub version: String,
}

impl WorkerCacheKey {
    pub fn from_worker_ref(worker_ref: &WorkerRef) -> Self {
        Self {
            dir: worker_ref.dir.clone(),
            name: worker_ref.name.clone(),
            version: worker_ref.version.clone(),
        }
    }
}
