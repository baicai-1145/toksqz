use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde_json::Value;

use toksqz::compression;
use crate::Config;

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("failed to build HTTP client")
});

struct CompressResult {
    original_tokens: usize,
    compressed_tokens: usize,
    filters_applied: Vec<String>,
    per_command: Vec<compression::stats::CommandStats>,
}

pub async fn health(State(config): State<Config>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::json!({
            "status": "ok",
            "upstream": config.upstream,
            "rtk": config.rtk_enabled,
            "caveman": config.caveman_level.as_deref().unwrap_or("off"),
            "grouping": config.grouping_enabled,
            "stats": config.stats_enabled,
        })
        .to_string(),
    )
}

pub async fn stats(State(config): State<Config>) -> impl IntoResponse {
    if !config.stats_enabled {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            "{\"error\":\"stats disabled\"}".to_string(),
        );
    }
    let summary = compression::stats::get_summary();
    let filter_hits: Vec<serde_json::Value> = summary.filter_hits.iter()
        .take(20)
        .map(|(id, count)| serde_json::json!({"filter": id, "hits": count}))
        .collect();
    let command_hits: Vec<serde_json::Value> = summary.command_hits.iter()
        .take(20)
        .map(|h| serde_json::json!({
            "command_type": h.command_type,
            "hits": h.hits,
            "saved_tokens": h.saved_tokens,
        }))
        .collect();
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::json!({
            "total_requests": summary.total_requests,
            "total_original_tokens": summary.total_original_tokens,
            "total_compressed_tokens": summary.total_compressed_tokens,
            "total_saved_tokens": summary.total_saved_tokens,
            "avg_savings_pct": format!("{:.1}", summary.avg_savings_pct),
            "top_filters": filter_hits,
            "top_commands": command_hits,
        })
        .to_string(),
    )
}

pub async fn handle(
    State(config): State<Config>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    method: axum::http::Method,
    body: Body,
) -> impl IntoResponse {
    // Read request body
    let bytes = match axum::body::to_bytes(body, 100 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                format!("{{\"error\":\"Body too large: {}\"}}", e),
            )
                .into_response();
        }
    };

    let mut payload: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "application/json")],
                "{\"error\":\"Invalid JSON\"}".to_string(),
            )
                .into_response();
        }
    };

    // Compress messages
    let result = compress_messages(&mut payload, &config);

    if config.log_enabled {
        if let Some(ref r) = result {
            if r.original_tokens > 0 {
                let saved = r.original_tokens.saturating_sub(r.compressed_tokens);
                if saved > 0 {
                    let pct = (saved as f64 / r.original_tokens as f64) * 100.0;
                    println!(
                        "  压缩: {} → {} tokens (省 {:.1}%)",
                        r.original_tokens, r.compressed_tokens, pct
                    );
                } else {
                    println!("  无需压缩 ({} tokens)", r.original_tokens);
                }
            }
        }
    }

    // Build upstream URL
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or(uri.path());
    let url = format!("{}{}", config.upstream, path_and_query);

    // Forward headers
    let mut upstream_headers = reqwest::header::HeaderMap::new();
    upstream_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    if let Some(auth) = headers.get("authorization") {
        if let Ok(val) = reqwest::header::HeaderValue::from_bytes(auth.as_bytes()) {
            upstream_headers.insert(reqwest::header::AUTHORIZATION, val);
        }
    }
    if let Some(rid) = headers.get("x-request-id") {
        if let Ok(val) = reqwest::header::HeaderValue::from_bytes(rid.as_bytes()) {
            upstream_headers.insert("x-request-id", val);
        }
    }

    let body_bytes = serde_json::to_vec(&payload).unwrap_or_default();

    let upstream_resp = match CLIENT
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST),
            &url,
        )
        .headers(upstream_headers)
        .body(body_bytes)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [("content-type", "application/json")],
                format!("{{\"error\":{{\"message\":\"Proxy error: {}\",\"type\":\"proxy_error\"}}}}", e),
            )
                .into_response();
        }
    };

    // Build response headers
    let mut response_headers = HeaderMap::new();
    for (key, value) in upstream_resp.headers().iter() {
        if is_hop_by_hop(key.as_str()) {
            continue;
        }
        if let Ok(hname) = axum::http::header::HeaderName::from_bytes(key.as_str().as_bytes()) {
            if let Ok(hval) = axum::http::header::HeaderValue::from_bytes(value.as_bytes()) {
                response_headers.insert(hname, hval);
            }
        }
    }
    if let Some(ref r) = result {
        response_headers.insert(
            "x-squeeze-original-tokens",
            r.original_tokens.to_string().parse().unwrap(),
        );
        response_headers.insert(
            "x-squeeze-compressed-tokens",
            r.compressed_tokens.to_string().parse().unwrap(),
        );
        // Extended stats headers
        if !r.filters_applied.is_empty() {
            let filters_str = r.filters_applied.join(",");
            if let Ok(val) = filters_str.parse() {
                response_headers.insert("x-toksqz-filters-applied", val);
            }
        }
        if !r.per_command.is_empty() {
            let per_cmd_str: Vec<String> = r.per_command.iter()
                .map(|c| format!("{}:{}->{},", c.command_type, c.original_tokens, c.compressed_tokens))
                .collect();
            let joined: String = per_cmd_str.join("");
            // Truncate header to 8KB max
            let truncated = if joined.len() > 8192 { &joined[..8192] } else { &joined };
            if let Ok(val) = truncated.parse() {
                response_headers.insert("x-toksqz-per-command", val);
            }
        }
    }

    let status = StatusCode::from_u16(upstream_resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    // Stream response body (supports SSE)
    let stream = upstream_resp.bytes_stream().map(|result| {
        result.map(|bytes| bytes.to_vec()).map_err(|e| {
            eprintln!("  Stream error: {}", e);
            std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
        })
    });

    (status, response_headers, Body::from_stream(stream)).into_response()
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "transfer-encoding" | "connection" | "keep-alive"
    )
}

