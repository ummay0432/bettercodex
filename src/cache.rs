//! Focused blocking LRU cache retained from OpenAI Codex commit
//! 1669c2403f793d0230065397dfc25f52b844244e.

use lru::LruCache;
use sha1::Digest;
use sha1::Sha1;
use std::borrow::Borrow;
use std::hash::Hash;
use std::num::NonZeroUsize;
use tokio::sync::Mutex;
use tokio::sync::MutexGuard;

pub(crate) struct BlockingLruCache<K, V> {
    inner: Mutex<LruCache<K, V>>,
}

impl<K, V> BlockingLruCache<K, V>
where
    K: Eq + Hash,
{
    pub(crate) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub(crate) fn get_or_insert_with(&self, key: K, value: impl FnOnce() -> V) -> V
    where
        V: Clone,
    {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            if let Some(value) = guard.get(&key) {
                return value.clone();
            }
            let value = value();
            guard.put(key, value.clone());
            return value;
        }
        value()
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
        V: Clone,
    {
        let mut guard = lock_if_runtime(&self.inner)?;
        guard.get(key).cloned()
    }

    pub(crate) fn with_mut<R>(&self, callback: impl FnOnce(&mut LruCache<K, V>) -> R) -> R {
        if let Some(mut guard) = lock_if_runtime(&self.inner) {
            callback(&mut guard)
        } else {
            let mut disabled = LruCache::unbounded();
            callback(&mut disabled)
        }
    }
}

fn lock_if_runtime<K, V>(cache: &Mutex<LruCache<K, V>>) -> Option<MutexGuard<'_, LruCache<K, V>>>
where
    K: Eq + Hash,
{
    tokio::runtime::Handle::try_current().ok()?;
    Some(tokio::task::block_in_place(|| cache.blocking_lock()))
}

pub(crate) fn sha1_digest(bytes: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut output = [0; 20];
    output.copy_from_slice(&result);
    output
}
