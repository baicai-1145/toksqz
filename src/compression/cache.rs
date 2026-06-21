//! LRU cache for compression results with TTL expiration.
//! 
//! In Agent scenarios, the same tool outputs appear repeatedly across turns.
//! This cache stores results directly (no compression overhead) to avoid
//! redundant compression work.
//! 
//! # Memory Efficiency
//! - Direct string storage: no zstd alloc/dealloc overhead
//! - LRU eviction: bounded memory usage
//! - TTL expiration: stale entries auto-cleaned (default 24h)
//! - Cache hit: ~0.5μs (clone) vs ~8000μs (full compression)
//! 
//! # Configuration
//! - `SQUEEZE_CACHE_SIZE`: Max cache entries (default: 10000)
//! - `SQUEEZE_CACHE_TTL`: TTL in seconds (default: 86400 = 24h)
//! - Set cache size to 0 to disable caching

use lru::LruCache;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

/// Default cache capacity (~17 MB max at 1.75 KB/entry)
const DEFAULT_CACHE_SIZE: usize = 10000;

/// Default TTL: 24 hours
const DEFAULT_TTL: Duration = Duration::from_secs(86400);

/// Global TTL value (cached at first access)
static TTL: Lazy<Duration> = Lazy::new(|| {
    std::env::var("SQUEEZE_CACHE_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TTL)
});

/// Cache entry: compressed string + last access time
struct CacheEntry {
    value: String,
    accessed: Instant,
}

/// Global cache instance — stores raw Strings with TTL tracking
static CACHE: Lazy<Mutex<LruCache<u64, CacheEntry>>> = Lazy::new(|| {
    let capacity = cache_capacity();
    Mutex::new(LruCache::new(NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::new(1).unwrap())))
});

/// Cache hit/miss statistics
static STATS: Lazy<Mutex<CacheStats>> = Lazy::new(|| Mutex::new(CacheStats::default()));

