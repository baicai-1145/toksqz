use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use std::sync::RwLock;
use std::path::Path;

/// Internal filter representation (after schema transformation)
pub struct Filter {
    pub id: String,
    pub command_types: Vec<String>,   // from match.outputTypes
    pub command_patterns: Vec<String>, // from match.commands (regex)
    pub match_patterns: Vec<String>,  // from match.patterns (regex)
    pub priority: i32,
    pub strip_ansi: bool,
    pub replace: Vec<ReplaceRule>,
    pub match_output: Vec<MatchOutputRule>,
    pub strip_patterns: Vec<String>,   // from rules.dropPatterns
    pub keep_patterns: Vec<String>,    // from rules.includePatterns
    pub collapse_patterns: Vec<String>,
    pub priority_patterns: Vec<String>, // from preserve.errorPatterns + preserve.summaryPatterns
    pub truncate_line_at: usize,
    pub on_empty: String,
    pub filter_stderr: bool,
    pub deduplicate: bool,
    pub max_lines: usize,
    pub preserve_head: usize,
    pub preserve_tail: usize,
    pub source: String,         // "builtin" or "custom"
    pub source_priority: i32,   // custom=100, builtin=50 (higher wins)
    // Pre-compiled regexes (populated after loading)
    pub compiled_strip: Vec<Regex>,
    pub compiled_keep: Vec<Regex>,
    pub compiled_collapse: Vec<Regex>,
    pub compiled_priority: Vec<Regex>,
    pub compiled_replace: Vec<Option<Regex>>,
    pub compiled_match_output: Vec<(Regex, Option<Regex>)>,
}

pub struct ReplaceRule {
    pub pattern: String,
    pub replacement: String,
}

pub struct MatchOutputRule {
    pub pattern: String,
    pub message: String,
    pub unless: Option<String>,
}

// ─── Raw JSON types (matching the filter pack format) ──────────────────────

#[derive(Deserialize)]
struct RawFilter {
    id: String,
    #[serde(alias = "label")]
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    category: String,
    #[serde(default = "default_priority")]
    priority: i32,
    #[serde(rename = "match")]
    #[serde(default)]
    r#match: RawMatch,
    #[serde(default)]
    rules: RawRules,
    #[serde(default)]
    preserve: RawPreserve,
    // Legacy format fields
    #[serde(default)]
    commandTypes: Vec<String>,
    #[serde(default)]
    stripPatterns: Vec<String>,
    #[serde(default)]
    keepPatterns: Vec<String>,
    #[serde(default)]
    collapsePatterns: Vec<String>,
    #[serde(default)]
    stripAnsi: Option<bool>,
    #[serde(default)]
    maxLines: Option<usize>,
    #[serde(default)]
    preserveHead: Option<usize>,
    #[serde(default)]
    preserveTail: Option<usize>,
}

fn default_priority() -> i32 { 50 }

