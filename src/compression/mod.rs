pub mod command_detector;
pub mod filter;
pub mod line_filter;
pub mod dedup;
pub mod truncate;
pub mod caveman;

pub fn init() {
    filter::load_filters();
    caveman::load_rules();
}

/// Compress tool output using RTK engine
pub fn rtk_compress(text: &str) -> String {
    use command_detector::detect_command_type;
    use filter::match_filter;
    use line_filter::apply_line_filter;
    use dedup::deduplicate_lines;
    use truncate::smart_truncate;
    use regex::Regex;

    // Detect command type
    let detection = detect_command_type(text, None);

    // Match filter
    let matched = match_filter(text, detection.command.as_deref());

    let mut result = text.to_string();

    if let Some(ref f) = matched {
        let filtered = apply_line_filter(&result, f);
        result = filtered.text;
    }

    // Deduplicate (threshold=3)
    let deduped = deduplicate_lines(&result, 3);
    result = deduped.text;

    // Smart truncate (120 lines, 12000 chars)
    let priority_patterns: Vec<Regex> = vec![
        Regex::new(r"(?i)error|failed|exception|traceback|TS\d{4}|FAIL|✖").unwrap(),
    ];
    let truncated = smart_truncate(&result, 120, 12000, 24, 24, &priority_patterns);
    result = truncated.text;

    result
}

/// Compress user text using Caveman engine
pub fn caveman_compress(text: &str, level: &str) -> String {
    caveman::compress(text, "user", level)
}
