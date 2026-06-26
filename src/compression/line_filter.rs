use once_cell::sync::Lazy;
use regex::Regex;

use super::filter::Filter;
use super::truncate::smart_truncate;

pub struct LineFilterResult {
    pub text: String,
    pub stripped_lines: usize,
}

pub(crate) fn strip_ansi(text: &str) -> String {
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
    let char_count = line.chars().count();
    if char_count <= max_chars {
        return line.to_string();
    }
    if max_chars <= 3 {
        return line.chars().take(max_chars).collect();
    }
    let truncated: String = line.chars().take(max_chars - 3).collect();
    format!("{}...", truncated)
}

pub fn apply_line_filter(
    text: &str,
    filter: &Filter,
    intent_keywords: &[String],
) -> LineFilterResult {
    // Use pre-compiled regexes from the Filter struct — no HashMap/RwLock overhead
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

    // Replace rules (use pre-compiled)
    for (i, rule) in filter.replace.iter().enumerate() {
        if let Some(ref re) = filter.compiled_replace.get(i).and_then(|r| r.as_ref()) {
            lines = lines.iter().map(|l| re.replace_all(l, rule.replacement.as_str()).to_string()).collect();
        }
    }

    // Match-output short-circuit (use pre-compiled)
    if !filter.match_output.is_empty() {
        let blob = lines.join("\n");
        for (i, rule) in filter.match_output.iter().enumerate() {
            if let Some((ref pat_re, ref unless_re)) = filter.compiled_match_output.get(i) {
                if !pat_re.is_match(&blob) {
                    continue;
                }
                if let Some(ref unless) = unless_re {
                    if unless.is_match(&blob) {
                        continue;
                    }
                }
                return LineFilterResult {
                    text: rule.message.clone(),
                    stripped_lines: original_line_count.saturating_sub(1),
                };
            }
        }
    }

    // Strip (drop) patterns (use pre-compiled)
    if !filter.compiled_strip.is_empty() {
        lines.retain(|line| !filter.compiled_strip.iter().any(|p| p.is_match(line)));
    }

    // Keep (include) patterns — only if something matches (use pre-compiled)
    if !filter.compiled_keep.is_empty() {
        let kept: Vec<String> = lines.iter()
            .filter(|line| filter.compiled_keep.iter().any(|p| p.is_match(line)))
            .cloned()
            .collect();
        if !kept.is_empty() {
            lines = kept;
        }
    }

    // Collapse patterns — deduplicate matching lines (use pre-compiled)
    if !filter.compiled_collapse.is_empty() {
        let mut seen = std::collections::HashSet::new();
        lines.retain(|line| {
            if !filter.compiled_collapse.iter().any(|p| p.is_match(line)) {
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

    // Smart truncate with head/tail/priority (use pre-compiled)
    let priority_refs: Vec<&Regex> = filter.compiled_priority.iter().collect();
    let intent_refs: Vec<&str> = intent_keywords.iter().map(|s| s.as_str()).collect();
    let truncated = smart_truncate(
        &lines.join("\n"),
        filter.max_lines,
        0, // no char limit at filter level
        filter.preserve_head,
        filter.preserve_tail,
        &priority_refs,
        &intent_refs,
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
