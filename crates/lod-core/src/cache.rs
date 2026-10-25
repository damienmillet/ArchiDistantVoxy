//! Persistent LOD section cache (EF-4) with a bounded in-RAM LRU layer (ENF-3).
//!
//! The persistent layer is an abstract key-value blob store so tests can use an
//! in-memory implementation while the plugin provides a SQLite-backed one.
//! SPDX-License-Identifier: GPL-3.0-or-later.

use std::collections::HashMap;
use std::sync::Mutex;

/// Cache statistics exposed by `/lod status` (EF-8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Total cache lookups.
    pub lookups: u64,
    /// Lookups served from the persistent store.
    pub hits: u64,
    /// Lookups served from the RAM layer.
    pub ram_hits: u64,
}

impl CacheStats {
    /// Hit rate over all lookups (persistent + RAM), in [0, 1].
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        if self.lookups == 0 {
            return 0.0;
        }
        f64::from(u32::try_from(self.hits + self.ram_hits).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(self.lookups).unwrap_or(u32::MAX))
    }
}

/// Persistent blob store abstraction for LOD sections.
pub trait BlobStore: Send + Sync {
    /// Reads the blob stored for `key`, if any.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;
    /// Writes or replaces the blob for `key`.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn put(&self, key: &str, blob: &[u8]) -> Result<(), String>;
    /// Removes the blob for `key` if present.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn remove(&self, key: &str) -> Result<(), String>;
    /// Number of stored blobs.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn len(&self) -> Result<usize, String>;
    /// Whether the store holds no blob.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn is_empty(&self) -> Result<bool, String> {
        Ok(self.len()? == 0)
    }

    /// Removes every stored blob.
    ///
    /// # Errors
    /// Returns the store's error type on storage failure.
    fn clear(&self) -> Result<(), String>;
}

/// In-memory [`BlobStore`] used for tests and as the RAM layer.
#[derive(Debug, Default)]
pub struct MemoryStore {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlobStore for Box<dyn BlobStore> {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        (**self).get(key)
    }

    fn put(&self, key: &str, blob: &[u8]) -> Result<(), String> {
        (**self).put(key, blob)
    }

    fn remove(&self, key: &str) -> Result<(), String> {
        (**self).remove(key)
    }

    fn len(&self) -> Result<usize, String> {
        (**self).len()
    }

    fn clear(&self) -> Result<(), String> {
        (**self).clear()
    }
}

impl BlobStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.map.lock().expect("poisoned").get(key).cloned())
    }

    fn put(&self, key: &str, blob: &[u8]) -> Result<(), String> {
        self.map
            .lock()
            .expect("poisoned")
            .insert(key.to_string(), blob.to_vec());
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<(), String> {
        self.map.lock().expect("poisoned").remove(key);
        Ok(())
    }

    fn len(&self) -> Result<usize, String> {
        Ok(self.map.lock().expect("poisoned").len())
    }

    fn clear(&self) -> Result<(), String> {
        self.map.lock().expect("poisoned").clear();
        Ok(())
    }
}

/// Cache key for one LOD section in one dimension.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SectionKey {
    /// Dimension (world) name.
    pub dimension: String,
    /// Section X coordinate.
    pub x: i32,
    /// Section Z coordinate.
    pub z: i32,
}

impl SectionKey {
    /// Builds the cache key string used by the persistent store.
    #[must_use]
    pub fn as_string(&self) -> String {
        format!("{}/{}/{}", self.dimension, self.x, self.z)
    }
}

/// Two-level LOD cache: bounded RAM LRU over a persistent blob store.
pub struct LodCache<P: BlobStore> {
    persistent: P,
    ram: HashMap<String, (u32, Vec<u8>)>,
    ram_capacity: usize,
    ram_bytes: usize,
    next_tick: u32,
    stats: CacheStats,
}

impl<P: BlobStore> LodCache<P> {
    /// Creates a cache over `persistent` with a RAM layer of `ram_capacity`
    /// sections max.
    #[must_use]
    pub fn new(persistent: P, ram_capacity: usize) -> Self {
        Self {
            persistent,
            ram: HashMap::new(),
            ram_capacity,
            ram_bytes: 0,
            next_tick: 0,
            stats: CacheStats::default(),
        }
    }

    /// Looks up an encoded LOD section.
    ///
    /// RAM hits are prioritized; persistent hits promote the blob to RAM.
    ///
    /// # Errors
    /// Fails when the persistent store fails.
    pub fn get(&mut self, key: &SectionKey) -> Result<Option<Vec<u8>>, String> {
        self.stats.lookups += 1;
        let k = key.as_string();
        if let Some((tick, blob)) = self.ram.get(&k) {
            let tick = *tick;
            let blob = blob.clone();
            self.stats.ram_hits += 1;
            self.touch(&k, tick);
            return Ok(Some(blob));
        }
        match self.persistent.get(&k)? {
            Some(blob) => {
                self.stats.hits += 1;
                self.promote(k, blob.clone());
                Ok(Some(blob))
            }
            None => Ok(None),
        }
    }

    /// Stores an encoded LOD section in both layers.
    ///
    /// # Errors
    /// Fails when the persistent store fails.
    pub fn put(&mut self, key: &SectionKey, blob: Vec<u8>) -> Result<(), String> {
        let k = key.as_string();
        self.persistent.put(&k, &blob)?;
        let len = blob.len();
        if let Some((_, old)) = self.ram.insert(k.clone(), (self.next_tick, blob)) {
            self.ram_bytes -= old.len();
        }
        self.ram_bytes += len;
        self.evict_if_needed();
        self.next_tick += 1;
        Ok(())
    }

