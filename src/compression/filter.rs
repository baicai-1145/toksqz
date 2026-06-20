use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

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

static FILTERS: Lazy<Vec<Filter>> = Lazy::new(|| {
    let raw_json = include_str!("../../assets/all_filters.json");
    let raw_filters: Vec<RawFilter> = serde_json::from_str(raw_json)
        .expect("Failed to parse embedded filters JSON");

    let mut filters: Vec<Filter> = raw_filters.into_iter().map(|raw| {
        // Detect if this is canonical (pack) format or legacy format
        let is_canonical = !raw.r#match.commands.is_empty()
            || !raw.r#match.patterns.is_empty()
            || !raw.r#match.outputTypes.is_empty()
            || !raw.rules.dropPatterns.is_empty()
            || !raw.rules.includePatterns.is_empty()
            || raw.rules.stripAnsi;

        if is_canonical {
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
            }
        }
    }).collect();

    // Sort by priority (desc), then id (asc)
    filters.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.id.cmp(&b.id)));
    filters
});

pub fn load_filters() -> &'static Vec<Filter> {
    &FILTERS
}

/// Match filter: commandTypes → commandPatterns → matchPatterns → generic fallback
pub fn match_filter(text: &str, command: Option<&str>) -> Option<&'static Filter> {
    use crate::compression::command_detector::detect_command_type;

    let detection = detect_command_type(text, command);
    let detected_command = detection.command.as_deref().unwrap_or("");

    // 1. Match by command type (outputTypes)
    if let Some(f) = FILTERS.iter().find(|f| f.command_types.contains(&detection.command_type)) {
        return Some(f);
    }

    // 2. Match by command patterns (regex on command string)
    if !detected_command.is_empty() {
        if let Some(f) = FILTERS.iter().find(|f| {
            f.command_patterns.iter().any(|p| {
                Regex::new(p).map_or(false, |re| re.is_match(detected_command))
            })
        }) {
            return Some(f);
        }
    }

    // 3. Match by match patterns (regex on full text)
    if let Some(f) = FILTERS.iter().find(|f| {
        f.match_patterns.iter().any(|p| {
            Regex::new(&format!("(?im){}", p)).map_or(false, |re| re.is_match(text))
        })
    }) {
        return Some(f);
    }

    // 4. Fallback to generic-output
    FILTERS.iter().find(|f| f.command_types.contains(&"generic-output".to_string()))
}