#[derive(Deserialize, Default)]
struct RawMatch {
    #[serde(default)]
    outputTypes: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    patterns: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawRules {
    #[serde(default)]
    stripAnsi: bool,
    #[serde(default)]
    replace: Vec<RawReplaceRule>,
    #[serde(default)]
    matchOutput: Vec<RawMatchOutputRule>,
    #[serde(default)]
    dropPatterns: Vec<String>,
    #[serde(default)]
    includePatterns: Vec<String>,
    #[serde(default)]
    collapsePatterns: Vec<String>,
    #[serde(default)]
    deduplicate: bool,
    #[serde(default)]
    truncateLineAt: usize,
    #[serde(default)]
    maxLines: usize,
    #[serde(default = "default_head")]
    headLines: usize,
    #[serde(default = "default_tail")]
    tailLines: usize,
    #[serde(default)]
    onEmpty: String,
    #[serde(default)]
    filterStderr: bool,
}

fn default_head() -> usize { 20 }
fn default_tail() -> usize { 20 }

#[derive(Deserialize)]
struct RawReplaceRule {
    pattern: String,
    replacement: String,
}

#[derive(Deserialize)]
struct RawMatchOutputRule {
    pattern: String,
    message: String,
    #[serde(default)]
    unless: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawPreserve {
    #[serde(default)]
    errorPatterns: Vec<String>,
    #[serde(default)]
    summaryPatterns: Vec<String>,
}

// ─── Filter loading and transformation ─────────────────────────────────────

static BUILTIN_FILTERS: Lazy<Vec<Filter>> = Lazy::new(|| {
    let raw_json = include_str!("../../assets/all_filters.json");
    parse_raw_filters(raw_json, "builtin")
});

static CUSTOM_FILTERS: Lazy<RwLock<Vec<Filter>>> = Lazy::new(|| RwLock::new(Vec::new()));

/// Combined filter registry: custom filters first (higher priority), then built-in.
static COMBINED_FILTERS: Lazy<RwLock<Vec<Filter>>> = Lazy::new(|| {
    let mut combined = Vec::new();
    if let Ok(custom) = CUSTOM_FILTERS.read() {
        combined.extend(custom.iter().map(|f| clone_filter(f)));
    }
    combined.extend(BUILTIN_FILTERS.iter().map(|f| clone_filter(f)));
    // Sort: custom filters (higher source_priority) first, then by priority desc, then id asc
    combined.sort_by(|a, b| {
        b.source_priority.cmp(&a.source_priority)
            .then(b.priority.cmp(&a.priority))
            .then(a.id.cmp(&b.id))
    });
    RwLock::new(combined)
});

/// Pre-compiled regex cache for filter command_patterns (index-aligned with COMBINED_FILTERS)
static COMPILED_FILTER_CMD_PATTERNS: Lazy<RwLock<Vec<Vec<Regex>>>> = Lazy::new(|| {
    let filters = COMBINED_FILTERS.read().unwrap();
    let compiled: Vec<Vec<Regex>> = filters.iter().map(|f| {
        f.command_patterns.iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    }).collect();
    RwLock::new(compiled)
});

/// Pre-compiled regex cache for filter match_patterns (index-aligned with COMBINED_FILTERS)
static COMPILED_FILTER_MATCH_PATTERNS: Lazy<RwLock<Vec<Vec<Regex>>>> = Lazy::new(|| {
    let filters = COMBINED_FILTERS.read().unwrap();
    let compiled: Vec<Vec<Regex>> = filters.iter().map(|f| {
        f.match_patterns.iter()
            .filter_map(|p| Regex::new(&format!("(?im){}", p)).ok())
            .collect()
    }).collect();
    RwLock::new(compiled)
});

fn clone_filter(f: &Filter) -> Filter {
    Filter {
        id: f.id.clone(),
        command_types: f.command_types.clone(),
        command_patterns: f.command_patterns.clone(),
        match_patterns: f.match_patterns.clone(),
        priority: f.priority,
        strip_ansi: f.strip_ansi,
        replace: f.replace.iter().map(|r| ReplaceRule { pattern: r.pattern.clone(), replacement: r.replacement.clone() }).collect(),
        match_output: f.match_output.iter().map(|r| MatchOutputRule { pattern: r.pattern.clone(), message: r.message.clone(), unless: r.unless.clone() }).collect(),
        strip_patterns: f.strip_patterns.clone(),
        keep_patterns: f.keep_patterns.clone(),
        collapse_patterns: f.collapse_patterns.clone(),
        priority_patterns: f.priority_patterns.clone(),
        truncate_line_at: f.truncate_line_at,
        on_empty: f.on_empty.clone(),
        filter_stderr: f.filter_stderr,
        deduplicate: f.deduplicate,
        max_lines: f.max_lines,
        preserve_head: f.preserve_head,
        preserve_tail: f.preserve_tail,
        source: f.source.clone(),
        source_priority: f.source_priority,
        compiled_strip: f.compiled_strip.clone(),
        compiled_keep: f.compiled_keep.clone(),
        compiled_collapse: f.compiled_collapse.clone(),
        compiled_priority: f.compiled_priority.clone(),
        compiled_replace: f.compiled_replace.clone(),
        compiled_match_output: f.compiled_match_output.clone(),
    }
}

/// Compile all regex patterns in a Filter into pre-compiled fields
fn compile_filter_regexes(f: &mut Filter) {
    f.compiled_strip = f.strip_patterns.iter()
        .filter_map(|p| Regex::new(&format!("(?i){}", p)).ok())
        .collect();
    f.compiled_keep = f.keep_patterns.iter()
        .filter_map(|p| Regex::new(&format!("(?i){}", p)).ok())
        .collect();
    f.compiled_collapse = f.collapse_patterns.iter()
        .filter_map(|p| Regex::new(&format!("(?i){}", p)).ok())
        .collect();
    f.compiled_priority = f.priority_patterns.iter()
        .filter_map(|p| Regex::new(&format!("(?i){}", p)).ok())
        .collect();
    f.compiled_replace = f.replace.iter()
        .map(|r| Regex::new(&r.pattern).ok())
        .collect();
    f.compiled_match_output = f.match_output.iter()
        .map(|r| {
            let pat = Regex::new(&format!("(?im){}", r.pattern)).ok();
            let unless = r.unless.as_ref().and_then(|u| Regex::new(&format!("(?im){}", u)).ok());
            (pat.unwrap_or_else(|| Regex::new("(?!)" ).unwrap()), unless)
        })
        .collect();
}

fn parse_raw_filters(raw_json: &str, source: &str) -> Vec<Filter> {
    let raw_filters: Vec<RawFilter> = match serde_json::from_str(raw_json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[toksqz] Failed to parse filters from {}: {}", source, e);
            return Vec::new();
        }
    };

