//! edger-worker — WorkerPool, LRU, supervisor (Epic 04).
//!
//! Depends only on `edger-core` for production; isolate backends injected via `IsolateFactory`.

pub mod ephemeral;
pub mod error;
pub mod factory;
pub mod instance;
pub mod lru;
pub mod metrics;
pub mod pool;
pub mod state;
pub mod supervisor;
pub mod types;

pub use ephemeral::EphemeralGate;
pub use error::WorkerError;
pub use factory::IsolateFactory;
pub use instance::WorkerInstance;
pub use metrics::{MetricsCollector, PoolMetrics, WorkerStats};
pub use pool::WorkerPool;
pub use state::{transition, WorkerEvent, WorkerState};
pub use supervisor::Supervisor;
pub use types::{PoolConfig, WorkerCacheKey};