fn estimate_tokens(text: &str) -> usize {
    // 与 Node.js Math.ceil(str.length / 4) 对齐：
    // JS str.length = UTF-16 代码单元数，用 chars().count() 近似（仅 emoji 等 BMP 外字符有差异）
    (text.chars().count() + 3) / 4
}

// ─── Multi-format compression ─────────────────────────────────────────────

/// Detect API format and dispatch to the appropriate handler.
fn compress_messages(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    // 1. Google Gemini: {"contents": [{"role": ..., "parts": [{"text": ...}]}]}
    if payload.get("contents").and_then(|v| v.as_array()).is_some() {
        return compress_gemini(payload, config);
    }
    // 2. OpenAI Responses API: {"input": [...]}
    if payload.get("input").and_then(|v| v.as_array()).is_some() {
        return compress_responses_api(payload, config);
    }
    // 3. Has "messages" array → OpenAI Chat or Anthropic Messages
    if payload.get("messages").and_then(|v| v.as_array()).is_some() {
        // Anthropic detection: check if any message has array content (content blocks)
        let is_anthropic = payload["messages"]
            .as_array()?
            .iter()
            .any(|m| m.get("content").and_then(|c| c.as_array()).is_some());
        if is_anthropic {
            return compress_anthropic(payload, config);
        }
        // Default: OpenAI Chat Completions
        return compress_openai(payload, config);
    }
    None
}

/// Helper: compress a tool-output string via RTK, record stats. Returns compressed text.
fn compress_tool_output(
    content: &str,
    config: &Config,
    original_tokens: &mut usize,
    compressed_tokens: &mut usize,
    filters_applied: &mut Vec<String>,
    per_command: &mut Vec<compression::stats::CommandStats>,
) -> String {
    let orig = estimate_tokens(content);
    *original_tokens += orig;
    if !config.rtk_enabled {
        *compressed_tokens += orig;
        return content.to_string();
    }
    let result = compression::rtk_compress_full(content);
    let new_tokens = estimate_tokens(&result.text);
    if config.log_enabled && result.text.len() < content.len() {
        println!(
            "  [RTK] {}→{} chars{}{}",
            content.len(),
            result.text.len(),
            result.filter_id.as_ref().map(|fid| format!(" ({})", fid)).unwrap_or_default(),
            if result.grouping_applied { " [grouped]" } else { "" },
        );
    }
    let filter_id = result.filter_id.clone().unwrap_or_else(|| "none".to_string());
    compression::stats::record_message(&filter_id, &result.command_type, orig, new_tokens);
    if let Some(ref fid) = result.filter_id {
        if !filters_applied.contains(fid) {
            filters_applied.push(fid.clone());
        }
    }
    per_command.push(compression::stats::CommandStats {
        command_type: result.command_type.clone(),
        filter_id,
        original_tokens: orig,
        compressed_tokens: new_tokens,
    });
    *compressed_tokens += new_tokens;
    result.text
}

/// Compress user text and return the compressed string.
fn compress_user_text_return(content: &str, config: &Config) -> (String, usize, usize) {
    let orig = estimate_tokens(content);
    if let Some(ref level) = config.caveman_level {
        let compressed = compression::caveman_compress(content, level);
        let new_tokens = estimate_tokens(&compressed);
        (compressed, orig, new_tokens)
    } else {
        (content.to_string(), orig, orig)
    }
}