    let source_priority = match source {
        "custom" => 100,
        _ => 50,
    };

    let mut filters: Vec<Filter> = raw_filters.into_iter().map(|raw| {
        // Detect if this is canonical (pack) format or legacy format
        let is_canonical = !raw.r#match.commands.is_empty()
            || !raw.r#match.patterns.is_empty()
            || !raw.r#match.outputTypes.is_empty()
            || !raw.rules.dropPatterns.is_empty()
            || !raw.rules.includePatterns.is_empty()
            || raw.rules.stripAnsi;

        let mut f = if is_canonical {
            let preserve_patterns: Vec<String> = [
                raw.preserve.errorPatterns.as_slice(),
                raw.preserve.summaryPatterns.as_slice(),
            ].concat();

            Filter {
                id: raw.id,
                command_types: raw.r#match.outputTypes,
                command_patterns: raw.r#match.commands,
                match_patterns: raw.r#match.patterns,
                priority: raw.priority,
                strip_ansi: raw.rules.stripAnsi,
                replace: raw.rules.replace.into_iter()
                    .map(|r| ReplaceRule { pattern: r.pattern, replacement: r.replacement })
                    .collect(),
                match_output: raw.rules.matchOutput.into_iter()
                    .map(|r| MatchOutputRule { pattern: r.pattern, message: r.message, unless: r.unless })
                    .collect(),
                strip_patterns: raw.rules.dropPatterns,
                keep_patterns: raw.rules.includePatterns,
                collapse_patterns: raw.rules.collapsePatterns,
                priority_patterns: preserve_patterns,
                truncate_line_at: raw.rules.truncateLineAt,
                on_empty: raw.rules.onEmpty,
                filter_stderr: raw.rules.filterStderr,
                deduplicate: raw.rules.deduplicate,
                max_lines: raw.rules.maxLines,
                preserve_head: raw.rules.headLines,
                preserve_tail: raw.rules.tailLines,
                source: source.to_string(),
                source_priority,
                compiled_strip: vec![],
                compiled_keep: vec![],
                compiled_collapse: vec![],
                compiled_priority: vec![],
                compiled_replace: vec![],
                compiled_match_output: vec![],
            }
        } else {
            // Legacy format
            let preserve_patterns: Vec<String> = [
                raw.preserve.errorPatterns.as_slice(),
                raw.preserve.summaryPatterns.as_slice(),
            ].concat();

            Filter {
                id: raw.id,
                command_types: raw.commandTypes,
                command_patterns: vec![],
                match_patterns: vec![],
                priority: raw.priority,
                strip_ansi: raw.stripAnsi.unwrap_or(false),
                replace: vec![],
                match_output: vec![],
                strip_patterns: raw.stripPatterns,
                keep_patterns: raw.keepPatterns,
                collapse_patterns: raw.collapsePatterns,
                priority_patterns: preserve_patterns,
                truncate_line_at: 0,
                on_empty: String::new(),
                filter_stderr: false,
                deduplicate: false,
                max_lines: raw.maxLines.unwrap_or(0),
                preserve_head: raw.preserveHead.unwrap_or(20),
                preserve_tail: raw.preserveTail.unwrap_or(20),
                source: source.to_string(),
                source_priority,
                compiled_strip: vec![],
                compiled_keep: vec![],
                compiled_collapse: vec![],
                compiled_priority: vec![],
                compiled_replace: vec![],
                compiled_match_output: vec![],
            }
        };
        
        // Pre-compile all regex patterns
        compile_filter_regexes(&mut f);
        f
    }).collect();

