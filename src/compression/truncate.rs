use regex::Regex;

pub struct TruncateResult {
    pub text: String,
    pub truncated: bool,
    pub dropped_lines: usize,
}

fn push_range(
    selected: &mut std::collections::BTreeSet<usize>,
    start: usize,
    count: usize,
    len: usize,
) {
    let end = start.saturating_add(count).min(len);
    for i in start.min(len)..end {
        selected.insert(i);
    }
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

    let mut selected = std::collections::BTreeSet::new();
    push_range(&mut selected, 0, head_lines, lines.len());

    let middle_start = lines.len().saturating_sub(middle_lines).saturating_div(2);
    push_range(&mut selected, middle_start, middle_lines, lines.len());

    let tail_start = lines.len().saturating_sub(tail_lines);
    push_range(&mut selected, tail_start, tail_lines, lines.len());

    while selected.len() > max_lines {
        if let Some(value) = selected.iter().copied().find(|i| *i >= middle_start) {
            selected.remove(&value);
        } else if let Some(value) = selected.iter().copied().next_back() {
            selected.remove(&value);
        } else {
            break;
        }
    }

    let dropped_lines = lines.len().saturating_sub(selected.len());
    let mut result_lines = Vec::new();
    let mut previous: Option<usize> = None;

    for i in selected {
        if let Some(prev) = previous {
            if i > prev + 1 {
                result_lines.push(format!("[rtk:truncated {} lines]", i - prev - 1));
            }
        }
        result_lines.push(lines[i].to_string());
        previous = Some(i);
    }

    TruncateResult {
        text: result_lines.join("\n"),
        truncated: true,
        dropped_lines,
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

/// Truncate only when output exceeds a byte budget. Does not drop lines by count.
pub fn char_truncate(text: &str, max_chars: usize) -> TruncateResult {
    char_truncate_with_intent(text, max_chars, &[])
}

/// Char budget fallback that keeps lines matching intent keywords when possible.
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

    if !intent_keywords.is_empty() {
        let lines: Vec<&str> = text.split('\n').collect();
        let mut selected = std::collections::BTreeSet::new();
        push_range(&mut selected, 0, 40, lines.len());
        push_range(
            &mut selected,
            lines.len().saturating_sub(40),
            40,
            lines.len(),
        );
        for (i, line) in lines.iter().enumerate() {
            if line_matches_intent(line, intent_keywords) {
                selected.insert(i);
            }
        }

        let dropped_lines = lines.len().saturating_sub(selected.len());
        let mut result_lines = Vec::new();
        let mut previous: Option<usize> = None;
        for i in selected {
            if let Some(prev) = previous {
                if i > prev + 1 {
                    result_lines.push(format!("[rtk:truncated {} lines]", i - prev - 1));
                }
            }
            result_lines.push(lines[i].to_string());
            previous = Some(i);
        }
        let sampled = result_lines.join("\n");
        if sampled.len() <= max_chars {
            return TruncateResult {
                text: sampled,
                truncated: true,
                dropped_lines,
            };
        }
        return truncate_text_by_chars(&sampled, max_chars);
    }

    truncate_text_by_chars(text, max_chars)
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
    priority_patterns: &[&Regex],
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

    // Collect priority lines (matching priority patterns)
    let priority_lines: Vec<usize> = if !priority_patterns.is_empty() {
        lines.iter().enumerate()
            .filter(|(_, line)| priority_patterns.iter().any(|p| p.is_match(line)))
            .map(|(i, _)| i)
            .collect()
    } else {
        vec![]
    };

    // Build selected lines: head + priority + tail
    // Use HashSet for O(1) lookup
    let mut selected_set: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Head lines
    for i in 0..preserve_head.min(lines.len()) {
        selected_set.insert(i);
    }

    // Priority lines (regex + intent keywords)
    for &i in &priority_lines {
        selected_set.insert(i);
    }
    for (i, line) in lines.iter().enumerate() {
        if line_matches_intent(line, intent_keywords) {
            selected_set.insert(i);
        }
    }

    // Tail lines
    let tail_start = lines.len().saturating_sub(preserve_tail);
    for i in tail_start..lines.len() {
        if i >= preserve_head {
            selected_set.insert(i);
        }
    }

    let mut selected: Vec<usize> = selected_set.into_iter().collect();
    selected.sort();
    let dropped_lines = lines.len().saturating_sub(selected.len());

    // Build output with a truncation marker at each non-contiguous gap so that
    // preserved head/priority/tail lines are never rendered as falsely adjacent.
    let mut result_lines: Vec<String> = Vec::new();
    let mut previous: Option<usize> = None;
    for &i in &selected {
        if let Some(prev) = previous {
            if i > prev + 1 {
                result_lines.push(format!("[rtk:truncated {} lines]", i - prev - 1));
            }
        }
        result_lines.push(lines[i].to_string());
        previous = Some(i);
    }

    let mut result = result_lines.join("\n");

    // Char truncation
    if max_chars > 0 && result.len() > max_chars {
        let marker = "\n[rtk:truncated by chars]\n";
        let budget = max_chars.saturating_sub(marker.len());
        if budget == 0 {
            result = marker[..max_chars.min(marker.len())].to_string();
        } else {
            let head_chars = (budget as f64 * 0.55).ceil() as usize;
            let tail_chars = budget.saturating_sub(head_chars);

            // Collect char boundaries once for efficiency
            let char_boundaries: Vec<usize> = result.char_indices().map(|(i, _)| i).collect();
            let char_count = char_boundaries.len();

            let tail_text = if tail_chars > 0 && char_count > tail_chars {
                let start = char_boundaries[char_count - tail_chars];
                &result[start..]
            } else {
                ""
            };

            let head_end = char_boundaries.get(head_chars.min(char_count)).copied().unwrap_or(result.len());
            result = format!("{}{}{}", &result[..head_end], marker, tail_text);

            if result.chars().count() > max_chars {
                let final_boundaries: Vec<usize> = result.char_indices().map(|(i, _)| i).collect();
                let end = final_boundaries.get(max_chars).copied().unwrap_or(result.len());
                result = result[..end].to_string();
            }
        }
    }

    TruncateResult {
        text: result,
        truncated: true,
        dropped_lines,
    }
}
