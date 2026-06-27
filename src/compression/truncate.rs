use std::collections::BTreeSet;

use once_cell::sync::Lazy;
use regex::Regex;

pub struct TruncateResult {
    pub text: String,
    pub truncated: bool,
    pub dropped_lines: usize,
}

/// Lines matching these patterns are always kept when char-budget truncation runs.
static ERROR_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)error|warning|\bfail(?:ed|ure)?\b|panic|exception|traceback|fatal|\bdenied\b|not found",
    )
    .unwrap()
});

fn push_range(
    selected: &mut BTreeSet<usize>,
    start: usize,
    count: usize,
    len: usize,
) {
    let end = start.saturating_add(count).min(len);
    for i in start.min(len)..end {
        selected.insert(i);
    }
}

fn line_matches_intent(line: &str, intent_keywords: &[&str]) -> bool {
    if intent_keywords.is_empty() {
        return false;
    }
    let lower = line.to_lowercase();
    intent_keywords
        .iter()
        .any(|keyword| keyword.len() >= 2 && lower.contains(&keyword.to_lowercase()))
}

fn is_error_line(line: &str) -> bool {
    ERROR_LINE_RE.is_match(line)
}

fn build_selected_output(lines: &[&str], selected: &BTreeSet<usize>) -> String {
    let mut result_lines: Vec<String> = Vec::new();
    let mut previous: Option<usize> = None;
    for &i in selected {
        if let Some(prev) = previous {
            if i > prev + 1 {
                result_lines.push(format!("[rtk:truncated {} lines]", i - prev - 1));
            }
        }
        result_lines.push(lines[i].to_string());
        previous = Some(i);
    }
    result_lines.join("\n")
}

fn selection_for_head_tail(
    lines: &[&str],
    must_keep: &BTreeSet<usize>,
    head: usize,
    tail: usize,
) -> BTreeSet<usize> {
    let mut selected = must_keep.clone();
    for i in 0..head.min(lines.len()) {
        selected.insert(i);
    }
    let tail_start = lines.len().saturating_sub(tail);
    for i in tail_start..lines.len() {
        selected.insert(i);
    }
    selected
}

/// Grow head/tail while staying within `max_chars`, always keeping error + intent lines.
fn select_lines_within_budget(
    lines: &[&str],
    max_chars: usize,
    intent_keywords: &[&str],
) -> (BTreeSet<usize>, usize) {
    let mut must_keep: BTreeSet<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_error_line(line) || line_matches_intent(line, intent_keywords))
        .map(|(i, _)| i)
        .collect();

    let mut head = 0usize;
    let mut tail = 0usize;
    let mut best_sel = must_keep.clone();
    let mut best_len = build_selected_output(lines, &best_sel).len();

    if best_len > max_chars {
        return (best_sel, lines.len().saturating_sub(must_keep.len()));
    }

    loop {
        let mut progressed = false;

        if head + tail < lines.len() {
            let try_head = head + 1;
            let sel = selection_for_head_tail(lines, &must_keep, try_head, tail);
            let len = build_selected_output(lines, &sel).len();
            if len <= max_chars {
                head = try_head;
                best_sel = sel;
                best_len = len;
                progressed = true;
            }
        }

        if head + tail < lines.len() {
            let try_tail = tail + 1;
            let sel = selection_for_head_tail(lines, &must_keep, head, try_tail);
            let len = build_selected_output(lines, &sel).len();
            if len <= max_chars {
                tail = try_tail;
                best_sel = sel;
                best_len = len;
                progressed = true;
            }
        }

        if !progressed {
            break;
        }
    }

    let dropped = lines.len().saturating_sub(best_sel.len());
    let _ = best_len;
    (best_sel, dropped)
}

pub fn balanced_truncate(
    text: &str,
    max_lines: usize,
    head_lines: usize,
    middle_lines: usize,
    tail_lines: usize,
) -> TruncateResult {
    let lines: Vec<&str> = text.split('\n').collect();

    if max_lines == 0 || lines.len() <= max_lines {
        return TruncateResult {
            text: text.to_string(),
            truncated: false,
            dropped_lines: 0,
        };
    }

    let mut selected = BTreeSet::new();
    push_range(&mut selected, 0, head_lines, lines.len());

    let middle_start = lines.len().saturating_sub(middle_lines).saturating_div(2);
    push_range(&mut selected, middle_start, middle_lines, lines.len());

    let tail_start = lines.len().saturating_sub(tail_lines);
    push_range(&mut selected, tail_start, tail_lines, lines.len());

    while selected.len() > max_lines {
        if let Some(value) = selected.iter().copied().find(|i| *i >= middle_start) {
            selected.remove(&value);
        } else if let Some(value) = selected.iter().next_back().copied() {
            selected.remove(&value);
        } else {
            break;
        }
    }

    let dropped_lines = lines.len().saturating_sub(selected.len());
    let text = build_selected_output(&lines, &selected);

    TruncateResult {
        text,
        truncated: true,
        dropped_lines,
    }
}

/// Truncate only when output exceeds a byte budget. Does not drop lines by count.
pub fn char_truncate(text: &str, max_chars: usize) -> TruncateResult {
    char_truncate_with_intent(text, max_chars, &[])
}