    /// Invalidates one section (EF-7 immediate policy).
    ///
    /// # Errors
    /// Fails when the persistent store fails.
    pub fn invalidate(&mut self, key: &SectionKey) -> Result<(), String> {
        let k = key.as_string();
        if let Some((_, old)) = self.ram.remove(&k) {
            self.ram_bytes -= old.len();
        }
        self.persistent.remove(&k)
    }

    /// Number of sections in the persistent store.
    ///
    /// # Errors
    /// Fails when the persistent store fails.
    pub fn persistent_len(&self) -> Result<usize, String> {
        self.persistent.len()
    }

    /// Clears the RAM layer and every persistent blob.
    ///
    /// # Errors
    /// Fails when the persistent store fails.
    pub fn clear(&mut self) -> Result<(), String> {
        self.ram.clear();
        self.ram_bytes = 0;
        self.persistent.clear()
    }

    /// Current RAM layer size in bytes.
    #[must_use]
    pub fn ram_bytes(&self) -> usize {
        self.ram_bytes
    }

    /// Collected statistics.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    fn touch(&mut self, k: &str, _tick: u32) {
        self.next_tick += 1;
        if let Some(entry) = self.ram.get_mut(k) {
            entry.0 = self.next_tick;
        }
    }

    fn promote(&mut self, k: String, blob: Vec<u8>) {
        let len = blob.len();
        if let Some((_, old)) = self.ram.insert(k, (self.next_tick, blob)) {
            self.ram_bytes -= old.len();
        }
        self.ram_bytes += len;
        self.next_tick += 1;
        self.evict_if_needed();
    }

    fn evict_if_needed(&mut self) {
        while self.ram.len() > self.ram_capacity {
            let evict = self
                .ram
                .iter()
                .min_by_key(|(_, (tick, _))| *tick)
                .map(|(k, _)| k.clone());
            match evict {
                Some(k) => {
                    if let Some((_, old)) = self.ram.remove(&k) {
                        self.ram_bytes -= old.len();
                    }
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(x: i32, z: i32) -> SectionKey {
        SectionKey {
            dimension: "overworld".into(),
            x,
            z,
        }
    }

    #[test]
    fn put_get_roundtrip_and_stats() {
        let mut cache = LodCache::new(MemoryStore::new(), 10);
        assert_eq!(cache.get(&key(0, 0)), Ok(None));
        cache.put(&key(0, 0), vec![1, 2, 3]).unwrap();
        // `put` also inserts into the RAM layer, so the next get is a RAM hit.
        assert_eq!(cache.get(&key(0, 0)), Ok(Some(vec![1, 2, 3])));
        let stats = cache.stats();
        assert_eq!(stats.lookups, 2);
        assert_eq!(stats.ram_hits, 1);
        assert_eq!(stats.hits, 0);
        assert!(stats.hit_rate() > 0.0);

    }

    #[test]
    fn persistent_hit_after_ram_eviction() {
        let mut cache = LodCache::new(MemoryStore::new(), 1);
        cache.put(&key(0, 0), vec![1]).unwrap();
        cache.put(&key(1, 0), vec![2]).unwrap(); // evicts (0,0) from RAM
        let stats_before = cache.stats();
        assert_eq!(stats_before.hits, 0);
        assert_eq!(cache.get(&key(0, 0)), Ok(Some(vec![1])));
        let stats = cache.stats();
        assert_eq!(stats.hits, 1, "served from the persistent layer");
    }

    #[test]
    fn ram_layer_is_bounded_and_lru() {
        let mut cache = LodCache::new(MemoryStore::new(), 2);
        for x in 0..4u8 {
            cache.put(&key(i32::from(x), 0), vec![x; 100]).unwrap();
        }
        assert_eq!(cache.ram.len(), 2);
        assert!(cache.get(&key(0, 0)).unwrap().is_some(), "evicted to persistent");
        assert_eq!(cache.ram.len(), 2);
    }

    #[test]
    fn invalidate_removes_both_layers() {
        let mut cache = LodCache::new(MemoryStore::new(), 10);
        cache.put(&key(1, 1), vec![9]).unwrap();
        cache.invalidate(&key(1, 1)).unwrap();
        assert_eq!(cache.get(&key(1, 1)), Ok(None));
        assert_eq!(cache.persistent_len(), Ok(0));
    }

    #[test]
    fn dimensions_are_isolated() {
        let mut cache = LodCache::new(MemoryStore::new(), 10);
        let nether = SectionKey {
            dimension: "the_nether".into(),
            x: 0,
            z: 0,
        };
        cache.put(&key(0, 0), vec![1]).unwrap();
        assert_eq!(cache.get(&nether), Ok(None));
        cache.put(&nether, vec![2]).unwrap();
        assert_eq!(cache.get(&key(0, 0)), Ok(Some(vec![1])));
        assert_eq!(cache.get(&nether), Ok(Some(vec![2])));
    }

    #[test]
    fn clear_empties_both_layers() {
        let mut cache = LodCache::new(MemoryStore::new(), 10);
        cache.put(&key(0, 0), vec![1]).unwrap();
        cache.put(&key(1, 0), vec![2]).unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.get(&key(0, 0)), Ok(None));
        assert_eq!(cache.get(&key(1, 0)), Ok(None));
        assert_eq!(cache.ram_bytes(), 0);
        assert_eq!(cache.persistent_len(), Ok(0));
    }

    #[test]
    fn key_string_is_stable() {
        let k = SectionKey {
            dimension: "overworld".into(),
            x: -5,
            z: 12,
        };
        assert_eq!(k.as_string(), "overworld/-5/12");
    }
}
