use regex::Regex;

pub struct TruncateResult {
    pub text: String,
    pub truncated: bool,
    pub dropped_lines: usize,
}

pub fn smart_truncate(
    text: &str,
    max_lines: usize,
    max_chars: usize,
    preserve_head: usize,
    preserve_tail: usize,
    priority_patterns: &[&Regex],
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

    // Priority lines
    for &i in &priority_lines {
        selected_set.insert(i);
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

    // Build output with truncation marker
    let head_count = preserve_head.min(lines.len());
    let mut result_lines: Vec<String> = Vec::new();

    // Add head lines
    for &i in selected.iter().take_while(|&&i| i < head_count) {
        result_lines.push(lines[i].to_string());
    }

    // Add truncation marker
    result_lines.push(format!("[rtk:truncated {} lines]", dropped_lines));

    // Add remaining selected lines (priority + tail)
    for &i in selected.iter().filter(|&&i| i >= head_count) {
        result_lines.push(lines[i].to_string());
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
