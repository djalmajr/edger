//! edger-worker — WorkerPool, LRU, supervisor (Epic 04).
//!
//! Depends only on `edger-core` for production; isolate backends injected via `IsolateFactory`.

pub mod error;
pub mod factory;
pub mod instance;
pub mod lru;
pub mod metrics;
pub mod pool;
pub mod types;

pub use error::WorkerError;
pub use factory::IsolateFactory;
pub use instance::WorkerInstance;
pub use metrics::PoolMetrics;
pub use pool::WorkerPool;
pub use types::{PoolConfig, WorkerCacheKey};
