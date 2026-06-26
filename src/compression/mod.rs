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

pub fn init() {
    // All components are Lazy — they initialize on first request.
    // This keeps startup memory minimal (~2 MB base).
    // First request triggers filter/regex/caveman/cache init automatically.
    println!("[toksqz] 延迟初始化模式 — 首次请求时加载压缩引擎");
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

/// Global RTK char budget fallback (per-filter maxLines already applied in line_filter).
static GLOBAL_MAX_CHARS: Lazy<usize> = Lazy::new(|| {
    std::env::var("SQUEEZE_RTK_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12_000)
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
    rtk_compress_with_command(text, None)
}

/// Full RTK compression with an optional explicit command hint.
pub fn rtk_compress_with_command(text: &str, command: Option<&str>) -> RtkCompressResult {
    // Check cache first (only for non-empty input)
    if !text.is_empty() && cache::is_enabled() {
        let cache_key = if let Some(cmd) = command {
            format!("cmd:{}\n{}", cmd, text)
        } else {
            text.to_string()
        };
        let key = cache::hash_content(&cache_key);
        if let Some(cached) = cache::get(key) {
            return RtkCompressResult {
                text: cached.text,
                filter_id: cached.filter_id.or_else(|| Some("cache".to_string())),
                command_type: cached.command_type,
                grouping_applied: cached.grouping_applied,
            };
        }
    }

    let result = rtk_compress_inner(text, command);
    
    // Cache the result for future use
    if !text.is_empty() && cache::is_enabled() {
        let cache_key = if let Some(cmd) = command {
            format!("cmd:{}\n{}", cmd, text)
        } else {
            text.to_string()
        };
        let key = cache::hash_content(&cache_key);
        cache::insert(key, cache::CachedCompressionResult {
            text: result.text.clone(),
            filter_id: result.filter_id.clone(),
            command_type: result.command_type.clone(),
            grouping_applied: result.grouping_applied,
        });
    }
    
    result
}

/// Inner compression logic (no caching)
fn rtk_compress_inner(text: &str, command: Option<&str>) -> RtkCompressResult {
    use command_detector::{detect_command_type, extract_intent_keywords};
    use filter::match_filter;
    use line_filter::apply_line_filter;
    use dedup::deduplicate_lines;
    use truncate::char_truncate_with_intent;
    use grouping::apply_grouping;

    // Detect command type
    let detection = detect_command_type(text, command);
    let command_type = detection.command_type.clone();

    // Short-output passthrough: tiny outputs from unrecognized commands (control
    // commands like cd/echo/mkdir, empty polls, single-line results) have nothing
    // worth compressing. Returning them verbatim avoids the generic pipeline
    // accidentally folding/truncating them and removes a large slice of
    // unrecognized commands from the compression path. Recognized commands still
    // run their (cheap) filter so their contract is preserved.
    const PASSTHROUGH_MAX_LINES: usize = 30;
    const PASSTHROUGH_MAX_CHARS: usize = 2000;
    if command_type == "unknown"
        && text.len() <= PASSTHROUGH_MAX_CHARS
        && text.bytes().filter(|&b| b == b'\n').count() < PASSTHROUGH_MAX_LINES
    {
        // Even verbatim passthrough should drop ANSI escape codes: they carry no
        // information for the agent and only waste tokens.
        return RtkCompressResult {
            text: line_filter::strip_ansi(text),
            filter_id: None,
            command_type,
            grouping_applied: false,
        };
    }

    let intent_keywords = command
        .map(extract_intent_keywords)
        .unwrap_or_default();
    let intent_refs: Vec<&str> = intent_keywords.iter().map(|s| s.as_str()).collect();

    // Match filter
    let matched = match_filter(text, detection.command.as_deref());
    let filter_id = matched.as_ref().map(|f| f.id.clone());

    let mut result = text.to_string();

    if let Some(ref f) = matched {
        let filtered = apply_line_filter(&result, f, &intent_keywords);
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

    // Deduplicate (threshold=3). High-fidelity reads must keep every line: collapsing
    // repeated disassembly/source/inspection lines would break line/offset alignment.
    const HIGH_FIDELITY: [&str; 5] =
        ["file-read", "binary-read", "structured-read", "macos-inspect", "json-output"];
    if !HIGH_FIDELITY.contains(&command_type.as_str()) {
        let deduped = deduplicate_lines(&result, 3);
        result = deduped.text;
    }

    // Global fallback: char budget only. Line limits are owned by per-filter rules.
    let truncated = char_truncate_with_intent(&result, *GLOBAL_MAX_CHARS, &intent_refs);
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
            return cached.text;
        }
    }
    
    // Compute
    let result = caveman::compress(text, "user", level);
    
    // Cache result
    if let Some(key) = cache_key {
        cache::insert(key, cache::CachedCompressionResult {
            text: result.clone(),
            filter_id: None,
            command_type: "caveman".to_string(),
            grouping_applied: false,
        });
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

    #[test]
    fn test_file_read_output_preserves_middle_context() {
        let mut lines = vec!["$ cat src/main.rs".to_string()];
        for i in 1..=180 {
            lines.push(format!("fn line_{i:03}() {{ println!(\"value {i}\"); }}"));
        }

        let input = lines.join("\n");
        let result = rtk_compress_full(&input);

        assert!(result.text.contains("line_090"), "file reads should preserve middle context");
        assert_eq!(
            result.text.lines().count(),
            181,
            "file reads should keep all lines when under char budget"
        );
    }

    #[test]
    fn test_file_list_output_keeps_paths_instead_of_directory_only_summary() {
        let mut lines = vec!["$ find . -type f".to_string()];
        for i in 1..=220 {
            lines.push(format!("./src/dir{}/file{i:03}.rs", i % 12));
        }

        let input = lines.join("\n");
        let result = rtk_compress_full(&input);

        assert!(!result.text.contains("[rtk:grouped"), "medium file lists should keep real paths");
        assert!(result.text.contains("file120.rs"), "file lists should preserve middle paths");
        assert_eq!(
            result.text.lines().count(),
            221,
            "file lists should keep all paths when under char budget"
        );
    }

    #[test]
    fn test_global_char_fallback_does_not_retruncate_lines() {
        let mut lines = vec!["$ cat src/main.rs".to_string()];
        for i in 1..=180 {
            lines.push(format!("fn line_{i:03}() {{}}"));
        }
        let input = lines.join("\n");
        assert!(input.len() < 12_000, "fixture should stay under char budget");

        let result = rtk_compress_full(&input);
        assert_eq!(
            result.text.lines().count(),
            181,
            "file-read filter keeps lines; global layer must not drop them again"
        );
        assert!(
            !result.text.contains("[rtk:truncated"),
            "no truncation markers expected under char budget"
        );
    }

    #[test]
    fn test_global_char_fallback_applies_over_char_budget() {
        let input = (0..30)
            .map(|i| format!("line_{i:02}: {}", "x".repeat(500)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(input.len() > 12_000, "fixture should exceed char budget");

        let result = rtk_compress_full(&input);
        assert!(result.text.len() <= 12_000);
        assert!(result.text.contains("[rtk:truncated by chars]"));
    }

    #[test]
    fn test_python_script_routes_to_python_filter_and_keeps_traceback() {
        let text = "Traceback (most recent call last):\n  File \"foo.py\", line 10, in <module>\n    run()\nValueError: boom\n";
        let result = rtk_compress_with_command(text, Some("python foo.py"));
        assert_eq!(result.command_type, "python-script");
        assert_eq!(result.filter_id.as_deref(), Some("python-script"));
        assert!(result.text.contains("Traceback"), "python tracebacks must be preserved");
        assert!(result.text.contains("ValueError: boom"), "python errors must be preserved");
    }

    #[test]
    fn test_binary_read_otool_is_high_fidelity() {
        let mut lines = Vec::new();
        for i in 1..=300 {
            lines.push(format!("000000010000{i:04x}\tmov x{}, x{}", i % 30, (i + 1) % 30));
        }
        let input = lines.join("\n");
        assert!(input.len() < 12_000, "fixture should stay under char budget");

        let result = rtk_compress_with_command(&input, Some("otool -tV bin"));
        assert_eq!(result.command_type, "binary-read");
        assert_eq!(result.filter_id.as_deref(), Some("binary-read"));
        assert_eq!(
            result.text.lines().count(),
            300,
            "binary inspection output must not be line-capped under char budget"
        );
    }

    #[test]
    fn test_shell_grep_no_longer_caps_match_lines() {
        let mut lines = Vec::new();
        for i in 1..=200 {
            lines.push(format!("src/f{i:03}.rs:{i}:target_sym"));
        }
        let input = lines.join("\n");
        assert!(input.len() < 12_000, "fixture should stay under char budget");

        let result = rtk_compress_with_command(&input, Some("rg target_sym"));
        assert_eq!(result.command_type, "shell-grep");
        assert!(result.text.contains("f100.rs"), "middle matches must survive");
        assert_eq!(
            result.text.lines().count(),
            200,
            "grep matches must not be line-capped under char budget"
        );
    }

    #[test]
    fn test_sed_print_script_routes_to_file_read_filter() {
        let mut lines = vec!["line one".to_string()];
        for i in 2..=120 {
            lines.push(format!("fn line_{i:03}() {{}}"));
        }
        let input = lines.join("\n");
        let result = rtk_compress_with_command(&input, Some("sed '1,240p' src/mod.rs"));
        assert_eq!(result.command_type, "file-read");
        assert_eq!(result.filter_id.as_deref(), Some("file-read"));
        assert!(result.text.contains("line_060"));
    }

    #[test]
    fn test_rg_intent_keywords_survive_char_budget() {
        let mut lines = Vec::new();
        for i in 1..=400 {
            if i == 200 {
                lines.push(format!(
                    "src/hot.rs:{i}:needle_symbol appears here {}",
                    "x".repeat(120)
                ));
            } else {
                lines.push(format!("src/cold{i:03}.rs:{i}:unrelated {}", "y".repeat(120)));
            }
        }
        let input = lines.join("\n");
        assert!(input.len() > 12_000, "fixture should exceed char budget");

        let result = rtk_compress_with_command(&input, Some("rg needle_symbol src"));
        assert!(
            result.text.contains("needle_symbol"),
            "intent keyword lines must survive char-budget truncation"
        );
    }

    #[test]
    fn test_structured_read_output_preserves_middle_keys() {
        let mut lines = vec!["$ yq . config.yml".to_string()];
        for i in 1..=90 {
            lines.push(format!("item_{i}:"));
            lines.push(format!("  name: example-{i}"));
            lines.push("  enabled: true".to_string());
        }

        let input = lines.join("\n");
        let result = rtk_compress_full(&input);

        assert!(result.text.contains("item_45:"), "structured reads should preserve middle keys");
    }

    #[test]
    fn test_short_unknown_output_passes_through_verbatim() {
        let result = rtk_compress_with_command("done\n", Some("echo done"));
        assert_eq!(result.command_type, "unknown");
        assert_eq!(result.filter_id, None);
        assert_eq!(result.text, "done\n", "tiny unknown output must be returned verbatim");
        assert!(!result.grouping_applied);
    }

    #[test]
    fn test_short_unknown_output_strips_ansi() {
        let result = rtk_compress_with_command("\u{1b}[32mdone\u{1b}[0m\n", Some("echo done"));
        assert_eq!(result.command_type, "unknown");
        assert_eq!(result.filter_id, None);
        assert_eq!(result.text, "done\n", "passthrough must still strip ANSI escape codes");
    }

    #[test]
    fn test_compile_build_drops_progress_keeps_error_context() {
        let mut lines = Vec::new();
        for i in 1..=50 {
            lines.push(format!("   Compiling crate_{i} v0.{i}.0"));
        }
        lines.push("error[E0308]: mismatched types".to_string());
        lines.push("  --> src/main.rs:10:5".to_string());
        lines.push("   |".to_string());
        lines.push("10 |     foo".to_string());
        lines.push("   |     ^^^ expected i32, found &str".to_string());
        lines.push("    Finished dev [unoptimized] target(s) in 2.3s".to_string());
        let input = lines.join("\n");

        let result = rtk_compress_with_command(&input, Some("cargo build"));
        assert_eq!(result.command_type, "cargo-build");
        assert_eq!(result.filter_id.as_deref(), Some("compile-build"));
        assert!(result.text.contains("error[E0308]"), "errors must be preserved");
        assert!(
            result.text.contains("expected i32, found &str"),
            "error context block must be preserved"
        );
        assert!(result.text.contains("Finished"), "final status must be preserved");
        assert!(
            !result.text.contains("Compiling crate_25"),
            "Compiling progress noise should be dropped"
        );
    }

    #[test]
    fn test_macos_inspect_is_high_fidelity() {
        let mut lines = Vec::new();
        for i in 1..=120 {
            lines.push(format!("kern.sysctl.node_{i:03}: value_{i}"));
        }
        let input = lines.join("\n");
        assert!(input.len() < 12_000, "fixture should stay under char budget");

        let result = rtk_compress_with_command(&input, Some("sysctl -a"));
        assert_eq!(result.command_type, "macos-inspect");
        assert_eq!(result.filter_id.as_deref(), Some("macos-inspect"));
        assert_eq!(
            result.text.lines().count(),
            120,
            "system inspection output must not be line-capped under char budget"
        );
    }

    #[test]
    fn test_generic_output_preserves_errors_and_strips_ansi() {
        let mut lines = Vec::new();
        for i in 1..=250 {
            if i == 130 {
                lines.push("\u{1b}[31merror: the severe breakage is here\u{1b}[0m".to_string());
            } else {
                lines.push(format!("step {i} running some unrecognized tool output"));
            }
        }
        let input = lines.join("\n");

        assert!(input.len() < 12_000, "fixture should stay under char budget");

        let result = rtk_compress_with_command(&input, Some("./mytool --run"));
        assert_eq!(result.command_type, "unknown");
        assert!(
            result.text.contains("error: the severe breakage is here"),
            "generic fallback must preserve error lines"
        );
        assert!(!result.text.contains('\u{1b}'), "generic fallback must strip ANSI");
        // Scheme A: generic-output no longer line-caps under the char budget, so
        // nothing is truncated and the whole (ANSI-stripped) body survives.
        assert!(
            !result.text.contains("[rtk:truncated"),
            "generic output must not be line-capped under char budget"
        );
        assert!(
            result.text.contains("step 1 ") && result.text.contains("step 250 "),
            "both head and tail lines must survive when under char budget"
        );
    }

    #[test]
    fn test_high_fidelity_reads_skip_dedup() {
        let mut lines: Vec<String> = (1..=40).map(|i| format!("0x{i:08x}\tnop")).collect();
        for _ in 0..5 {
            lines.push("0x00000000\tnop".to_string());
        }
        for i in 41..=80 {
            lines.push(format!("0x{i:08x}\tret"));
        }
        let input = lines.join("\n");

        let result = rtk_compress_with_command(&input, Some("otool -tV bin"));
        assert_eq!(result.command_type, "binary-read");
        assert!(
            !result.text.contains("[rtk:dropped"),
            "high-fidelity reads must not collapse repeated lines"
        );
        assert_eq!(result.text.lines().count(), 85);
    }

    #[test]
    fn test_dedup_uses_single_marker() {
        let mut lines: Vec<String> = (1..=35).map(|i| format!("unique line {i}")).collect();
        for _ in 0..5 {
            lines.push("REPEATED".to_string());
        }
        let input = lines.join("\n");

        let result = rtk_compress_with_command(&input, Some("./noise"));
        assert!(
            result.text.contains("[rtk:dropped 4 repeated lines]"),
            "dedup should emit a single dropped-lines marker"
        );
        assert!(
            !result.text.contains("[line repeated"),
            "the redundant second marker must be removed"
        );
    }

    #[test]
    fn test_git_log_patch_keeps_diff_lines() {
        let input = "commit abc1234def5678 (HEAD)\nAuthor: Foo <foo@x>\nDate: today\n\n    Fix the bug\n\ndiff --git a/src/main.rs b/src/main.rs\n@@ -1,3 +1,4 @@\n-old line\n+new line\n";
        let result = rtk_compress_with_command(input, Some("git log -p"));
        assert_eq!(result.command_type, "git-log");
        assert!(result.text.contains("+new line"), "git log -p must keep patch additions");
        assert!(result.text.contains("@@ -1,3 +1,4 @@"), "git log -p must keep hunk headers");
    }

    #[test]
    fn test_curl_keeps_plaintext_body() {
        let input = "HTTP/1.1 200 OK\nContent-Type: text/plain\n\nthis is the plain text response body\nsecond body line\n";
        let result = rtk_compress_with_command(input, Some("curl -i https://example.com"));
        assert_eq!(result.command_type, "curl");
        assert!(
            result.text.contains("plain text response body"),
            "curl must not whitelist-delete unpredictable plain-text bodies"
        );
    }
}
