use std::sync::RwLock;
use std::collections::HashMap;
use once_cell::sync::Lazy;
use regex::Regex;

use super::filter::Filter;
use super::truncate::smart_truncate;

pub struct LineFilterResult {
    pub text: String,
    pub stripped_lines: usize,
}

// ─── Regex cache ───────────────────────────────────────────────────────────

static REGEX_CACHE: Lazy<RwLock<HashMap<String, Option<Regex>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

fn cached_regex(pattern: &str, flags: &str) -> Option<Regex> {
    let key = format!("{}::{}", pattern, flags);
    {
        let cache = REGEX_CACHE.read().unwrap();
        if let Some(entry) = cache.get(&key) {
            return entry.clone();
        }
    }
    let result = Regex::new(&format!("(?{}){}", flags, pattern)).ok();
    let mut cache = REGEX_CACHE.write().unwrap();
    cache.insert(key, result.clone());
    result
}

fn compile_patterns(patterns: &[String], flags: &str) -> Vec<Regex> {
    patterns
        .iter()
        .filter_map(|p| cached_regex(p, flags))
        .collect()
}

fn strip_ansi(text: &str) -> String {
    static ANSI_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\u{001b}\[[0-?]*[ -/]*[@-~]").unwrap()
    });
    ANSI_RE.replace_all(text, "").to_string()
}

fn normalize_stderr_prefix(line: &str) -> String {
    static STDERR_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?i)^\s*(?:stderr|err)\s*(?:\||:)\s*").unwrap()
    });
    STDERR_RE.replace(line, "").to_string()
}

fn truncate_unicode_safe(line: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return line.to_string();
    }
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= max_chars {
        return line.to_string();
    }
    if max_chars <= 3 {
        return chars[..max_chars].iter().collect();
    }
    let truncated: String = chars[..max_chars - 3].iter().collect();
    format!("{}...", truncated)
}

pub fn apply_line_filter(text: &str, filter: &Filter) -> LineFilterResult {
    let strip_patterns = compile_patterns(&filter.strip_patterns, "i");
    let keep_patterns = compile_patterns(&filter.keep_patterns, "i");
    let collapse_patterns = compile_patterns(&filter.collapse_patterns, "i");
    let priority_patterns = compile_patterns(&filter.priority_patterns, "i");

    let mut lines: Vec<String> = text.split("\r\n").flat_map(|l| l.split('\n'))
        .map(|s| s.to_string())
        .collect();
    let original_line_count = lines.len();

    // Strip ANSI
    if filter.strip_ansi {
        lines = lines.iter().map(|l| strip_ansi(l)).collect();
    }

    // Filter stderr prefixes
    if filter.filter_stderr {
        lines = lines.iter().map(|l| normalize_stderr_prefix(l)).collect();
    }

    // Replace rules
    for rule in &filter.replace {
        if let Some(re) = cached_regex(&rule.pattern, "g") {
            lines = lines.iter().map(|l| re.replace_all(l, rule.replacement.as_str()).to_string()).collect();
        }
    }

    // Match-output short-circuit
    if !filter.match_output.is_empty() {
        let blob = lines.join("\n");
        for rule in &filter.match_output {
            let pattern_re = match cached_regex(&rule.pattern, "im") {
                Some(re) => re,
                None => continue,
            };
            if !pattern_re.is_match(&blob) {
                continue;
            }
            if let Some(ref unless_str) = rule.unless {
                if let Some(unless_re) = cached_regex(unless_str, "im") {
                    if unless_re.is_match(&blob) {
                        continue;
                    }
                }
            }
            return LineFilterResult {
                text: rule.message.clone(),
                stripped_lines: original_line_count.saturating_sub(1),
            };
        }
    }

    // Strip (drop) patterns
    if !strip_patterns.is_empty() {
        lines.retain(|line| !strip_patterns.iter().any(|p| p.is_match(line)));
    }

    // Keep (include) patterns — only if something matches
    if !keep_patterns.is_empty() {
        let kept: Vec<String> = lines.iter()
            .filter(|line| keep_patterns.iter().any(|p| p.is_match(line)))
            .cloned()
            .collect();
        if !kept.is_empty() {
            lines = kept;
        }
    }

    // Collapse patterns — deduplicate matching lines
    if !collapse_patterns.is_empty() {
        let mut seen = std::collections::HashSet::new();
        lines.retain(|line| {
            if !collapse_patterns.iter().any(|p| p.is_match(line)) {
                return true;
            }
            let key = line.trim().to_string();
            seen.insert(key)
        });
    }

    // Truncate individual lines
    if filter.truncate_line_at > 0 {
        lines = lines.iter()
            .map(|l| truncate_unicode_safe(l, filter.truncate_line_at))
            .collect();
    }

    // Smart truncate with head/tail/priority
    let truncated = smart_truncate(
        &lines.join("\n"),
        filter.max_lines,
        0, // no char limit at filter level
        filter.preserve_head,
        filter.preserve_tail,
        &priority_patterns,
    );

    let output = if truncated.text.trim().is_empty() && !filter.on_empty.is_empty() {
        filter.on_empty.clone()
    } else {
        truncated.text
    };

    let output_line_count = output.split('\n').count();
    LineFilterResult {
        text: output,
        stripped_lines: original_line_count.saturating_sub(output_line_count),
    }
}
