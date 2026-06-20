use std::collections::BTreeMap;
use regex::Regex;
use once_cell::sync::Lazy;

pub struct GroupingResult {
    pub text: String,
    pub applied: bool,
    pub original_lines: usize,
    pub grouped_lines: usize,
}

// ─── Regex patterns ─────────────────────────────────────────────────────────

static FILE_PATH_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?:modified|new file|deleted|renamed|both modified|both added|typechange):\s+(.+)$").unwrap()
});

static GIT_STATUS_SHORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*([MADRCU?!]{1,2})\s+(.+)$").unwrap()
});

static FIND_PATH_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:\.{0,2}/|/)(.+)$").unwrap()
});

static TEST_FAIL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:FAIL|FAILED|✘|×|❯)\s+(.+)$").unwrap()
});

static TEST_PASS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:PASS|✓|√)\s+(.+)$").unwrap()
});

static TEST_SUMMARY_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:test\s+(?:result|suites?|files?)|tests?:?\s+\d+\s+(?:passed|failed))").unwrap()
});

static BUILD_ERROR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:error|ERROR|✖|ERR!)[:\s]").unwrap()
});

static BUILD_WARNING_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:warning|WARNING|warn|⚠)[:\s]").unwrap()
});

static BUILD_INFO_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:info|INFO|note|notice)[:\s]").unwrap()
});

/// Extract directory from a file path
fn dir_of(path: &str) -> &str {
    let path = path.trim();
    match path.rfind('/') {
        Some(pos) => &path[..pos],
        None => ".",
    }
}

/// Group file list entries by directory, returning a compact summary.
fn group_file_list(lines: &[&str]) -> Option<String> {
    let mut dir_counts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut non_file_lines = Vec::new();
    let mut file_count = 0usize;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            non_file_lines.push(*line);
            continue;
        }

        // Try git status long format
        if let Some(caps) = FILE_PATH_RE.captures(trimmed) {
            let path = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let dir = dir_of(path).to_string();
            dir_counts.entry(dir).or_default().push(path.to_string());
            file_count += 1;
            continue;
        }

        // Try git status short format
        if let Some(caps) = GIT_STATUS_SHORT_RE.captures(trimmed) {
            let path = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let dir = dir_of(path).to_string();
            dir_counts.entry(dir).or_default().push(path.to_string());
            file_count += 1;
            continue;
        }

        // Try generic file paths (find results, etc.)
        if FIND_PATH_RE.is_match(trimmed) && !trimmed.starts_with(' ') {
            let dir = dir_of(trimmed).to_string();
            dir_counts.entry(dir).or_default().push(trimmed.to_string());
            file_count += 1;
            continue;
        }

        non_file_lines.push(*line);
    }

    // Only group if we have enough files to benefit
    if file_count < 5 || dir_counts.len() <= 1 {
        return None;
    }

    let mut result = Vec::new();

    // Keep non-file header/context lines
    for line in &non_file_lines {
        if !line.trim().is_empty() {
            result.push(line.to_string());
        }
    }

    // Build grouped summary
    result.push(format!("[rtk:grouped {} files across {} directories]", file_count, dir_counts.len()));
    for (dir, files) in &dir_counts {
        if files.len() == 1 {
            result.push(format!("  {} (1 file)", dir));
        } else {
            result.push(format!("  {} ({} files)", dir, files.len()));
        }
    }

    Some(result.join("\n"))
}

/// Group test output: show failures in detail, summarize passes.
fn group_test_output(lines: &[&str]) -> Option<String> {
    let mut failures = Vec::new();
    let mut passes = Vec::new();
    let mut summaries = Vec::new();
    let mut context_lines = Vec::new();
    let mut in_failure_block = false;

    for line in lines {
        let trimmed = line.trim();

        // Check for test summary lines
        if TEST_SUMMARY_RE.is_match(trimmed) {
            summaries.push(*line);
            in_failure_block = false;
            continue;
        }

        // Check for failure markers
        if TEST_FAIL_RE.is_match(trimmed) {
            failures.push(*line);
            in_failure_block = true;
            continue;
        }

        // Check for pass markers
        if TEST_PASS_RE.is_match(trimmed) {
            passes.push(*line);
            in_failure_block = false;
            continue;
        }

        // Lines following a failure (error details, stack traces)
        if in_failure_block && !trimmed.is_empty() {
            failures.push(*line);
            continue;
        }

        if !trimmed.is_empty() {
            context_lines.push(*line);
        }
        in_failure_block = false;
    }

    let total_tests = failures.len() + passes.len();
    if total_tests < 3 {
        return None;
    }

    let mut result = Vec::new();

    // Context lines (headers, etc.)
    for line in &context_lines {
        result.push(line.to_string());
    }

    // Failures in detail
    if !failures.is_empty() {
        result.push(format!("[rtk:test-failures {}]", failures.len()));
        for f in &failures {
            result.push(f.to_string());
        }
    }

    // Pass summary
    if !passes.is_empty() {
        result.push(format!("[rtk:test-passed {} tests]", passes.len()));
    }

    // Original summary lines
    for s in &summaries {
        result.push(s.to_string());
    }

    Some(result.join("\n"))
}