// ─── OpenAI Chat Completions ──────────────────────────────────────────────
// {"messages": [{"role": "tool"|"user"|"assistant"|"system", "content": "..."}]}

fn compress_openai(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    let messages = payload.get_mut("messages")?.as_array_mut()?;
    let mut original_tokens: usize = 0;
    let mut compressed_tokens: usize = 0;
    let mut filters_applied: Vec<String> = Vec::new();
    let mut per_command: Vec<compression::stats::CommandStats> = Vec::new();

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = match msg.get("content").and_then(|c| c.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        if role == "tool" {
            let compressed = compress_tool_output(&content, config, &mut original_tokens, &mut compressed_tokens, &mut filters_applied, &mut per_command);
            msg["content"] = Value::String(compressed);
        } else if role == "user" {
            let (compressed, orig, new) = compress_user_text_return(&content, config);
            original_tokens += orig;
            compressed_tokens += new;
            if config.log_enabled && compressed.len() < content.len() {
                println!("  [Caveman] {}→{} chars", content.len(), compressed.len());
            }
            msg["content"] = Value::String(compressed);
        } else {
            let orig = estimate_tokens(&content);
            original_tokens += orig;
            compressed_tokens += orig;
        }
    }

    Some(CompressResult { original_tokens, compressed_tokens, filters_applied, per_command })
}

// ─── Anthropic Messages API ───────────────────────────────────────────────
// {"messages": [{"role": "user"|"assistant", "content": "str" | [blocks]}]}
// Blocks: {"type": "text", "text": "..."}
//         {"type": "tool_result", "tool_use_id": "...", "content": "str" | [blocks]}

