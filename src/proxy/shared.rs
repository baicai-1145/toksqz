//! Shared compression helpers used by every API-format handler.
//!
//! These are format-agnostic: they take an already-extracted tool-output (or
//! user) string and run it through the RTK / Caveman engines while accumulating
//! per-request stats. Each format module (openai_chat, anthropic, gemini,
//! responses) is responsible for walking its own JSON shape and feeding strings
//! plus an optional command hint here.

use toksqz::compression;

/// Aggregated per-request compression stats, returned to the proxy layer.
pub(crate) struct CompressResult {
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub filters_applied: Vec<String>,
    pub per_command: Vec<compression::stats::CommandStats>,
}

impl CompressResult {
    pub(crate) fn new() -> Self {
        CompressResult {
            original_tokens: 0,
            compressed_tokens: 0,
            filters_applied: Vec::new(),
            per_command: Vec::new(),
        }
    }
}

/// Token estimate aligned with Node.js `Math.ceil(str.length / 4)`.
/// JS `str.length` counts UTF-16 code units; `chars().count()` approximates it
/// (only differs for non-BMP characters such as emoji).
pub(crate) fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

/// Compress a single tool-output string via RTK, recording stats. The optional
/// `command_hint` is the command that produced this output (raw or synthesized);
/// when present it drives precise filter routing and intent injection. When
/// `None`, RTK falls back to content-based detection.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compress_tool_output(
    content: &str,
    command_hint: Option<&str>,
    config: &crate::Config,
    acc: &mut CompressResult,
) -> String {
    let orig = estimate_tokens(content);
    acc.original_tokens += orig;
    if !config.rtk_enabled {
        acc.compressed_tokens += orig;
        return content.to_string();
    }
    let result = compression::rtk_compress_with_command(content, command_hint);
    let new_tokens = estimate_tokens(&result.text);
    if config.log_enabled && result.text.len() < content.len() {
        println!(
            "  [RTK] {}→{} chars{}{}",
            content.len(),
            result.text.len(),
            result
                .filter_id
                .as_ref()
                .map(|fid| format!(" ({})", fid))
                .unwrap_or_default(),
            if result.grouping_applied { " [grouped]" } else { "" },
        );
    }
    let filter_id = result.filter_id.clone().unwrap_or_else(|| "none".to_string());
    compression::stats::record_message(&filter_id, &result.command_type, orig, new_tokens);
    if let Some(ref fid) = result.filter_id {
        if !acc.filters_applied.contains(fid) {
            acc.filters_applied.push(fid.clone());
        }
    }
    acc.per_command.push(compression::stats::CommandStats {
        command_type: result.command_type.clone(),
        filter_id,
        original_tokens: orig,
        compressed_tokens: new_tokens,
    });
    acc.compressed_tokens += new_tokens;
    result.text
}

/// Compress user-authored text via the Caveman engine (no-op when disabled).
/// Returns `(compressed, original_tokens, new_tokens)`.
pub(crate) fn compress_user_text_return(content: &str, config: &crate::Config) -> (String, usize, usize) {
    let orig = estimate_tokens(content);
    if let Some(ref level) = config.caveman_level {
        let compressed = compression::caveman_compress(content, level);
        let new_tokens = estimate_tokens(&compressed);
        (compressed, orig, new_tokens)
    } else {
        (content.to_string(), orig, orig)
    }
}

/// Account for a non-compressed text span (assistant/system/developer content):
/// it counts toward both original and compressed totals unchanged.
pub(crate) fn account_passthrough(content: &str, acc: &mut CompressResult) {
    let orig = estimate_tokens(content);
    acc.original_tokens += orig;
    acc.compressed_tokens += orig;
}
