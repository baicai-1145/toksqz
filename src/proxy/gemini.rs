//! Google Gemini handler.
//!
//! Shape: `{"contents": [{"role": "user"|"model"|"function", "parts": [...]}]}`.
//! A function call is `{"functionCall": {"name": ..., "args": {...}}}` and its
//! output is `{"functionResponse": {"name": ..., "response": {...}}}` (sometimes
//! accompanied by a `text` part). Gemini does not carry a per-call id, so
//! association is best-effort: we map `functionCall.name → hint` and only apply
//! it when that name is unambiguous (appears once). Anything uncertain falls
//! back to content-based detection — current behavior.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use serde_json::Value;

use super::hint;
use super::shared::{compress_tool_output, compress_user_text_return, estimate_tokens, CompressResult};

fn collect_hints(payload: &Value) -> HashMap<String, String> {
    // name -> Some(hint) when seen exactly once; None marks an ambiguous name.
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    if let Some(contents) = payload.get("contents").and_then(|c| c.as_array()) {
        for content in contents {
            if let Some(parts) = content.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        if name.is_empty() {
                            continue;
                        }
                        let args = fc.get("args").cloned().unwrap_or(Value::Null);
                        let h = hint::synthesize(name, &args);
                        match seen.entry(name.to_string()) {
                            Entry::Occupied(mut e) => {
                                e.insert(None);
                            }
                            Entry::Vacant(e) => {
                                e.insert(h);
                            }
                        }
                    }
                }
            }
        }
    }
    seen.into_iter()
        .filter_map(|(k, v)| v.map(|h| (k, h)))
        .collect()
}

pub(crate) fn compress(payload: &mut Value, config: &crate::Config) -> Option<CompressResult> {
    let hints = collect_hints(payload);
    let contents = payload.get_mut("contents")?.as_array_mut()?;
    let mut acc = CompressResult::new();

    for content in contents.iter_mut() {
        let role = content
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        let parts = match content.get_mut("parts").and_then(|p| p.as_array_mut()) {
            Some(p) => p,
            None => continue,
        };

        for part in parts.iter_mut() {
            let is_function_response = part.get("functionResponse").is_some();
            let is_tool_like = is_function_response || role == "function";

            let text = match part.get("text").and_then(|t| t.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };

            if is_tool_like {
                let hint = part
                    .get("functionResponse")
                    .and_then(|fr| fr.get("name"))
                    .and_then(|v| v.as_str())
                    .and_then(|name| hints.get(name))
                    .cloned();
                let compressed = compress_tool_output(&text, hint.as_deref(), config, &mut acc);
                part["text"] = Value::String(compressed);
            } else if role == "user" {
                let (compressed, orig, new) = compress_user_text_return(&text, config, &mut acc);
                acc.original_tokens += orig;
                acc.compressed_tokens += new;
                if config.log_enabled && compressed.len() < text.len() {
                    println!("  [Caveman/Gemini] {}→{} chars", text.len(), compressed.len());
                }
                part["text"] = Value::String(compressed);
            } else {
                let orig = estimate_tokens(&text);
                acc.original_tokens += orig;
                acc.compressed_tokens += orig;
            }
        }
    }

    acc.finish()
}

#[cfg(test)]
mod tests {
    use crate::proxy::compress_messages;
    use crate::proxy::tests_util::test_config;

    #[test]
    fn unique_function_call_name_routes_response() {
        let mut payload = serde_json::json!({
            "contents": [
                {"role": "model", "parts": [
                    {"functionCall": {"name": "Bash", "args": {"command": "find . -type f"}}}
                ]},
                {"role": "function", "parts": [
                    {"functionResponse": {"name": "Bash"}, "text": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
                ]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn ambiguous_function_call_name_falls_back() {
        let mut payload = serde_json::json!({
            "contents": [
                {"role": "model", "parts": [
                    {"functionCall": {"name": "Bash", "args": {"command": "find . -type f"}}},
                    {"functionCall": {"name": "Bash", "args": {"command": "ls -la"}}}
                ]},
                {"role": "function", "parts": [
                    {"functionResponse": {"name": "Bash"}, "text": "some output line\n"}
                ]}
            ]
        });
        // Hint suppressed and output too small to change — forward original bytes.
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_none());
    }
}