    // Sort by priority (desc), then id (asc)
    filters.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    filters
}

pub fn load_filters() -> usize {
    Lazy::force(&BUILTIN_FILTERS);
    // Load custom filters from TOKSQZ_FILTERS_DIR
    load_custom_filters();
    // Force rebuild combined filters
    Lazy::force(&COMBINED_FILTERS);
    COMBINED_FILTERS.read().map(|f| f.len()).unwrap_or(0)
}

/// Load custom filters from the directory specified by TOKSQZ_FILTERS_DIR env var.
fn load_custom_filters() {
    let dir = match std::env::var("TOKSQZ_FILTERS_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };

    let path = Path::new(&dir);
    if !path.is_dir() {
        eprintln!("[toksqz] TOKSQZ_FILTERS_DIR '{}' is not a directory", dir);
        return;
    }

    let mut custom_filters = Vec::new();
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[toksqz] Cannot read filters dir '{}': {}", dir, e);
            return;
        }
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&entry_path) {
            Ok(content) => {
                let parsed = parse_raw_filters(&content, "custom");
                let fname = entry_path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
                println!("[toksqz] Loaded {} custom filter(s) from {}", parsed.len(), fname);
                custom_filters.extend(parsed);
            }
            Err(e) => {
                eprintln!("[toksqz] Cannot read filter file '{}': {}", entry_path.display(), e);
            }
        }
    }

    if !custom_filters.is_empty() {
        if let Ok(mut cf) = CUSTOM_FILTERS.write() {
            *cf = custom_filters;
        }
        // Invalidate combined cache so it rebuilds
        if let Ok(mut combined) = COMBINED_FILTERS.write() {
            combined.clear();
            if let Ok(custom) = CUSTOM_FILTERS.read() {
                combined.extend(custom.iter().map(|f| clone_filter(f)));
            }
            combined.extend(BUILTIN_FILTERS.iter().map(|f| clone_filter(f)));
            combined.sort_by(|a, b| {
                b.source_priority.cmp(&a.source_priority)
                    .then(b.priority.cmp(&a.priority))
                    .then(a.id.cmp(&b.id))
            });
        }
    }
}

/// Match filter: commandTypes → commandPatterns → matchPatterns → generic fallback
/// Returns (matched filter, filter_id) or None.
pub fn match_filter(text: &str, command: Option<&str>) -> Option<&'static Filter> {
    match_filter_dyn(text, command)
        .and_then(|idx| {
            COMBINED_FILTERS.read().ok().and_then(|filters| {
                // SAFETY: We return a reference into the Lazy static which lives for 'static
                let ptr = &filters[idx] as *const Filter;
                Some(unsafe { &*ptr })
            })
        })
}

/// Dynamic match returning the index into COMBINED_FILTERS.
fn match_filter_dyn(text: &str, command: Option<&str>) -> Option<usize> {
    use crate::compression::command_detector::detect_command_type;

    let detection = detect_command_type(text, command);
    let detected_command = detection.command.as_deref().unwrap_or("");

    let filters = COMBINED_FILTERS.read().ok()?;
    let cmd_patterns = COMPILED_FILTER_CMD_PATTERNS.read().ok()?;
    let match_patterns = COMPILED_FILTER_MATCH_PATTERNS.read().ok()?;

    // 1. Match by command type (outputTypes)
    if let Some(idx) = filters.iter().position(|f| f.command_types.contains(&detection.command_type)) {
        return Some(idx);
    }

    // 2. Match by command patterns (regex on command string)
    if !detected_command.is_empty() {
        if let Some(idx) = filters.iter().enumerate().position(|(i, _f)| {
            cmd_patterns[i].iter().any(|re| re.is_match(detected_command))
        }) {
            return Some(idx);
        }
    }

    // 3. Match by match patterns (regex on full text)
    if let Some(idx) = filters.iter().enumerate().position(|(i, _f)| {
        match_patterns[i].iter().any(|re| re.is_match(text))
    }) {
        return Some(idx);
    }

    // 4. Fallback to generic-output
    filters.iter().position(|f| f.command_types.contains(&"generic-output".to_string()))
}

/// Return the filter ID for the currently matched filter (for stats headers).
#[allow(dead_code)]
pub fn matched_filter_id(text: &str, command: Option<&str>) -> Option<String> {
    match_filter_dyn(text, command).and_then(|idx| {
        COMBINED_FILTERS.read().ok().map(|filters| filters[idx].id.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_filters_load() {
        let filters = &*BUILTIN_FILTERS;
        assert!(!filters.is_empty(), "Built-in filters should not be empty");
        // Should have at least git-status, git-diff, etc.
        let ids: Vec<&str> = filters.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"git-status"));
        assert!(ids.contains(&"git-diff"));
        assert!(ids.contains(&"test-cargo"));
    }

    #[test]
    fn test_match_filter_git_status() {
        let text = "On branch main\nChanges not staged for commit:\n  modified: src/app.ts";
        let result = match_filter(text, Some("git status"));
        assert!(result.is_some());
        let f = result.unwrap();
        assert_eq!(f.id, "git-status");
    }

    #[test]
    fn test_match_filter_fallback_to_generic() {
        let text = "Some random text with no special format";
        let result = match_filter(text, None);
        // Should fallback to generic-output
        if let Some(f) = result {
            assert_eq!(f.id, "generic-output");
        }
    }

    #[test]
    fn test_parse_raw_filters_invalid_json() {
        let filters = parse_raw_filters("not valid json", "test");
        assert!(filters.is_empty());
    }

    #[test]
    fn test_filter_source_tracking() {
        let filters = &*BUILTIN_FILTERS;
        for f in filters {
            assert_eq!(f.source, "builtin");
            assert_eq!(f.source_priority, 50);
        }
    }
}
