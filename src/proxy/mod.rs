//! Reverse-proxy HTTP layer and API-format dispatch.
//!
//! `handle` reads the upstream request, hands the JSON body to
//! [`compress_messages`] which detects the API format and routes to the matching
//! per-format handler, then forwards (streaming) to the configured upstream.
//!
//! Per-format handlers live in their own modules:
//! - [`responses`]  — OpenAI Responses API (Codex), incl. session-id tracking
//! - [`openai_chat`] — OpenAI Chat Completions
//! - [`anthropic`]  — Anthropic Messages
//! - [`gemini`]     — Google Gemini
//!
//! Shared compression helpers live in [`shared`]; the agent-agnostic tool →
//! pseudo-command synthesis used by the non-Codex handlers lives in [`hint`].

mod anthropic;
mod gemini;
mod hint;
mod openai_chat;
mod responses;
mod shared;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde_json::Value;

use crate::Config;
use shared::CompressResult;
use toksqz::compression;

// ─── Upstream HTTP client (reqwest with rustls) ─────────────────────────

static UPSTREAM_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .no_proxy()
        .build()
        .expect("Failed to build reqwest client")
});

/// Detect API format and dispatch to the appropriate handler.
fn compress_messages(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    // 1. Google Gemini: {"contents": [{"role": ..., "parts": [{"text": ...}]}]}
    if payload.get("contents").and_then(|v| v.as_array()).is_some() {
        return gemini::compress(payload, config);
    }
    // 2. OpenAI Responses API: {"input": [...]}
    if payload.get("input").and_then(|v| v.as_array()).is_some() {
        return responses::compress(payload, config);
    }
    // 3. Has "messages" array → OpenAI Chat or Anthropic Messages
    if payload.get("messages").and_then(|v| v.as_array()).is_some() {
        // Anthropic detection: any message has array content (content blocks)
        let is_anthropic = payload["messages"]
            .as_array()?
            .iter()
            .any(|m| m.get("content").and_then(|c| c.as_array()).is_some());
        if is_anthropic {
            return anthropic::compress(payload, config);
        }
        // Default: OpenAI Chat Completions
        return openai_chat::compress(payload, config);
    }
    None
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
    let filter_hits: Vec<serde_json::Value> = summary
        .filter_hits
        .iter()
        .take(20)
        .map(|(id, count)| serde_json::json!({"filter": id, "hits": count}))
        .collect();
    let command_hits: Vec<serde_json::Value> = summary
        .command_hits
        .iter()
        .take(20)
        .map(|h| {
            serde_json::json!({
                "command_type": h.command_type,
                "hits": h.hits,
                "saved_tokens": h.saved_tokens,
            })
        })
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

pub async fn stats_time(State(config): State<Config>) -> impl IntoResponse {
    if !config.stats_enabled {
        return (
            StatusCode::NOT_FOUND,
            [("content-type", "application/json")],
            "{\"error\":\"stats disabled\"}".to_string(),
        );
    }
    let summary = compression::stats::get_summary();
    let time_series = compression::stats::get_time_series();
    let cache = compression::cache::get_stats();
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::json!({
            "summary": {
                "total_requests": summary.total_requests,
                "total_original_tokens": summary.total_original_tokens,
                "total_compressed_tokens": summary.total_compressed_tokens,
                "total_saved_tokens": summary.total_saved_tokens,
                "avg_savings_pct": summary.avg_savings_pct,
                "cache_hits": cache.hits,
                "cache_misses": cache.misses,
                "command_hits": summary.command_hits,
            },
            "time_series": time_series,
        })
        .to_string(),
    )
}

