//! Thread-safe LRU wrapper for worker instance groups.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use lru::LruCache;
use uuid::Uuid;

use crate::instance::WorkerInstance;
use crate::state::{accepts_dispatch, WorkerState};
use crate::types::WorkerCacheKey;

pub enum ReservedSlot {
    Acquired {
        instance: Arc<WorkerInstance>,
        guard: tokio::sync::OwnedMutexGuard<()>,
    },
    Wait(Arc<WorkerInstance>),
    Unavailable,
}

pub struct WorkerGroup {
    instances: Mutex<Vec<Arc<WorkerInstance>>>,
    next_index: AtomicUsize,
}

impl WorkerGroup {
    pub fn new(instances: Vec<Arc<WorkerInstance>>) -> Self {
        Self {
            instances: Mutex::new(instances),
            next_index: AtomicUsize::new(0),
        }
    }

    pub fn instances_snapshot(&self) -> Vec<Arc<WorkerInstance>> {
        self.instances.lock().expect("worker group lock").clone()
    }

    pub fn len(&self) -> usize {
        self.instances.lock().expect("worker group lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn next_round_robin(&self, len: usize) -> usize {
        self.next_index.fetch_add(1, Ordering::Relaxed) % len.max(1)
    }

    pub fn remove_instance(&self, instance_id: Uuid) -> bool {
        let mut instances = self.instances.lock().expect("worker group lock");
        instances.retain(|instance| instance.id() != instance_id);
        instances.is_empty()
    }

    pub fn reserve_slot<F>(&self, max_processes: usize, mut create: F) -> ReservedSlot
    where
        F: FnMut() -> Arc<WorkerInstance>,
    {
        let mut instances = self.instances.lock().expect("worker group lock");
        instances.retain(|instance| instance.state() != WorkerState::Terminated);

        if instances.is_empty() {
            let instance = create();
            let guard = instance
                .dispatch_lock()
                .try_lock_owned()
                .expect("new worker instance dispatch lock must be free");
            instances.push(Arc::clone(&instance));
            return ReservedSlot::Acquired { instance, guard };
        }

        let len = instances.len();
        let offset = self.next_round_robin(len);
        let mut wait_candidate = None;

        for step in 0..len {
            let instance = Arc::clone(&instances[(offset + step) % len]);
            match instance.dispatch_lock().try_lock_owned() {
                Ok(guard) => {
                    if instance.state() == WorkerState::Creating
                        || accepts_dispatch(instance.state())
                    {
                        return ReservedSlot::Acquired { instance, guard };
                    }
                }
                Err(_) => {
                    if wait_candidate.is_none() {
                        wait_candidate = Some(instance);
                    }
                }
            }
        }

        if instances.len() < max_processes.max(1) {
            let instance = create();
            let guard = instance
                .dispatch_lock()
                .try_lock_owned()
                .expect("new worker instance dispatch lock must be free");
            instances.push(Arc::clone(&instance));
            return ReservedSlot::Acquired { instance, guard };
        }

        wait_candidate
            .map(ReservedSlot::Wait)
            .unwrap_or(ReservedSlot::Unavailable)
    }
}

pub struct WorkerLru {
    inner: Mutex<LruCache<WorkerCacheKey, Arc<WorkerGroup>>>,
}

impl WorkerLru {
    pub fn new(max_size: usize) -> Self {
        let cap = NonZeroUsize::new(max_size.max(1)).unwrap();
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn get_group(&self, key: &WorkerCacheKey) -> Option<Arc<WorkerGroup>> {
        let mut cache = self.inner.lock().expect("lru lock");
        cache.get(key).cloned()
    }

    pub fn insert_group(
        &self,
        key: WorkerCacheKey,
        group: Arc<WorkerGroup>,
    ) -> Option<WorkerCacheKey> {
        let mut cache = self.inner.lock().expect("lru lock");
        if cache.contains(&key) {
            cache.put(key, group);
            return None;
        }
        let evicted_key = if cache.len() >= cache.cap().get() {
            cache.peek_lru().map(|(k, _)| k.clone())
        } else {
            None
        };
        cache.put(key, group);
        evicted_key
    }

    pub fn group_count(&self) -> usize {
        self.inner.lock().expect("lru lock").len()
    }

    pub fn len(&self) -> usize {
        self.values_snapshot().len()
    }

    pub fn is_empty(&self) -> bool {
        self.group_count() == 0
    }

    pub fn remove_instance(&self, key: &WorkerCacheKey, instance_id: Uuid) {
        let mut cache = self.inner.lock().expect("lru lock");
        let remove_group = cache
            .peek(key)
            .map(|group| group.remove_instance(instance_id))
            .unwrap_or(false);
        if remove_group {
            cache.pop(key);
        }
    }

    pub fn find_by_worker_id(&self, worker_id: Uuid) -> Option<Arc<WorkerInstance>> {
        let cache = self.inner.lock().expect("lru lock");
        cache
            .iter()
            .flat_map(|(_, group)| group.instances_snapshot())
            .find(|instance| instance.id() == worker_id)
    }

    pub fn values_snapshot(&self) -> Vec<Arc<WorkerInstance>> {
        self.inner
            .lock()
            .expect("lru lock")
            .iter()
            .flat_map(|(_, group)| group.instances_snapshot())
            .collect()
    }

    pub fn count_idle(&self) -> usize {
        self.values_snapshot()
            .iter()
            .filter(|instance| instance.state() == WorkerState::Idle)
            .count()
    }

    pub fn clear(&self) {
        self.inner.lock().expect("lru lock").clear();
    }
}
