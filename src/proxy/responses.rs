//! OpenAI Responses API handler (Codex).
//!
//! Shape: `{"input": "string" | [items]}` where items are
//! `{"role": ..., "content": ...}` or `{"type": "function_call"|"function_call_output", ...}`.
//!
//! This path already associates each `function_call` (carrying the executed
//! command) with its `function_call_output` via `call_id`, and additionally
//! tracks Codex's `write_stdin` polling across requests using a persisted
//! session-id → command map. That cross-request state is unique to Codex's
//! stdin-polling tool and stays local to this module.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use toksqz::compression;

use super::shared::{
    account_passthrough, compress_tool_output, estimate_tokens, CompressResult,
};

static SESSION_COMMANDS: Lazy<RwLock<HashMap<i64, String>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

static SESSION_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)Process (?:running with )?session ID (\d+)").unwrap());

static SHELL_PROMPT_PREFIX_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(?:\$|%|#)\s*").unwrap());

static SESSION_COMMAND_IGNORE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^(?:q|quit|exit|done|PY|EOF|\\x03|\\u0003|>|>>)$").unwrap());

static SESSION_STATE_COMMAND_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^(?:cd\b|export\b|set\b|unset\b|source\b|bash\b|zsh\b|sh\b|pwd\b|mkdir\b)$")
        .unwrap()
});

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PersistedSessionCommand {
    session_id: i64,
    command: String,
    updated_at: u64,
}

fn extract_command_hint(item: &Value) -> Option<String> {
    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match item_type {
        "function_call" => {
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = item.get("arguments")?;
            let args = if let Some(s) = arguments.as_str() {
                serde_json::from_str::<Value>(s).ok()?
            } else {
                arguments.clone()
            };
            match name {
                "exec_command" | "shell_command" => args
                    .get("cmd")
                    .or_else(|| args.get("command"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                "write_stdin" => args
                    .get("session_id")
                    .and_then(|v| v.as_i64())
                    .map(|id| format!("write_stdin:{id}")),
                _ => None,
            }
        }
        _ => None,
    }
}

fn extract_session_command_from_stdin(item: &Value) -> Option<(i64, String)> {
    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if item_type != "function_call" {
        return None;
    }
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name != "write_stdin" {
        return None;
    }
    let arguments = item.get("arguments")?;
    let args = if let Some(s) = arguments.as_str() {
        serde_json::from_str::<Value>(s).ok()?
    } else {
        arguments.clone()
    };
    let session_id = args.get("session_id").and_then(|v| v.as_i64())?;
    let chars = args.get("chars").and_then(|v| v.as_str())?;
    let trimmed = chars.trim();
    if trimmed.is_empty() || trimmed.contains('\u{3}') {
        return None;
    }
    let candidate_line = trimmed
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())?
        .trim();
    let command = SHELL_PROMPT_PREFIX_RE.replace(candidate_line, "").to_string();
    if command.is_empty() {
        return None;
    }
    if SESSION_COMMAND_IGNORE_RE.is_match(command.trim()) {
        return None;
    }
    let normalized = compression::command_detector::normalize_command_for_hint(&command);
    if normalized.is_empty() || SESSION_COMMAND_IGNORE_RE.is_match(normalized.trim()) {
        return None;
    }
    let head = normalized.split_whitespace().next().unwrap_or("");
    if SESSION_STATE_COMMAND_RE.is_match(head) {
        return None;
    }
    Some((session_id, normalized))
}

fn extract_session_id_from_output(output: &str) -> Option<i64> {
    SESSION_ID_RE
        .captures(output)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok())
}

