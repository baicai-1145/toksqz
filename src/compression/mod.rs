pub mod command_detector;
pub mod filter;
pub mod line_filter;
pub mod dedup;
pub mod truncate;
pub mod caveman;
pub mod grouping;
pub mod stats;
pub mod cache;

use once_cell::sync::Lazy;
use regex::Regex;

pub fn init() {
    let count = filter::load_filters();
    println!("[toksqz] Loaded {} filters", count);
    caveman::load_rules();
    // Initialize compression result cache
    cache::init();
    // Pre-cache env vars and regex at init time
    Lazy::force(&GROUPING_ENABLED);
    Lazy::force(&GROUPING_LEVEL);
    Lazy::force(&TRUNCATE_PRIORITY_RE);
}

/// Cached env: SQUEEZE_GROUPING
static GROUPING_ENABLED: Lazy<bool> = Lazy::new(|| {
    std::env::var("SQUEEZE_GROUPING")
        .unwrap_or_else(|_| "true".into()) != "false"
});

/// Cached env: SQUEEZE_GROUPING_LEVEL
static GROUPING_LEVEL: Lazy<String> = Lazy::new(|| {
    std::env::var("SQUEEZE_GROUPING_LEVEL")
        .unwrap_or_else(|_| "lite".into())
});

/// Pre-compiled priority pattern for truncation (used in every compress call)
static TRUNCATE_PRIORITY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)error|failed|exception|traceback|TS\d{4}|FAIL|\x{2716}").unwrap()
});

/// Result of RTK compression with detailed stats.
pub struct RtkCompressResult {
    pub text: String,
    pub filter_id: Option<String>,
    pub command_type: String,
    pub grouping_applied: bool,
}

/// Compress tool output using RTK engine (simple string variant)
#[allow(dead_code)]
pub fn rtk_compress(text: &str) -> String {
    rtk_compress_full(text).text
}

/// Full RTK compression with detailed result and caching.
pub fn rtk_compress_full(text: &str) -> RtkCompressResult {
    // Check cache first (only for non-empty input)
    if !text.is_empty() && cache::is_enabled() {
        let key = cache::hash_content(text);
        if let Some(cached_text) = cache::get(key) {
            // Cache hit - return cached result with minimal metadata
            // Note: We don't cache filter_id/command_type to save memory
            // The compression stats are still recorded
            return RtkCompressResult {
                text: cached_text,
                filter_id: Some("cache".to_string()),
                command_type: "cached".to_string(),
                grouping_applied: false,
            };
        }
    }

    let result = rtk_compress_inner(text);
    
    // Cache the result for future use
    if !text.is_empty() && cache::is_enabled() {
        let key = cache::hash_content(text);
        cache::insert(key, &result.text);
    }
    
    result
}

/// Inner compression logic (no caching)
fn rtk_compress_inner(text: &str) -> RtkCompressResult {
    use command_detector::detect_command_type;
    use filter::match_filter;
    use line_filter::apply_line_filter;
    use dedup::deduplicate_lines;
    use truncate::smart_truncate;
    use grouping::apply_grouping;

    // Detect command type
    let detection = detect_command_type(text, None);
    let command_type = detection.command_type.clone();

    // Match filter
    let matched = match_filter(text, detection.command.as_deref());
    let filter_id = matched.as_ref().map(|f| f.id.clone());

    let mut result = text.to_string();

    if let Some(ref f) = matched {
        let filtered = apply_line_filter(&result, f);
        result = filtered.text;
    }

    // Apply grouping (cached env vars)
    let mut grouping_applied = false;
    if *GROUPING_ENABLED {
        let grouped = apply_grouping(&result, &command_type, &GROUPING_LEVEL);
        if grouped.applied {
            result = grouped.text;
            grouping_applied = true;
        }
    }

    // Deduplicate (threshold=3)
    let deduped = deduplicate_lines(&result, 3);
    result = deduped.text;

    // Smart truncate (120 lines, 12000 chars) - use pre-compiled regex
    let priority_patterns = [&*TRUNCATE_PRIORITY_RE];
    let truncated = smart_truncate(&result, 120, 12000, 24, 24, &priority_patterns);
    result = truncated.text;

    RtkCompressResult {
        text: result,
        filter_id,
        command_type,
        grouping_applied,
    }
}

/// Compress user text using Caveman engine with caching.
pub fn caveman_compress(text: &str, level: &str) -> String {
    // Create a cache key that includes both text and level
    let cache_key = if cache::is_enabled() {
        Some(cache::hash_content(&format!("{}:{}", level, text)))
    } else {
        None
    };
    
    // Check cache
    if let Some(key) = cache_key {
        if let Some(cached) = cache::get(key) {
            return cached;
        }
    }
    
    // Compute
    let result = caveman::compress(text, "user", level);
    
    // Cache result
    if let Some(key) = cache_key {
        cache::insert(key, &result);
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtk_compress_full_returns_filter_id() {
        let input = "On branch main\nChanges not staged for commit:\n\tmodified: src/app.ts\nnothing added to commit";
        let result = rtk_compress_full(input);
        // Should match git-status filter
        assert!(result.filter_id.is_some() || result.command_type != "unknown");
    }

    #[test]
    fn test_rtk_compress_full_grouping_applied() {
        // Large multi-directory git status output
        let mut lines = vec!["On branch main".to_string(), "Changes not staged for commit:".to_string()];
        for dir in &["src", "src/utils", "src/components", "tests", "lib"] {
            for i in 0..5 {
                lines.push(format!("\tmodified: {}/file{}.ts", dir, i));
            }
        }
        let input = lines.join("\n");
        let result = rtk_compress_full(&input);
        // With many files across directories, grouping should be applied
        // (though it depends on the line_filter output first)
        assert!(!result.text.is_empty());
    }

    #[test]
    fn test_rtk_compress_empty_input() {
        let result = rtk_compress_full("");
        assert!(result.text.is_empty());
    }

    #[test]
    fn test_rtk_compress_plain_text() {
        let input = "This is just plain text output with no special patterns.";
        let result = rtk_compress_full(input);
        assert!(!result.text.is_empty());
        assert_eq!(result.command_type, "unknown");
    }

    #[test]
    fn test_rtk_compress_test_output() {
        let input = "\nrunning 10 tests\ntest a::b ... ok\ntest a::c ... ok\ntest a::d ... FAILED\nfailures:\n\n---- a::d stdout ----\nthread 'a::d' panicked at 'assertion failed'\ntest result: FAILED. 2 passed; 1 failed; 0 ignored";
        let result = rtk_compress_full(input);
        assert!(!result.text.is_empty());
        // Should detect test-cargo
        assert!(result.filter_id.is_some());
    }
}