#[derive(Default, Clone)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// Get cache capacity from environment
fn cache_capacity() -> usize {
    std::env::var("SQUEEZE_CACHE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_CACHE_SIZE)
}

/// Fast hash function for cache keys (FxHash-style)
#[inline]
pub fn hash_content(content: &str) -> u64 {
    // Use a simple but fast hash - FNV-1a variant
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in content.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}


/// Get TTL from environment
fn cache_ttl() -> Duration {
    *TTL
}

/// Initialize the cache (call at startup)
pub fn init() {
    let capacity = cache_capacity();
    if capacity > 0 {
        // Force initialization
        Lazy::force(&CACHE);
        Lazy::force(&TTL);
        let ttl_secs = cache_ttl().as_secs();
        println!("[cache] Initialized with capacity: {} entries, TTL: {}s", capacity, ttl_secs);
    }
}

/// Check if caching is enabled
pub fn is_enabled() -> bool {
    cache_capacity() > 0
}

/// Get a compressed result from cache.
/// Returns the cached string if found and not expired.
#[inline]
pub fn get(key: u64) -> Option<String> {
    if !is_enabled() {
        return None;
    }
    
    let mut cache = CACHE.lock();
    let ttl = cache_ttl();
    let now = Instant::now();
    
    // LruCache::get_mut returns &mut V — update accessed time in place
    let found = if let Some(entry) = cache.get_mut(&key) {
        if now.duration_since(entry.accessed) < ttl {
            entry.accessed = now;  // refresh timestamp via &mut
            Some(entry.value.clone())
        } else {
            None  // expired
        }
    } else {
        None  // not found
    };
    
    // If expired, remove (mutable borrow from get() is released here)
    if found.is_none() && cache.contains(&key) {
        cache.pop(&key);
    }
    
    let mut stats = STATS.lock();
    if let Some(value) = found {
        stats.hits += 1;
        Some(value)
    } else {
        stats.misses += 1;
        None
    }
}

/// Insert a compression result into cache.
/// Stores the string directly with access timestamp.
/// Periodically cleans up expired entries.
#[inline]
pub fn insert(key: u64, value: &str) {
    if !is_enabled() {
        return;
    }
    
    let mut cache = CACHE.lock();
    let now = Instant::now();
    
    // Track evictions
    if cache.len() >= cache.cap().get() {
        let mut stats = STATS.lock();
        stats.evictions += 1;
    }
    
    // Clean up expired entries before inserting
    let ttl = cache_ttl();
    let expired_keys: Vec<u64> = cache.iter()
        .filter(|(_, e)| now.duration_since(e.accessed) >= ttl)
        .map(|(k, _)| *k)
        .collect();
    for k in expired_keys {
        cache.pop(&k);
    }
    
    cache.put(key, CacheEntry { value: value.to_string(), accessed: now });
}

/// Get or compute: try cache first, fallback to compute function.
/// 
/// # Example
/// ```ignore
/// let result = cache::get_or_compute(hash, || {
///     // Expensive compression work
///     compress_expensive(input)
/// });
/// ```
#[inline]
pub fn get_or_compute<F>(key: u64, compute: F) -> String
where
    F: FnOnce() -> String,
{
    // Try cache first
    if let Some(cached) = get(key) {
        return cached;
    }
    
    // Compute and cache
    let result = compute();
    insert(key, &result);
    result
}

/// Get cache statistics
pub fn get_stats() -> CacheStats {
    STATS.lock().clone()
}

/// Reset cache statistics
pub fn reset_stats() {
    *STATS.lock() = CacheStats::default();
}

/// Clear all cached entries
pub fn clear() {
    let mut cache = CACHE.lock();
    cache.clear();
    reset_stats();
}

/// Get current cache size (number of entries)
pub fn len() -> usize {
    CACHE.lock().len()
}

/// Check if cache is empty
pub fn is_empty() -> bool {
    len() == 0
}

/// Estimate current cache memory usage in bytes.
/// Includes key + string heap + Instant + node overhead for each entry.
pub fn memory_bytes() -> usize {
    let cache = CACHE.lock();
    // LruCache overhead: ~72 bytes per node (prev/next pointers + key + value)
    // Plus the actual String heap data + Instant (32 bytes)
    let mut total = 0usize;
    for (_, e) in cache.iter() {
        total += 8 + e.value.capacity() + 32 + 72; // key(8) + string heap + Instant + node
    }
    total
}

/// Get cache capacity
pub fn capacity() -> usize {
    CACHE.lock().cap().get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_content() {
        let h1 = hash_content("hello");
        let h2 = hash_content("hello");
        let h3 = hash_content("world");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_cache_roundtrip() {
        let original = "Hello, World! This is a test string for caching roundtrip unique123";
        let key = hash_content(original);
        insert(key, original);
        let cached = get(key).unwrap();
        assert_eq!(original, cached);
    }

    #[test]
    fn test_cache_insert_get() {
        // Use unique key to avoid collision with concurrent tests
        let key = hash_content("test content unique insert get 7f3a9b2c");
        let value = "compressed result for test content 7f3a9b2c";
        
        // Record stats before
        let stats_before = get_stats();
        
        // Insert
        insert(key, value);
        
        // Should be retrievable
        let cached = get(key).unwrap();
        assert_eq!(cached, value);
        
        // Stats should have incremented
        let stats_after = get_stats();
        assert!(stats_after.hits > stats_before.hits, "Expected at least 1 more hit");
    }

    #[test]
    fn test_cache_lru_eviction() {
        // Insert many items with unique prefix to avoid collision
        for i in 0..100 {
            insert(1_000_000 + i, &format!("lru_eviction_value{}", i));
        }
        
        // Cache should have entries
        let l = len();
        assert!(l > 0, "Cache should have entries");
        assert!(l <= 10000, "Cache should not exceed default capacity");
        
        // Recent entries should be accessible
        let recent = get(1_000_000 + 99);
        assert!(recent.is_some(), "Recent entry should be in cache");
        assert_eq!(recent.unwrap(), "lru_eviction_value99");
    }

    #[test]
    fn test_get_or_compute() {
        let key = hash_content("compute me unique key 8d4f2e1a");
        let mut compute_count = 0;
        
        // First call: insert the value
        insert(key, "computed value");
        
        // get_or_compute should find it in cache
        let result = get_or_compute(key, || {
            compute_count += 1;
            "should not be called".to_string()
        });
        assert_eq!(result, "computed value");
        assert_eq!(compute_count, 0); // Cache hit, compute not called
    }

    #[test]
    fn test_cache_memory_size() {
        // Insert a typical tool output and verify cache storage
        let typical_output = r#"On branch main
Changes not staged for commit:
  (use "git add <file>..." to update what will be committed)
  (use "git restore <file>..." to discard changes in working directory)
        modified:   src/compression/mod.rs
        modified:   src/compression/cache.rs
        modified:   src/proxy.rs
        modified:   Cargo.toml

no changes added to commit (use "git add" and/or "git commit -a")"#;

        let key = hash_content(typical_output);
        insert(key, typical_output);
        
        // Verify retrieval
        let cached = get(key).unwrap();
        assert_eq!(cached, typical_output);
        
        // Verify cache has entries and memory > 0
        assert!(len() >= 1);
        assert!(memory_bytes() > 0, "Cache memory should be > 0");
    }
}