fn session_command_ttl() -> Duration {
    std::env::var("SQUEEZE_SESSION_COMMAND_TTL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(86400))
}

fn session_state_path() -> PathBuf {
    std::env::var("SQUEEZE_SESSION_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("toksqz-session-commands.json"))
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn load_persisted_session_commands_from(path: &PathBuf, ttl_secs: u64) -> HashMap<i64, String> {
    let now = now_epoch_secs();
    let content = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    let items: Vec<PersistedSessionCommand> = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };
    items
        .into_iter()
        .filter(|item| now.saturating_sub(item.updated_at) <= ttl_secs)
        .map(|item| (item.session_id, item.command))
        .collect()
}

fn load_persisted_session_commands() -> HashMap<i64, String> {
    load_persisted_session_commands_from(&session_state_path(), session_command_ttl().as_secs())
}

fn persist_session_commands_to(path: &PathBuf, ttl_secs: u64, map: &HashMap<i64, String>) {
    let now = now_epoch_secs();
    let mut items: Vec<PersistedSessionCommand> = map
        .iter()
        .map(|(session_id, command)| PersistedSessionCommand {
            session_id: *session_id,
            command: command.clone(),
            updated_at: now,
        })
        .collect();
    items.retain(|item| now.saturating_sub(item.updated_at) <= ttl_secs);
    if let Ok(serialized) = serde_json::to_string(&items) {
        let _ = std::fs::write(path, serialized);
    }
}

fn persist_session_commands(map: &HashMap<i64, String>) {
    persist_session_commands_to(&session_state_path(), session_command_ttl().as_secs(), map);
}

fn remember_session_command(session_id: i64, command: &str) {
    if let Ok(mut map) = SESSION_COMMANDS.write() {
        map.insert(session_id, command.to_string());
        persist_session_commands(&map);
    }
}

fn lookup_session_command(session_id: i64) -> Option<String> {
    if let Ok(map) = SESSION_COMMANDS.read() {
        if let Some(command) = map.get(&session_id).cloned() {
            return Some(command);
        }
    }
    let persisted = load_persisted_session_commands();
    if persisted.is_empty() {
        return None;
    }
    if let Ok(mut map) = SESSION_COMMANDS.write() {
        for (sid, command) in persisted {
            map.entry(sid).or_insert(command);
        }
        return map.get(&session_id).cloned();
    }
    None
}

pub(crate) fn compress(payload: &mut Value, config: &crate::Config) -> Option<CompressResult> {
    let input = payload.get_mut("input")?;
    let mut acc = CompressResult::new();
    let mut call_commands: HashMap<String, String> = HashMap::new();
    let mut session_commands: HashMap<i64, String> = HashMap::new();

    match input {
        // String input — treat as single user message. Do not Caveman-compress:
        // Codex Responses API relies on exact prompt bytes for deferred tools.
        Value::String(s) if !s.is_empty() => {
            account_passthrough(s, &mut acc);
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if item_type == "function_call" {
                    if let Some((session_id, command)) = extract_session_command_from_stdin(item) {
                        session_commands.insert(session_id, command.clone());
                        remember_session_command(session_id, &command);
                    }
                    if let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) {
                        if let Some(command_hint) = extract_command_hint(item) {
                            if let Some(session_id) = command_hint
                                .strip_prefix("write_stdin:")
                                .and_then(|s| s.parse::<i64>().ok())
                            {
                                if let Some(mapped) = session_commands
                                    .get(&session_id)
                                    .cloned()
                                    .or_else(|| lookup_session_command(session_id))
                                {
                                    call_commands.insert(call_id.to_string(), mapped);
                                }
                            } else {
                                call_commands.insert(call_id.to_string(), command_hint.clone());
                                if let Some(args) = item.get("arguments") {
                                    let parsed_args = if let Some(s) = args.as_str() {
                                        serde_json::from_str::<Value>(s).ok()
                                    } else {
                                        Some(args.clone())
                                    };
                                    if let Some(parsed) = parsed_args {
                                        if let Some(session_id) =
                                            parsed.get("session_id").and_then(|v| v.as_i64())
                                        {
                                            session_commands.insert(session_id, command_hint);
                                        }
                                    }
                                }
                                if let Some(session_id) = item
                                    .get("arguments")
                                    .and_then(|args| {
                                        if let Some(s) = args.as_str() {
                                            serde_json::from_str::<Value>(s).ok()
                                        } else {
                                            Some(args.clone())
                                        }
                                    })
                                    .and_then(|parsed| {
                                        parsed.get("session_id").and_then(|v| v.as_i64())
                                    })
                                {
                                    if let Some(mapped) = lookup_session_command(session_id) {
                                        call_commands.insert(call_id.to_string(), mapped);
                                    }
                                }
                            }
                        }
                    }
                }

                if item_type == "function_call_output" {
                    let command_hint_owned = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .and_then(|call_id| call_commands.get(call_id))
                        .cloned();
                    if let Some(output) = item.get_mut("output") {
                        if let Some(s) = output.as_str() {
                            let text = s.to_string();
                            if let Some(command_hint) = command_hint_owned.as_deref() {
                                if let Some(session_id) = extract_session_id_from_output(&text) {
                                    remember_session_command(session_id, command_hint);
                                }
                            }
                            let compressed = compress_tool_output(
                                &text,
                                command_hint_owned.as_deref(),
                                config,
                                &mut acc,
                            );
                            if compressed != text
                                && estimate_tokens(&compressed) < estimate_tokens(&text)
                            {
                                acc.output_patches.push((text, compressed.clone()));
                            }
                            *output = Value::String(compressed);
                        }
                    }
                } else {
                    // Regular message: {"role": ..., "content": "..." | [parts]}
                    let role = item
                        .get("role")
                        .and_then(|r| r.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some(content) = item.get_mut("content") {
                        match content {
                            Value::String(s) if !s.is_empty() => {
                                if role == "user" {
                                    // Never Caveman-compress Codex user messages (includes
                                    // injected AGENTS.md). RTK only applies to tool output.
                                    account_passthrough(s, &mut acc);
                                } else {
                                    account_passthrough(s, &mut acc);
                                }
                            }
                            Value::Array(parts) => {
                                for part in parts.iter_mut() {
                                    let text = match part.get("text").and_then(|v| v.as_str()) {
                                        Some(s) if !s.is_empty() => s.to_string(),
                                        _ => continue,
                                    };
                                    if role == "user" {
                                        account_passthrough(&text, &mut acc);
                                    } else {
                                        account_passthrough(&text, &mut acc);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        _ => {}
    }

    Some(acc)
}

/// Patch only `function_call_output.output` string values in the original JSON
/// bytes. Preserves top-level key order and the `tools` array exactly — Codex
/// deferred tools (`spawn_agent`, `tool_search`) break when the full body is
/// re-serialized via `serde_json::to_vec`.
pub(crate) fn apply_output_patches(
    original: &[u8],
    patches: &[(String, String)],
) -> Option<Vec<u8>> {
    if patches.is_empty() {
        return Some(original.to_vec());
    }
    let mut body = std::str::from_utf8(original).ok()?.to_string();
    for (before, after) in patches {
        if before == after {
            continue;
        }
        let before_json = serde_json::to_string(before).ok()?;
        let after_json = serde_json::to_string(after).ok()?;
        let mut replaced = false;
        for sep in ["", " "] {
            let needle = format!(r#""output":{sep}{before_json}"#);
            if let Some(idx) = body.find(&needle) {
                let patched = format!(r#""output":{sep}{after_json}"#);
                body.replace_range(idx..idx + needle.len(), &patched);
                replaced = true;
                break;
            }
        }
        if !replaced {
            return None;
        }
    }
    Some(body.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::compress_messages;
    use crate::proxy::tests_util::test_config;

    #[test]
    fn test_apply_output_patches_preserves_tools_key_order() {
        let original = br#"{"model":"gpt-5.5","tools":[{"type":"namespace","name":"multi_agent_v1"}],"input":[{"type":"function_call_output","call_id":"c1","output":"OLD_OUTPUT_TEXT"}]}"#;
        let patched = apply_output_patches(
            original,
            &[("OLD_OUTPUT_TEXT".into(), "NEW_OUTPUT".into())],
        )
        .unwrap();
        let patched_str = std::str::from_utf8(&patched).unwrap();
        assert!(patched_str.contains(r#""output":"NEW_OUTPUT""#));
        assert!(patched_str.find("\"tools\"").unwrap() < patched_str.find("\"input\"").unwrap());
        assert!(!patched_str.contains("OLD_OUTPUT_TEXT"));
    }

    #[test]
    fn test_responses_user_only_request_returns_none() {
        let mut payload = serde_json::json!({
            "model": "gpt-5.5",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "Spawn one explorer agent to inspect this repo"}
                ]}
            ]
        });
        let acc = compress_messages(&mut payload, &test_config()).expect("responses format");
        assert!(
            acc.finish().is_none(),
            "Codex user-only turns must forward original JSON bytes (deferred tools)"
        );
    }

    #[test]
    fn test_responses_exec_command_hint_drives_detection() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_exec", "name": "exec_command", "arguments": "{\"cmd\":\"find . -type f\"}"},
                {"type": "function_call_output", "call_id": "call_exec", "output": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn test_responses_write_stdin_inherits_session_command() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_exec", "name": "exec_command", "arguments": "{\"cmd\":\"find . -type f\",\"session_id\":42}"},
                {"type": "function_call", "call_id": "call_poll", "name": "write_stdin", "arguments": "{\"chars\":\"\",\"session_id\":42}"},
                {"type": "function_call_output", "call_id": "call_poll", "output": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn test_responses_write_stdin_uses_runtime_session_mapping() {
        let mut seed_payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_exec_seed", "name": "exec_command", "arguments": "{\"cmd\":\"find . -type f\"}"},
                {"type": "function_call_output", "call_id": "call_exec_seed", "output": "Chunk ID: abc123\nWall time: 0.1000 seconds\nProcess running with session ID 77\nOriginal token count: 0\nOutput:\n"}
            ]
        });
        let _ = compress_messages(&mut seed_payload, &test_config()).unwrap();

        let mut poll_payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_poll_seeded", "name": "write_stdin", "arguments": "{\"chars\":\"\",\"session_id\":77}"},
                {"type": "function_call_output", "call_id": "call_poll_seeded", "output": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}
            ]
        });
        let result = compress_messages(&mut poll_payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn test_lookup_session_command_uses_persisted_state() {
        let state_path =
            std::env::temp_dir().join(format!("toksqz-session-state-test-{}.json", std::process::id()));
        let payload = vec![PersistedSessionCommand {
            session_id: 501,
            command: "find . -type f".to_string(),
            updated_at: now_epoch_secs(),
        }];
        std::fs::write(&state_path, serde_json::to_string(&payload).unwrap()).unwrap();
        let loaded = load_persisted_session_commands_from(&state_path, 3600);
        let command = loaded.get(&501).cloned();
        assert_eq!(command.as_deref(), Some("find . -type f"));
        let _ = std::fs::remove_file(&state_path);
    }

    #[test]
    fn test_write_stdin_chars_refreshes_session_command() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_seed", "name": "exec_command", "arguments": "{\"cmd\":\"find . -type f\"}"},
                {"type": "function_call_output", "call_id": "call_seed", "output": "Chunk ID: abc123\nProcess running with session ID 88\nOutput:\n"},
                {"type": "function_call", "call_id": "call_write", "name": "write_stdin", "arguments": "{\"chars\":\"python script.py\\n\",\"session_id\":88}"},
                {"type": "function_call_output", "call_id": "call_write", "output": "Traceback (most recent call last):\n  File \"script.py\", line 1, in <module>\nModuleNotFoundError: No module named 'demo'\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "python-script"));
    }

    #[test]
    fn test_write_stdin_chars_normalizes_shell_wrapped_command() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "call_seed", "name": "exec_command", "arguments": "{\"cmd\":\"find . -type f\"}"},
                {"type": "function_call_output", "call_id": "call_seed", "output": "Chunk ID: abc123\nProcess running with session ID 89\nOutput:\n"},
                {"type": "function_call", "call_id": "call_write", "name": "write_stdin", "arguments": "{\"chars\":\"cd repo && rg --files src\\n\",\"session_id\":89}"},
                {"type": "function_call_output", "call_id": "call_write", "output": "./src/main.rs\n./src/lib.rs\n"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.per_command.iter().any(|c| c.command_type == "file-list"));
    }

    #[test]
    fn test_write_stdin_ignores_single_letter_quit_commands() {
        let item = serde_json::json!({"type": "function_call", "name": "write_stdin", "arguments": "{\"chars\":\"q\\n\",\"session_id\":90}"});
        assert!(extract_session_command_from_stdin(&item).is_none());
    }

    #[test]
    fn test_write_stdin_ignores_heredoc_markers() {
        let item = serde_json::json!({"type": "function_call", "name": "write_stdin", "arguments": "{\"chars\":\"PY\\n\",\"session_id\":91}"});
        assert!(extract_session_command_from_stdin(&item).is_none());
    }

    #[test]
    fn test_write_stdin_ignores_cd_state_changes() {
        let item = serde_json::json!({"type": "function_call", "name": "write_stdin", "arguments": "{\"chars\":\"cd repo\\n\",\"session_id\":92}"});
        assert!(extract_session_command_from_stdin(&item).is_none());
    }

    #[test]
    fn test_write_stdin_ignores_export_state_changes() {
        let item = serde_json::json!({"type": "function_call", "name": "write_stdin", "arguments": "{\"chars\":\"export TMPDIR=/tmp/cache\\n\",\"session_id\":93}"});
        assert!(extract_session_command_from_stdin(&item).is_none());
    }
}
