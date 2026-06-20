use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde_json::Value;

use crate::compression;
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

fn compress_messages(payload: &mut Value, config: &Config) -> Option<CompressResult> {
    let messages = payload.get_mut("messages")?.as_array_mut()?;
    let mut original_tokens: usize = 0;
    let mut compressed_tokens: usize = 0;

    for msg in messages.iter_mut() {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = match msg.get("content").and_then(|c| c.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let orig = estimate_tokens(&content);
        original_tokens += orig;

        // RTK: compress tool output
        if config.rtk_enabled && role == "tool" {
            let compressed = compression::rtk_compress(&content);
            let new_tokens = estimate_tokens(&compressed);
            if config.log_enabled && compressed.len() < content.len() {
                println!(
                    "  [RTK] {}→{} chars",
                    content.len(),
                    compressed.len()
                );
            }
            compressed_tokens += new_tokens;
            msg["content"] = Value::String(compressed);
            continue;
        }

        // Caveman: compress user input
        if let Some(ref level) = config.caveman_level {
            if role == "user" {
                let compressed = compression::caveman_compress(&content, level);
                let new_tokens = estimate_tokens(&compressed);
                if config.log_enabled && compressed.len() < content.len() {
                    println!(
                        "  [Caveman] {}→{} chars",
                        content.len(),
                        compressed.len()
                    );
                }
                compressed_tokens += new_tokens;
                msg["content"] = Value::String(compressed);
                continue;
            }
        }

        compressed_tokens += orig;
    }

    Some(CompressResult { original_tokens, compressed_tokens })
}
