//! Anthropic Messages handler.
//!
//! Shape: `{"messages": [{"role": "user"|"assistant", "content": "str" | [blocks]}]}`.
//! Blocks of interest:
//!   - `{"type": "tool_use", "id": "...", "name": "...", "input": {...}}` (assistant)
//!   - `{"type": "tool_result", "tool_use_id": "...", "content": "str" | [blocks]}` (user)
//!
//! First pass maps `tool_use.id → synthesized command hint`; second pass
//! compresses each `tool_result` with that hint (content-based fallback when
//! absent).

use std::collections::HashMap;

use serde_json::Value;

use super::hint;
use super::shared::{compress_tool_output, compress_user_text_return, estimate_tokens, CompressResult};

fn collect_hints(payload: &Value) -> HashMap<String, String> {
    let mut hints: HashMap<String, String> = HashMap::new();
    let messages = match payload.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return hints,
    };
    for msg in messages {
        let blocks = match msg.get("content").and_then(|c| c.as_array()) {
            Some(b) => b,
            None => continue,
        };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let id = match block.get("id").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            let name = block.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let input = block.get("input").cloned().unwrap_or(Value::Null);
            if let Some(h) = hint::synthesize(name, &input) {
                hints.insert(id.to_string(), h);
            }
        }
    }
    hints
}

pub(crate) fn compress(payload: &mut Value, config: &crate::Config) -> Option<CompressResult> {
    let hints = collect_hints(payload);
    let messages = payload.get_mut("messages")?.as_array_mut()?;
    let mut acc = CompressResult::new();

    for msg in messages.iter_mut() {
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        let content = match msg.get_mut("content") {
            Some(c) => c,
            None => continue,
        };

        match content {
            Value::String(s) if !s.is_empty() => {
                let text = s.clone();
                if role == "user" {
                    let (compressed, orig, new) = compress_user_text_return(&text, config, &mut acc);
                    acc.original_tokens += orig;
                    acc.compressed_tokens += new;
                    *s = compressed;
                } else {
                    let orig = estimate_tokens(&text);
                    acc.original_tokens += orig;
                    acc.compressed_tokens += orig;
                }
            }
            Value::Array(blocks) => {
                for block in blocks.iter_mut() {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match block_type {
                        "tool_result" => {
                            let hint = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .and_then(|id| hints.get(id))
                                .cloned();
                            if let Some(tc) = block.get_mut("content") {
                                compress_content(tc, true, hint.as_deref(), config, &mut acc);
                            }
                        }
                        "text" => {
                            if let Some(text_val) = block.get_mut("text") {
                                if let Some(s) = text_val.as_str() {
                                    let text = s.to_string();
                                    if role == "user" {
                                        let (compressed, orig, new) =
                                            compress_user_text_return(&text, config, &mut acc);
                                        acc.original_tokens += orig;
                                        acc.compressed_tokens += new;
                                        *text_val = Value::String(compressed);
                                    } else {
                                        let orig = estimate_tokens(&text);
                                        acc.original_tokens += orig;
                                        acc.compressed_tokens += orig;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    acc.finish()
}
fn compress_content(
    content: &mut Value,
    is_tool: bool,
    hint: Option<&str>,
    config: &crate::Config,
    acc: &mut CompressResult,
) {
    match content {
        Value::String(s) if !s.is_empty() => {
            let text = s.clone();
            if is_tool {
                *s = compress_tool_output(&text, hint, config, acc);
            } else {
                let (compressed, orig, new) = compress_user_text_return(&text, config, acc);
                acc.original_tokens += orig;
                acc.compressed_tokens += new;
                *s = compressed;
            }
        }
        Value::Array(blocks) => {
            for block in blocks.iter_mut() {
                let t = match block.get("text").and_then(|v| v.as_str()) {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => continue,
                };
                if is_tool {
                    let compressed = compress_tool_output(&t, hint, config, acc);
                    block["text"] = Value::String(compressed);
                } else {
                    let (compressed, orig, new) = compress_user_text_return(&t, config, acc);
                    acc.original_tokens += orig;
                    acc.compressed_tokens += new;
                    block["text"] = Value::String(compressed);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::proxy::compress_messages;
    use crate::proxy::tests_util::test_config;

    #[test]
    fn tool_use_hint_routes_tool_result_string() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "find . -type f"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
                ]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn read_tool_hint_routes_tool_result_array() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/repo/src/main.rs"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_2", "content": [
                        {"type": "text", "text": "fn main() {}\n"}
                    ]}
                ]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-read"));
    }
}
