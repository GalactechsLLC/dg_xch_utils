use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Default)]
pub struct MemoMetrics {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub shared: AtomicU64,
    pub evictions: AtomicU64,
    pub lock_wait_nanos: AtomicU64,
}

struct Entries<Key, Value> {
    values: HashMap<Key, (Arc<OnceLock<Value>>, u64)>,
    order: BTreeMap<u64, Key>,
    tick: u64,
}

pub(crate) struct Memo<Key, Value> {
    entries: Mutex<Entries<Key, Value>>,
    capacity: usize,
    pub metrics: MemoMetrics,
}

impl<Key: Clone + Eq + Hash, Value> Memo<Key, Value> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            entries: Mutex::new(Entries {
                values: HashMap::new(),
                order: BTreeMap::new(),
                tick: 0,
            }),
            capacity,
            metrics: MemoMetrics::default(),
        }
    }

    pub fn get_or_init(&self, key: Key, build: impl FnOnce() -> Value) -> Arc<OnceLock<Value>> {
        let started = Instant::now();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.metrics
            .lock_wait_nanos
            .fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        entries.tick += 1;
        let tick = entries.tick;
        let cell = if let Some((cell, previous)) = entries.values.get(&key).cloned() {
            entries.order.remove(&previous);
            self.metrics.hits.fetch_add(1, Ordering::Relaxed);
            if cell.get().is_none() {
                self.metrics.shared.fetch_add(1, Ordering::Relaxed);
            }
            cell
        } else {
            self.metrics.misses.fetch_add(1, Ordering::Relaxed);
            if entries.values.len() >= self.capacity {
                if let Some((_, oldest)) = entries.order.pop_first() {
                    entries.values.remove(&oldest);
                    self.metrics.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
            Arc::new(OnceLock::new())
        };
        entries.order.insert(tick, key.clone());
        entries.values.insert(key, (cell.clone(), tick));
        drop(entries);
        cell.get_or_init(build);
        cell
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values
            .len()
    }
}

pub fn metrics() -> [(&'static str, &'static MemoMetrics); 2] {
    [
        ("verification", &crate::proof::verify_memo().metrics),
        (
            "discriminant",
            &crate::discriminant::discriminant_memo().metrics,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_requests_share_derivation_and_eviction_is_bounded() {
        let memo = Memo::new(2);
        let builds = AtomicU64::new(0);
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    let value = memo.get_or_init(1, || {
                        builds.fetch_add(1, Ordering::Relaxed);
                        42
                    });
                    assert_eq!(value.get(), Some(&42));
                });
            }
        });
        assert_eq!(builds.load(Ordering::Relaxed), 1);
        memo.get_or_init(2, || 2);
        memo.get_or_init(1, || panic!("cached"));
        memo.get_or_init(3, || 3);
        assert_eq!(memo.len(), 2);
        memo.get_or_init(1, || panic!("recent entry evicted"));
        assert_eq!(memo.get_or_init(2, || 22).get(), Some(&22));
    }
}
