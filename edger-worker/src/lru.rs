//! Thread-safe LRU wrapper for worker instances.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use lru::LruCache;

use crate::instance::WorkerInstance;
use crate::types::WorkerCacheKey;

pub struct WorkerLru {
    inner: Mutex<LruCache<WorkerCacheKey, Arc<WorkerInstance>>>,
}

impl WorkerLru {
    pub fn new(max_size: usize) -> Self {
        let cap = NonZeroUsize::new(max_size.max(1)).unwrap();
        Self {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    pub fn get(&self, key: &WorkerCacheKey) -> Option<Arc<WorkerInstance>> {
        let mut cache = self.inner.lock().expect("lru lock");
        cache.get(key).cloned()
    }

    pub fn insert(
        &self,
        key: WorkerCacheKey,
        value: Arc<WorkerInstance>,
    ) -> Option<WorkerCacheKey> {
        let mut cache = self.inner.lock().expect("lru lock");
        if cache.contains(&key) {
            cache.put(key, value);
            return None;
        }
        let evicted_key = if cache.len() >= cache.cap().get() {
            cache.peek_lru().map(|(k, _)| k.clone())
        } else {
            None
        };
        cache.put(key, value);
        evicted_key
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("lru lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn remove(&self, key: &WorkerCacheKey) {
        let mut cache = self.inner.lock().expect("lru lock");
        cache.pop(key);
    }

    pub fn clear(&self) {
        self.inner.lock().expect("lru lock").clear();
    }
}
