use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub struct DedupResult {
    pub text: String,
    pub collapsed: usize,
}

const MIN_BLOCK_LINES: usize = 3;
const MAX_BLOCK_LINES: usize = 30;

fn hash_block(lines: &[&str], start: usize, len: usize) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in &lines[start..start + len] {
        line.hash(&mut hasher);
        '\n'.hash(&mut hasher);
    }
    hasher.finish()
}

fn blocks_equal(lines: &[&str], a: usize, b: usize, len: usize) -> bool {
    lines[a..a + len] == lines[b..b + len]
}

/// Short preview of the first line of a block for self-describing markers.
fn anchor_preview(line: &str) -> String {
    let trimmed = line.trim();
    let preview: String = trimmed.chars().take(60).collect();
    if trimmed.chars().count() > 60 {
        format!("{preview}…")
    } else {
        preview
    }
}

fn escape_for_marker(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Exact block deduplication (LZ77-style): when a multi-line block repeats an
/// earlier identical block, keep the first occurrence and replace later copies
/// with a self-describing marker. Lossless — no unique information is removed.
pub fn deduplicate_blocks(text: &str) -> DedupResult {
    if text.is_empty() {
        return DedupResult {
            text: String::new(),
            collapsed: 0,
        };
    }

    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len();
    if n < MIN_BLOCK_LINES {
        return DedupResult {
            text: text.to_string(),
            collapsed: 0,
        };
    }

    // Index first occurrence of each (length, hash) block.
    let mut first_seen: HashMap<(usize, u64), usize> = HashMap::new();
    for start in 0..n {
        let max_len = MAX_BLOCK_LINES.min(n - start);
        if max_len < MIN_BLOCK_LINES {
            continue;
        }
        for len in MIN_BLOCK_LINES..=max_len {
            let key = (len, hash_block(&lines, start, len));
            first_seen.entry(key).or_insert(start);
        }
    }

    let mut output: Vec<String> = Vec::with_capacity(n);
    let mut collapsed = 0usize;
    let mut index = 0usize;

    while index < n {
        let mut best: Option<(usize, usize)> = None; // (first_start, len)

        let max_len = MAX_BLOCK_LINES.min(n - index);
        if max_len >= MIN_BLOCK_LINES {
            for len in (MIN_BLOCK_LINES..=max_len).rev() {
                let key = (len, hash_block(&lines, index, len));
                let Some(&first_start) = first_seen.get(&key) else {
                    continue;
                };
                if first_start >= index {
                    continue;
                }
                if !blocks_equal(&lines, first_start, index, len) {
                    continue;
                }
                best = Some((first_start, len));
                break;
            }
        }

        if let Some((first_start, len)) = best {
            let anchor = escape_for_marker(&anchor_preview(lines[first_start]));
            output.push(format!(
                "[rtk: omitted {len}-line block repeating earlier content starting with \"{anchor}\"]"
            ));
            collapsed += len.saturating_sub(1);
            index += len;
            continue;
        }

        output.push(lines[index].to_string());
        index += 1;
    }

    DedupResult {
        text: output.join("\n"),
        collapsed,
    }
}

/// Collapse runs of consecutive identical non-empty lines (threshold ≥ 2).
pub fn deduplicate_lines(text: &str, threshold: usize) -> DedupResult {
    let threshold = threshold.max(2);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut collapsed: usize = 0;

    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let mut run_length = 1usize;
        while index + run_length < lines.len() && lines[index + run_length] == line {
            run_length += 1;
        }

        if !line.trim().is_empty() && run_length >= threshold {
            output.push(line.to_string());
            output.push(format!("[rtk:dropped {} repeated lines]", run_length - 1));
            collapsed += run_length - 1;
            index += run_length;
            continue;
        }

        output.push(line.to_string());
        index += 1;
    }

    DedupResult {
        text: output.join("\n"),
        collapsed,
    }
}

/// Block dedup first (non-consecutive repeats), then consecutive single-line dedup.
pub fn deduplicate(text: &str) -> DedupResult {
    let blocks = deduplicate_blocks(text);
    let lines = deduplicate_lines(&blocks.text, 3);
    DedupResult {
        text: lines.text,
        collapsed: blocks.collapsed + lines.collapsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_dedup_non_consecutive() {
        let block_a: Vec<String> = (1..=10).map(|i| format!("line-a-{i}")).collect();
        let mut lines = block_a.clone();
        lines.push("separator-b".into());
        lines.extend(block_a.clone());
        lines.push("separator-c".into());
        lines.extend(block_a.clone());
        let input = lines.join("\n");

        let result = deduplicate_blocks(&input);
        assert!(result.collapsed > 0, "should collapse repeated blocks");
        assert!(
            result.text.contains("omitted 10-line block"),
            "marker should describe the omitted block"
        );
        assert!(result.text.contains("line-a-1"));
        assert!(result.text.contains("separator-b"));
        assert!(result.text.contains("separator-c"));
        assert_eq!(result.text.matches("line-a-10").count(), 1);
    }

    #[test]
    fn test_block_dedup_preserves_unique_middle_line() {
        let block: Vec<String> = (1..=5).map(|i| format!("chunk-{i}")).collect();
        let mut lines = block.clone();
        lines.push("UNIQUE-ERROR".into());
        lines.extend(block);
        let input = lines.join("\n");

        let result = deduplicate_blocks(&input);
        assert!(result.text.contains("UNIQUE-ERROR"));
        assert!(result.text.contains("omitted 5-line block"));
    }

    #[test]
    fn test_consecutive_line_dedup_still_works() {
        let mut lines: Vec<String> = (1..=5).map(|i| format!("u-{i}")).collect();
        for _ in 0..4 {
            lines.push("SAME".into());
        }
        let input = lines.join("\n");

        let result = deduplicate(&input);
        assert!(
            result.text.contains("[rtk:dropped")
                || result.text.contains("omitted")
                || result.text.contains("repeating earlier"),
            "consecutive repeats should be collapsed"
        );
    }
}