pub async fn dashboard() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        include_str!("../../dashboard.html"),
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

    // For requests without body (GET /v1/models, DELETE, etc.), forward directly
    let mut payload: Option<Value> = if bytes.is_empty() {
        None
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => Some(v),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    "{\"error\":\"Invalid JSON\"}".to_string(),
                )
                    .into_response();
            }
        }
    };

    // Compress messages (only for requests with JSON body)
    let result = payload
        .as_mut()
        .and_then(|p| compress_messages(p, &config));

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
    let path_and_query = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());
    let url = format!("{}{}", config.upstream, path_and_query);

    // Forward headers — pass through all original headers except hop-by-hop
    let mut req_headers = HeaderMap::new();
    for (key, value) in headers.iter() {
        if is_hop_by_hop(key.as_str()) {
            continue;
        }
        req_headers.insert(key.clone(), value.clone());
    }

    let body_bytes = if result.is_some() {
        // Body was compressed — re-serialize
        match &payload {
            Some(p) => serde_json::to_vec(p).unwrap_or_default(),
            None => bytes.to_vec(),
        }
    } else {
        // No compression — forward original bytes unchanged
        bytes.to_vec()
    };

    if config.log_enabled {
        eprintln!("[debug] Forwarding {} bytes to {}", body_bytes.len(), url);
    }

    // Build reqwest request
    let mut req_builder = UPSTREAM_CLIENT.request(
        reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::POST),
        &url,
    );
    req_builder = req_builder.headers(req_headers);
    if payload.is_some() {
        req_builder = req_builder.header("content-type", "application/json");
    }
    let reqwest_req = req_builder.body(body_bytes).build().unwrap();

    let upstream_resp = match UPSTREAM_CLIENT.execute(reqwest_req).await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                [("content-type", "application/json")],
                format!(
                    "{{\"error\":{{\"message\":\"Proxy error: {}\",\"type\":\"proxy_error\"}}}}",
                    e
                ),
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
        response_headers.insert(key.clone(), value.clone());
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
            let per_cmd_str: Vec<String> = r
                .per_command
                .iter()
                .map(|c| {
                    format!(
                        "{}:{}->{},",
                        c.command_type, c.original_tokens, c.compressed_tokens
                    )
                })
                .collect();
            let joined: String = per_cmd_str.join("");
            // Truncate header to 8KB max
            let truncated = if joined.len() > 8192 {
                &joined[..8192]
            } else {
                &joined
            };
            if let Ok(val) = truncated.parse() {
                response_headers.insert("x-toksqz-per-command", val);
            }
        }
    }

    let status =
        StatusCode::from_u16(upstream_resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    // Stream response body (supports SSE)
    let stream = upstream_resp
        .bytes_stream()
        .map(|result: Result<bytes::Bytes, reqwest::Error>| {
            result.map_err(|e| {
                eprintln!("  Stream error: {}", e);
                std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
            })
        });

    (status, response_headers, Body::from_stream(stream)).into_response()
}

fn is_hop_by_hop(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "host"
            | "content-length"
            | "content-encoding"
    )
}

#[cfg(test)]
pub(crate) mod tests_util {
    use crate::Config;

    pub(crate) fn test_config() -> Config {
        Config {
            upstream: "http://localhost:9999".into(),
            host: std::net::IpAddr::from([127, 0, 0, 1]),
            port: 8787,
            rtk_enabled: true,
            caveman_level: Some("lite".into()),
            log_enabled: false,
            grouping_enabled: true,
            stats_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compress_messages;
    use super::tests_util::test_config;

    #[test]
    fn test_detect_openai_format() {
        let mut payload = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hello world"},
                {"role": "tool", "content": "On branch main\nChanges not staged for commit:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs\n  modified: src/lib.rs"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_some(), "OpenAI format should compress tool output");
    }

    #[test]
    fn test_detect_anthropic_format() {
        let mut payload = serde_json::json!({
            "model": "claude-3-opus",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "git status"}}
                ]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "On branch main\nChanges:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md"}]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_some(), "Anthropic format should compress tool output");
    }

    #[test]
    fn test_detect_gemini_format() {
        let mut payload = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "please help me with this task that requires a lot of text for compression testing purposes"}]},
                {"role": "function", "parts": [{"functionResponse": {"name": "Bash"}, "text": "./src/main.rs\n./src/lib.rs\n./Cargo.toml\n./README.md\n"}]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_some(), "Gemini format should compress tool output");
    }

    #[test]
    fn test_detect_responses_api_format() {
        let mut payload = serde_json::json!({
            "model": "gpt-4",
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "exec_command", "arguments": "{\"cmd\":\"git status\"}"},
                {"type": "function_call_output", "call_id": "c1", "output": "On branch main\nChanges not staged for commit:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs\n  modified: src/lib.rs"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config());
        assert!(result.is_some(), "Responses API format should compress tool output");
    }

    #[test]
    fn test_openai_tool_compression() {
        let mut payload = serde_json::json!({
            "messages": [
                {"role": "tool", "content": "On branch main\nChanges not staged for commit:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs\n  modified: src/lib.rs"}
            ]
        });
        let orig = payload["messages"][0]["content"].as_str().unwrap().len();
        let result = compress_messages(&mut payload, &test_config()).unwrap();
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
        let result = compress_messages(&mut payload, &test_config()).unwrap();
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
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_gemini_compression() {
        let mut payload = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "please help me with this task that requires text for compression testing purposes"}]}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.original_tokens > 0);
    }

    #[test]
    fn test_responses_api_compression() {
        let mut payload = serde_json::json!({
            "input": [
                {"type": "function_call", "call_id": "c1", "name": "exec_command", "arguments": "{\"cmd\":\"git status\"}"},
                {"type": "function_call_output", "call_id": "c1", "output": "On branch main\nChanges:\n  modified: src/app.ts\n  modified: src/utils.ts\n  modified: package.json\n  modified: Cargo.toml\n  modified: tests/test.rs\n  modified: README.md\n  modified: src/main.rs"}
            ]
        });
        let result = compress_messages(&mut payload, &test_config()).unwrap();
        assert!(result.original_tokens > 0);
        assert!(result.modified);
    }

    #[test]
    fn test_unknown_format_returns_none() {
        let payload = serde_json::json!({"foo": "bar"});
        let result = compress_messages(&mut payload.clone(), &test_config());
        assert!(result.is_none(), "Unknown format should return None");
    }
}
