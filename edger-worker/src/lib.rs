//! edger-worker — WorkerPool, LRU, supervisor (Epic 04).
//!
//! Isolate backends are injected via `IsolateFactory`; shared limit guards live in edger-isolation.

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
pub use pool::{LifecycleEventSender, WorkerLifecycleEvent, WorkerLifecycleEventKind, WorkerPool};
pub use state::{transition, WorkerEvent, WorkerState};
pub use supervisor::Supervisor;
pub use types::{PoolConfig, WorkerCacheKey};
