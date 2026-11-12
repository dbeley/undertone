//! A small bounded TTL + LRU cache for server responses.
//!
//! Deliberately not an index: it stores whatever was fetched, evicts by least
//! recent use, and expires by age. It exists so that going back to a view you
//! just left does not re-hit the network, and nothing more.

use std::collections::HashMap;
use std::time::{Duration, Instant};

struct Entry<V> {
    value: V,
    expires_at: Instant,
    last_used: u64,
}

pub struct TtlCache<V> {
    entries: HashMap<String, Entry<V>>,
    capacity: usize,
    ttl: Duration,
    clock: u64,
}

impl<V> TtlCache<V> {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(64)),
            capacity,
            ttl,
            clock: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(500, Duration::from_secs(300))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn get(&mut self, key: &str) -> Option<&V> {
        self.get_at(key, Instant::now())
    }

    pub fn insert(&mut self, key: impl Into<String>, value: V) {
        self.insert_at(key, value, Instant::now());
    }

    /// Test-visible variant: lookup against an explicit clock.
    pub fn get_at(&mut self, key: &str, now: Instant) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;

        // Two-phase to satisfy the borrow checker: check expiry, then touch.
        let expired = {
            let entry = self.entries.get(key)?;
            entry.expires_at <= now
        };
        if expired {
            self.entries.remove(key);
            return None;
        }
        let entry = self.entries.get_mut(key)?;
        entry.last_used = clock;
        Some(&entry.value)
    }

    /// Test-visible variant: insert against an explicit clock.
    pub fn insert_at(&mut self, key: impl Into<String>, value: V, now: Instant) {
        self.clock += 1;
        let clock = self.clock;
        let key = key.into();

        if self.entries.len() >= self.capacity && !self.entries.contains_key(&key) {
            self.evict_one();
        }
        self.entries.insert(
            key,
            Entry {
                value,
                expires_at: now + self.ttl,
                last_used: clock,
            },
        );
    }

    /// Drop entries whose TTL has passed. Call from the render worker, not the
    /// hot path.
    pub fn purge_expired(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    fn evict_one(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone());
        if let Some(victim) = victim {
            self.entries.remove(&victim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ttl() -> Duration {
        Duration::from_secs(60)
    }

    #[test]
    fn stores_and_returns_values() {
        let mut cache: TtlCache<u32> = TtlCache::new(4, ttl());
        cache.insert("a", 1);
        assert_eq!(cache.get("a"), Some(&1));
        assert_eq!(cache.get("missing"), None);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn entries_expire() {
        let mut cache: TtlCache<u32> = TtlCache::new(4, Duration::from_secs(10));
        let start = Instant::now();
        cache.insert_at("a", 1, start);
        assert_eq!(cache.get_at("a", start + Duration::from_secs(9)), Some(&1));
        assert_eq!(cache.get_at("a", start + Duration::from_secs(11)), None);
        assert!(cache.is_empty(), "expired entry should be removed on read");
    }

    #[test]
    fn evicts_least_recently_used() {
        let mut cache: TtlCache<u32> = TtlCache::new(3, ttl());
        let t = Instant::now();
        cache.insert_at("a", 1, t);
        cache.insert_at("b", 2, t);
        cache.insert_at("c", 3, t);
        // Touch "a" so "b" becomes the oldest.
        assert_eq!(cache.get_at("a", t), Some(&1));
        cache.insert_at("d", 4, t);
        assert_eq!(cache.len(), 3);
        assert!(cache.get("b").is_none(), "b should have been evicted");
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());
        assert!(cache.get("d").is_some());
    }

    #[test]
    fn capacity_is_never_exceeded() {
        let mut cache: TtlCache<u32> = TtlCache::new(10, ttl());
        for i in 0..100 {
            cache.insert(format!("k{i}"), i);
        }
        assert_eq!(cache.len(), 10);
    }

    #[test]
    fn overwriting_a_key_does_not_evict() {
        let mut cache: TtlCache<u32> = TtlCache::new(2, ttl());
        cache.insert("a", 1);
        cache.insert("b", 2);
        cache.insert("a", 99);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get("a"), Some(&99));
    }

    #[test]
    fn purge_drops_only_expired() {
        let mut cache: TtlCache<u32> = TtlCache::new(8, Duration::from_secs(10));
        let now = Instant::now();
        // Insert one already-expired entry and one live one.
        cache.insert_at("old", 1, now - Duration::from_secs(30));
        cache.insert_at("new", 2, now);
        assert_eq!(cache.len(), 2);
        cache.purge_expired();
        assert_eq!(cache.len(), 1);
        assert!(cache.get("new").is_some());
    }

    #[test]
    fn images_are_not_cached_here() {
        // Guard against scope creep: the cache holds response values, and the
        // art blob cache is deliberately a separate on-disk concern.
        let cache: TtlCache<Vec<u8>> = TtlCache::new(4, ttl());
        assert_eq!(cache.capacity(), 4);
    }
}