/// Char budget fallback: keep head + tail (fill budget) + error lines + intent keywords.
pub fn char_truncate_with_intent(
    text: &str,
    max_chars: usize,
    intent_keywords: &[&str],
) -> TruncateResult {
    if max_chars == 0 || text.len() <= max_chars {
        return TruncateResult {
            text: text.to_string(),
            truncated: false,
            dropped_lines: 0,
        };
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let (selected, dropped_lines) = select_lines_within_budget(&lines, max_chars, intent_keywords);
    let sampled = build_selected_output(&lines, &selected);

    if sampled.len() <= max_chars {
        return TruncateResult {
            text: sampled,
            truncated: dropped_lines > 0,
            dropped_lines,
        };
    }

    truncate_text_by_chars(&sampled, max_chars)
}

fn truncate_text_by_chars(text: &str, max_chars: usize) -> TruncateResult {
    let marker = "\n[rtk:truncated by chars]\n";
    let budget = max_chars.saturating_sub(marker.len());
    let result = if budget == 0 {
        marker[..max_chars.min(marker.len())].to_string()
    } else {
        let head_chars = (budget as f64 * 0.55).ceil() as usize;
        let tail_chars = budget.saturating_sub(head_chars);

        let char_boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let char_count = char_boundaries.len();

        let tail_text = if tail_chars > 0 && char_count > tail_chars {
            let start = char_boundaries[char_count - tail_chars];
            &text[start..]
        } else {
            ""
        };

        let head_end = char_boundaries
            .get(head_chars.min(char_count))
            .copied()
            .unwrap_or(text.len());
        let mut result = format!("{}{}{}", &text[..head_end], marker, tail_text);

        if result.chars().count() > max_chars {
            let final_boundaries: Vec<usize> = result.char_indices().map(|(i, _)| i).collect();
            let end = final_boundaries
                .get(max_chars)
                .copied()
                .unwrap_or(result.len());
            result = result[..end].to_string();
        }
        result
    };

    TruncateResult {
        text: result,
        truncated: true,
        dropped_lines: 0,
    }
}

pub fn smart_truncate(
    text: &str,
    max_lines: usize,
    max_chars: usize,
    preserve_head: usize,
    preserve_tail: usize,
    priority_patterns: &[&regex::Regex],
    intent_keywords: &[&str],
) -> TruncateResult {
    let lines: Vec<&str> = text.split('\n').collect();
    let over_line_limit = max_lines > 0 && lines.len() > max_lines;
    let over_char_limit = max_chars > 0 && text.len() > max_chars;

    if !over_line_limit && !over_char_limit {
        return TruncateResult {
            text: text.to_string(),
            truncated: false,
            dropped_lines: 0,
        };
    }

    let preserve_head = preserve_head.max(0);
    let preserve_tail = preserve_tail.max(0);

    let priority_lines: Vec<usize> = if !priority_patterns.is_empty() {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| priority_patterns.iter().any(|p| p.is_match(line)))
            .map(|(i, _)| i)
            .collect()
    } else {
        vec![]
    };

    let mut selected_set: BTreeSet<usize> = BTreeSet::new();

    for i in 0..preserve_head.min(lines.len()) {
        selected_set.insert(i);
    }

    for &i in &priority_lines {
        selected_set.insert(i);
    }
    for (i, line) in lines.iter().enumerate() {
        if line_matches_intent(line, intent_keywords) {
            selected_set.insert(i);
        }
    }

    let tail_start = lines.len().saturating_sub(preserve_tail);
    for i in tail_start..lines.len() {
        if i >= preserve_head {
            selected_set.insert(i);
        }
    }

    let dropped_lines = lines.len().saturating_sub(selected_set.len());
    let mut result = build_selected_output(&lines, &selected_set);

    if max_chars > 0 && result.len() > max_chars {
        let truncated = truncate_text_by_chars(&result, max_chars);
        return TruncateResult {
            text: truncated.text,
            truncated: true,
            dropped_lines,
        };
    }

    TruncateResult {
        text: result,
        truncated: true,
        dropped_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_truncate_keeps_error_lines_in_middle() {
        let mut lines: Vec<String> = (1..=250)
            .map(|i| format!("noise line {i} {}", "x".repeat(80)))
            .collect();
        lines[125] = "error: the critical failure is here".into();
        let input = lines.join("\n");
        assert!(input.len() > 12_000, "len={}", input.len());

        let result = char_truncate_with_intent(&input, 12_000, &[]);
        assert!(result.truncated);
        assert!(
            result.text.contains("error: the critical failure is here"),
            "error line must survive char budget truncation"
        );
        assert!(result.text.contains("[rtk:truncated"));
    }

    #[test]
    fn char_truncate_keeps_intent_keywords() {
        let mut lines: Vec<String> = (1..=250)
            .map(|i| format!("log entry {i} {}", "x".repeat(80)))
            .collect();
        lines[125] = "matched UNIQUE_KEYWORD token here".into();
        let input = lines.join("\n");
        assert!(input.len() > 12_000, "len={}", input.len());

        let result = char_truncate_with_intent(&input, 12_000, &["UNIQUE_KEYWORD"]);
        assert!(result.text.contains("UNIQUE_KEYWORD"));
    }
}