/// Group build output by severity: errors, warnings, info.
fn group_build_output(lines: &[&str]) -> Option<String> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut infos = Vec::new();
    let mut other = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if BUILD_ERROR_RE.is_match(trimmed) {
            errors.push(*line);
        } else if BUILD_WARNING_RE.is_match(trimmed) {
            warnings.push(*line);
        } else if BUILD_INFO_RE.is_match(trimmed) {
            infos.push(*line);
        } else {
            other.push(*line);
        }
    }

    let total_categorized = errors.len() + warnings.len() + infos.len();
    if total_categorized < 3 {
        return None;
    }

    let mut result = Vec::new();

    // Other context lines (summary lines, etc.)
    for line in &other {
        result.push(line.to_string());
    }

    // Errors first
    if !errors.is_empty() {
        result.push(format!("[rtk:build-errors {}]", errors.len()));
        for e in &errors {
            result.push(e.to_string());
        }
    }

    // Warnings
    if !warnings.is_empty() {
        result.push(format!("[rtk:build-warnings {}]", warnings.len()));
        // Only show first 10 warnings
        for w in warnings.iter().take(10) {
            result.push(w.to_string());
        }
        if warnings.len() > 10 {
            result.push(format!("  ... and {} more warnings", warnings.len() - 10));
        }
    }

    // Info (just count)
    if !infos.is_empty() {
        result.push(format!("[rtk:build-info {} messages]", infos.len()));
    }

    Some(result.join("\n"))
}

/// Apply grouping strategy based on command type.
/// Returns the grouped text, or None if grouping was not beneficial.
pub fn apply_grouping(text: &str, command_type: &str, _level: &str) -> GroupingResult {
    let lines: Vec<&str> = text.lines().collect();
    let original_lines = lines.len();

    // Don't group small outputs
    if original_lines < 5 {
        return GroupingResult {
            text: text.to_string(),
            applied: false,
            original_lines,
            grouped_lines: original_lines,
        };
    }

    let grouped = match command_type {
        // File list grouping
        "git-status" | "shell-find" | "shell-ls" => group_file_list(&lines),

        // Test output grouping
        "test-vitest" | "test-jest" | "test-pytest" | "test-cargo" | "test-go" | "playwright" => {
            group_test_output(&lines)
        }

        // Build output grouping
        "build-typescript" | "build-eslint" | "build-webpack" | "build-vite"
        | "biome" | "ruff" | "mypy" | "golangci-lint" | "rubocop" => {
            group_build_output(&lines)
        }

        // Generic grouping for unknown types with file-like output
        _ => {
            // Try file list grouping as fallback
            group_file_list(&lines)
                .or_else(|| group_build_output(&lines))
        }
    };

    match grouped {
        Some(grouped_text) => {
            let grouped_lines = grouped_text.lines().count();
            GroupingResult {
                text: grouped_text,
                applied: true,
                original_lines,
                grouped_lines,
            }
        }
        None => GroupingResult {
            text: text.to_string(),
            applied: false,
            original_lines,
            grouped_lines: original_lines,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_group_git_status_by_directory() {
        let input = "\
On branch main
Changes not staged for commit:
\tmodified: src/app.ts
\tmodified: src/utils/helper.ts
\tmodified: src/utils/format.ts
\tmodified: src/components/Button.tsx
\tmodified: src/components/Input.tsx
\tmodified: src/components/Modal.tsx
\tmodified: tests/app.test.ts
\tmodified: tests/utils.test.ts";
        let result = apply_grouping(input, "git-status", "lite");
        assert!(result.applied);
        assert!(result.text.contains("[rtk:grouped"));
        assert!(result.text.contains("src ("));
        assert!(result.grouped_lines < result.original_lines);
    }

    #[test]
    fn test_no_grouping_for_small_output() {
        let input = "line1\nline2\nline3";
        let result = apply_grouping(input, "git-status", "lite");
        assert!(!result.applied);
    }

    #[test]
    fn test_group_test_output_with_failures() {
        let input = "\
Running 5 tests
PASS src/a.test.ts
PASS src/b.test.ts
FAIL src/c.test.ts
  Error: expected true
  at c.test.ts:10
FAIL src/d.test.ts
  Error: timeout
Test Suites: 2 failed, 2 passed";
        let result = apply_grouping(input, "test-jest", "lite");
        assert!(result.applied);
        assert!(result.text.contains("[rtk:test-failures"));
        assert!(result.text.contains("[rtk:test-passed 2 tests]"));
    }

    #[test]
    fn test_group_build_output_by_severity() {
        let input = "\
Building project...
error: TS2322: Type 'string' not assignable
warning: unused variable 'x'
info: compiled in 2s
error: TS2345: Argument not assignable
warning: deprecated API usage
warning: missing return type
info: watching for changes
info: cache hit
Build failed with 2 errors";
        let result = apply_grouping(input, "build-typescript", "lite");
        assert!(result.applied);
        assert!(result.text.contains("[rtk:build-errors 2]"));
        assert!(result.text.contains("[rtk:build-warnings 3]"));
        assert!(result.text.contains("[rtk:build-info 3 messages]"));
    }

    #[test]
    fn test_no_grouping_single_directory() {
        let input = "\
./src/a.ts
./src/b.ts
./src/c.ts
./src/d.ts
./src/e.ts";
        let result = apply_grouping(input, "shell-find", "lite");
        // Single directory should not trigger grouping
        assert!(!result.applied);
    }

    #[test]
    fn test_group_find_multidir() {
        let input = "\
./src/app.ts
./src/utils/helper.ts
./src/utils/format.ts
./lib/core.ts
./lib/plugins/a.ts
./lib/plugins/b.ts
./tests/test.ts
./tests/integration/test.ts";
        let result = apply_grouping(input, "shell-find", "lite");
        assert!(result.applied);
        assert!(result.text.contains("[rtk:grouped"));
    }
}
