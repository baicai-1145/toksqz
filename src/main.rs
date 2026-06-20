mod proxy;
mod compression;

use std::net::SocketAddr;
use axum::{Router, routing::{get, any}};

#[derive(Clone)]
pub struct Config {
    pub upstream: String,
    pub port: u16,
    pub rtk_enabled: bool,
    pub caveman_level: Option<String>,
    pub log_enabled: bool,
    pub grouping_enabled: bool,
    pub stats_enabled: bool,
}

impl Config {
    fn from_env() -> Self {
        let upstream = std::env::var("SQUEEZE_UPSTREAM")
            .unwrap_or_else(|_| "https://your-newapi.example.com".into())
            .trim_end_matches('/')
            .to_string();
        let port = std::env::var("SQUEEZE_PORT")
            .ok().and_then(|v| v.parse().ok()).unwrap_or(8787);
        let rtk_enabled = std::env::var("SQUEEZE_RTK").unwrap_or_else(|_| "true".into()) != "false";
        let caveman_level = match std::env::var("SQUEEZE_CAVEMAN")
            .unwrap_or_else(|_| "true".into()).as_str()
        {
            "false" => None,
            other => Some(
                std::env::var("SQUEEZE_CAVEMAN_LEVEL").unwrap_or_else(|_| {
                    if other == "true" { "lite".into() } else { other.into() }
                }),
            ),
        };
        let log_enabled = std::env::var("SQUEEZE_LOG").unwrap_or_else(|_| "true".into()) != "false";
        let grouping_enabled = std::env::var("SQUEEZE_GROUPING").unwrap_or_else(|_| "true".into()) != "false";
        let stats_enabled = std::env::var("SQUEEZE_STATS").unwrap_or_else(|_| "true".into()) != "false";
        Config { upstream, port, rtk_enabled, caveman_level, log_enabled, grouping_enabled, stats_enabled }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let config = Config::from_env();

    // Initialize compression engines (load filters + caveman rules)
    compression::init();

    let app = Router::new()
        .route("/health", get(proxy::health))
        .route("/stats", get(proxy::stats))
        .route("/", get(proxy::health))
        .fallback(any(proxy::handle))
        .with_state(config.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let caveman_str = config.caveman_level.as_deref().unwrap_or("off");
    println!(
        "\n┌─────────────────────────────────────────────┐\n\
           │  squeeze-proxy-rs 已启动                    │\n\
           │  本地地址:  http://localhost:{}{}\n\
           │  上游 API:  {}{}\n\
           │  RTK:       {}{}\n\
           │  Caveman:   {}{}\n\
           └─────────────────────────────────────────────┘",
        config.port,
        " ".repeat(26_usize.saturating_sub(config.port.to_string().len())),
        config.upstream,
        " ".repeat(26_usize.saturating_sub(config.upstream.len())),
        config.rtk_enabled,
        if config.rtk_enabled { " ".repeat(25) } else { " ".repeat(24) },
        caveman_str,
        " ".repeat(26_usize.saturating_sub(caveman_str.len())),
    );

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app.into_make_service()).await.unwrap();
}