fn compress_anthropic(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    let messages = payload.get_mut("messages")?.as_array_mut()?;
    let mut original_tokens: usize = 0;
    let mut compressed_tokens: usize = 0;
    let mut filters_applied: Vec<String> = Vec::new();
    let mut per_command: Vec<compression::stats::CommandStats> = Vec::new();

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let content = match msg.get_mut("content") {
            Some(c) => c,
            None => continue,
        };

        match content {
            // String content: {"role": "user", "content": "hello"}
            Value::String(s) if !s.is_empty() => {
                let text = s.clone();
                if role == "user" {
                    let (compressed, orig, new) = compress_user_text_return(&text, config);
                    original_tokens += orig;
                    compressed_tokens += new;
                    *s = compressed;
                } else {
                    let orig = estimate_tokens(&text);
                    original_tokens += orig;
                    compressed_tokens += orig;
                }
            }
            // Array content: {"role": "user", "content": [{"type": "tool_result", ...}, {"type": "text", ...}]}
            Value::Array(blocks) => {
                for block in blocks.iter_mut() {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match block_type {
                        "tool_result" => {
                            // tool_result.content can be string or array of blocks
                            if let Some(tc) = block.get_mut("content") {
                                compress_anthropic_content(tc, true, config, &mut original_tokens, &mut compressed_tokens, &mut filters_applied, &mut per_command);
                            }
                        }
                        "text" => {
                            if let Some(text_val) = block.get_mut("text") {
                                if let Some(s) = text_val.as_str() {
                                    let text = s.to_string();
                                    if role == "user" {
                                        let (compressed, orig, new) = compress_user_text_return(&text, config);
                                        original_tokens += orig;
                                        compressed_tokens += new;
                                        *text_val = Value::String(compressed);
                                    } else {
                                        let orig = estimate_tokens(&text);
                                        original_tokens += orig;
                                        compressed_tokens += orig;
                                    }
                                }
                            }
                        }
                        _ => {
                            // Other block types (image, tool_use, etc.) — skip
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Some(CompressResult { original_tokens, compressed_tokens, filters_applied, per_command })
}

/// Recursively compress Anthropic content (can be string or array of blocks).
fn compress_anthropic_content(
    content: &mut Value,
    is_tool: bool,
    config: &Config,
    original_tokens: &mut usize,
    compressed_tokens: &mut usize,
    filters_applied: &mut Vec<String>,
    per_command: &mut Vec<compression::stats::CommandStats>,
) {
    match content {
        Value::String(s) if !s.is_empty() => {
            let text = s.clone();
            if is_tool {
                let orig = estimate_tokens(&text);
                *original_tokens += orig;
                if config.rtk_enabled {
                    let result = compression::rtk_compress_full(&text);
                    let new_tokens = estimate_tokens(&result.text);
                    if config.log_enabled && result.text.len() < text.len() {
                        println!("  [RTK] {}→{} chars", text.len(), result.text.len());
                    }
                    let filter_id = result.filter_id.clone().unwrap_or_else(|| "none".to_string());
                    compression::stats::record_message(&filter_id, &result.command_type, orig, new_tokens);
                    if let Some(ref fid) = result.filter_id {
                        if !filters_applied.contains(fid) { filters_applied.push(fid.clone()); }
                    }
                    per_command.push(compression::stats::CommandStats {
                        command_type: result.command_type.clone(),
                        filter_id,
                        original_tokens: orig,
                        compressed_tokens: new_tokens,
                    });
                    *compressed_tokens += new_tokens;
                    *s = result.text;
                } else {
                    *compressed_tokens += orig;
                }
            } else {
                let (compressed, orig, new) = compress_user_text_return(&text, config);
                *original_tokens += orig;
                *compressed_tokens += new;
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
                    let compressed = compress_tool_output(&t, config, original_tokens, compressed_tokens, filters_applied, per_command);
                    block["text"] = Value::String(compressed);
                } else {
                    let (compressed, orig, new) = compress_user_text_return(&t, config);
                    *original_tokens += orig;
                    *compressed_tokens += new;
                    block["text"] = Value::String(compressed);
                }
            }
        }
        _ => {}
    }
}

// ─── Google Gemini API ────────────────────────────────────────────────────
// {"contents": [{"role": "user"|"model", "parts": [{"text": "..."}]}]}

fn compress_gemini(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    let contents = payload.get_mut("contents")?.as_array_mut()?;
    let mut original_tokens: usize = 0;
    let mut compressed_tokens: usize = 0;
    let mut filters_applied: Vec<String> = Vec::new();
    let mut per_command: Vec<compression::stats::CommandStats> = Vec::new();

    for content in contents.iter_mut() {
        let role = content.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
        let parts = match content.get_mut("parts").and_then(|p| p.as_array_mut()) {
            Some(p) => p,
            None => continue,
        };

        for part in parts.iter_mut() {
            // Check if this is a functionResponse part (tool-like output)
            let is_function_response = part.get("functionResponse").is_some();
            let is_tool_like = is_function_response || role == "function";

            // Only process parts that have a "text" field
            let text = match part.get("text").and_then(|t| t.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };

            if is_tool_like {
                let compressed = compress_tool_output(&text, config, &mut original_tokens, &mut compressed_tokens, &mut filters_applied, &mut per_command);
                part["text"] = Value::String(compressed);
            } else if role == "user" {
                let (compressed, orig, new) = compress_user_text_return(&text, config);
                original_tokens += orig;
                compressed_tokens += new;
                if config.log_enabled && compressed.len() < text.len() {
                    println!("  [Caveman/Gemini] {}→{} chars", text.len(), compressed.len());
                }
                part["text"] = Value::String(compressed);
            } else {
                let orig = estimate_tokens(&text);
                original_tokens += orig;
                compressed_tokens += orig;
            }
        }
    }

    Some(CompressResult { original_tokens, compressed_tokens, filters_applied, per_command })
}

// ─── OpenAI Responses API ─────────────────────────────────────────────────
// {"input": "string" | [items]}
// Items: {"role": "user"|"system"|"developer", "content": "str" | [parts]}
//        {"type": "function_call_output", "output": "..."}

fn compress_responses_api(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    let input = payload.get_mut("input")?;
    let mut original_tokens: usize = 0;
    let mut compressed_tokens: usize = 0;
    let mut filters_applied: Vec<String> = Vec::new();
    let mut per_command: Vec<compression::stats::CommandStats> = Vec::new();

    match input {
        // String input — treat as single user message
        Value::String(s) if !s.is_empty() => {
            let text = s.clone();
            let (compressed, orig, new) = compress_user_text_return(&text, config);
            original_tokens += orig;
            compressed_tokens += new;
            *s = compressed;
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if item_type == "function_call_output" {
                    // {"type": "function_call_output", "output": "..."}
                    if let Some(output) = item.get_mut("output") {
                        if let Some(s) = output.as_str() {
                            let text = s.to_string();
                            let orig = estimate_tokens(&text);
                            original_tokens += orig;
                            if config.rtk_enabled {
                                let result = compression::rtk_compress_full(&text);
                                let new_tokens = estimate_tokens(&result.text);
                                if config.log_enabled && result.text.len() < text.len() {
                                    println!("  [RTK/Responses] {}→{} chars", text.len(), result.text.len());
                                }
                                let filter_id = result.filter_id.clone().unwrap_or_else(|| "none".to_string());
                                compression::stats::record_message(&filter_id, &result.command_type, orig, new_tokens);
                                if let Some(ref fid) = result.filter_id {
                                    if !filters_applied.contains(fid) { filters_applied.push(fid.clone()); }
                                }
                                per_command.push(compression::stats::CommandStats {
                                    command_type: result.command_type.clone(),
                                    filter_id,
                                    original_tokens: orig,
                                    compressed_tokens: new_tokens,
                                });
                                compressed_tokens += new_tokens;
                                *output = Value::String(result.text);
                            } else {
                                compressed_tokens += orig;
                            }
                        }
                    }
                } else {
                    // Regular message: {"role": "user"|"system"|"developer", "content": "..." | [parts]}
                    let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("").to_string();
                    if let Some(content) = item.get_mut("content") {
                        match content {
                            Value::String(s) if !s.is_empty() => {
                                let text = s.clone();
                                if role == "user" {
                                    let (compressed, orig, new) = compress_user_text_return(&text, config);
                                    original_tokens += orig;
                                    compressed_tokens += new;
                                    *s = compressed;
                                } else {
                                    let orig = estimate_tokens(&text);
                                    original_tokens += orig;
                                    compressed_tokens += orig;
                                }
                            }
                            Value::Array(parts) => {
                                for part in parts.iter_mut() {
                                    let text = match part.get("text").and_then(|v| v.as_str()) {
                                        Some(s) if !s.is_empty() => s.to_string(),
                                        _ => continue,
                                    };
                                    if role == "user" {
                                        let (compressed, orig, new) = compress_user_text_return(&text, config);
                                        original_tokens += orig;
                                        compressed_tokens += new;
                                        part["text"] = Value::String(compressed);
                                    } else {
                                        let orig = estimate_tokens(&text);
                                        original_tokens += orig;
                                        compressed_tokens += orig;
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

    Some(CompressResult { original_tokens, compressed_tokens, filters_applied, per_command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            upstream: "http://localhost:9999".into(),
            port: 8787,
            rtk_enabled: true,
            caveman_level: Some("lite".into()),
            log_enabled: false,
            grouping_enabled: true,
            stats_enabled: false,
        }
    }

    #[test]
    fn test_detect_openai_format() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hello world"},
                {"role": "tool", "content": "some tool output with enough text to compress"}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload.clone(), &config);
        assert!(result.is_some(), "OpenAI format should be detected");
    }

    #[test]
    fn test_detect_anthropic_format() {
        let payload = serde_json::json!({
            "model": "claude-3-opus",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": [{"type": "tool_result", "content": "tool output"}]}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload.clone(), &config);
        assert!(result.is_some(), "Anthropic format should be detected");
    }

    #[test]
    fn test_detect_gemini_format() {
        let payload = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "hello"}]},
                {"role": "model", "parts": [{"text": "response"}]}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload.clone(), &config);
        assert!(result.is_some(), "Gemini format should be detected");
    }

    #[test]
    fn test_detect_responses_api_format() {
        let payload = serde_json::json!({
            "model": "gpt-4",
            "input": [
                {"role": "user", "content": "hello"},
                {"type": "function_call_output", "output": "tool output"}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload.clone(), &config);
        assert!(result.is_some(), "Responses API format should be detected");
    }

    #[test]
    fn test_openai_tool_compression() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "tool", "content": "On branch main\nChanges not staged for commit:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs\n  modified: src/lib.rs"}
            ]
        });
        let config = test_config();
        let orig = payload["messages"][0]["content"].as_str().unwrap().len();
        let result = compress_messages(&mut payload, &config).unwrap();
        let compressed = payload["messages"][0]["content"].as_str().unwrap().len();
        assert!(compressed <= orig, "Tool output should be compressed");
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_anthropic_string_content() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hello please help me with this task that requires a lot of text for compression"}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload, &config).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_anthropic_array_content() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "please help with this task requiring text for compression testing"},
                    {"type": "tool_result", "tool_use_id": "t1", "content": "On branch main\nChanges:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md"}
                ]}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload, &config).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_gemini_compression() {
        let mut payload = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "please help me with this task that requires text for compression testing purposes"}]}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload, &config).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_responses_api_compression() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call_output", "output": "On branch main\nChanges:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs"}
            ]
        });
        let config = test_config();
        let result = compress_messages(&mut payload, &config).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_unknown_format_returns_none() {
        let payload = serde_json::json!({"foo": "bar"});
        let config = test_config();
        let result = compress_messages(&mut payload.clone(), &config);
        assert!(result.is_none(), "Unknown format should return None");
    }
}
