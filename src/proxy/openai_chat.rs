//! OpenAI Chat Completions handler.
//!
//! Shape: `{"messages": [{"role": "tool"|"user"|"assistant"|"system", "content": "..."}]}`.
//! Tool calls live on the assistant message as `tool_calls[].id` +
//! `function.{name,arguments}`; the matching output arrives as a `tool` message
//! whose `tool_call_id` references that id. We do a first pass to build
//! `tool_call_id → synthesized command hint`, then a second pass to compress
//! each tool output with the precise hint (falling back to content-based
//! detection when no hint is available).

use std::collections::HashMap;

use serde_json::Value;

use super::hint;
use super::shared::{
    account_passthrough, compress_tool_output, compress_user_text_return, CompressResult,
};

fn collect_hints(payload: &Value) -> HashMap<String, String> {
    let mut hints: HashMap<String, String> = HashMap::new();
    let messages = match payload.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return hints,
    };
    for msg in messages {
        let tool_calls = match msg.get("tool_calls").and_then(|v| v.as_array()) {
            Some(tc) => tc,
            None => continue,
        };
        for tc in tool_calls {
            let id = match tc.get("id").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            let func = tc.get("function");
            let name = func
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = hint::parse_args(func.and_then(|f| f.get("arguments")));
            if let Some(h) = hint::synthesize(name, &args) {
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
        let content = match msg.get("content").and_then(|c| c.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        if role == "tool" {
            let hint = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .and_then(|id| hints.get(id))
                .cloned();
            let compressed = compress_tool_output(&content, hint.as_deref(), config, &mut acc);
            msg["content"] = Value::String(compressed);
        } else if role == "user" {
            let (compressed, orig, new) = compress_user_text_return(&content, config, &mut acc);
            acc.original_tokens += orig;
            acc.compressed_tokens += new;
            if config.log_enabled && compressed.len() < content.len() {
                println!("  [Caveman] {}→{} chars", content.len(), compressed.len());
            }
            msg["content"] = Value::String(compressed);
        } else {
            account_passthrough(&content, &mut acc);
        }
    }

    acc.finish()
}

#[cfg(test)]
mod tests {
    use crate::proxy::compress_messages;
    use crate::proxy::tests_util::test_config;

    #[test]
    fn bash_tool_call_hint_routes_output() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "Bash", "arguments": "{\"command\":\"find . -type f\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn tool_output_without_matching_call_falls_back() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "tool", "tool_call_id": "missing", "content": "some unrecognized text output\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_none());
    }
}
