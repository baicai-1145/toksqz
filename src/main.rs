mod proxy;
use toksqz::compression;

use std::net::{IpAddr, SocketAddr};
use axum::{Router, routing::{get, any}};

const BIN_NAME: &str = env!("CARGO_PKG_NAME");
const BIN_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct Config {
    pub upstream: String,
    pub host: IpAddr,
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
        let host = std::env::var("SQUEEZE_HOST")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(IpAddr::from([127, 0, 0, 1]));
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
        Config { upstream, host, port, rtk_enabled, caveman_level, log_enabled, grouping_enabled, stats_enabled }
    }
}

fn handle_cli_args() -> bool {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") => {
            println!("{BIN_NAME} {BIN_VERSION}");
            true
        }
        Some("--help") | Some("-h") => {
            println!(
                "{BIN_NAME} {BIN_VERSION}\n\n\
                 Environment variables:\n\
                   SQUEEZE_UPSTREAM        Upstream API base URL\n\
                   SQUEEZE_HOST            Listen host (default: 127.0.0.1)\n\
                   SQUEEZE_PORT            Listen port (default: 8787)\n\
                   SQUEEZE_RTK             Enable RTK compression\n\
                   SQUEEZE_CAVEMAN         Enable Caveman compression\n\
                   SQUEEZE_CAVEMAN_LEVEL   Caveman intensity level\n\
                   SQUEEZE_LOG             Print compression stats\n\
                   SQUEEZE_GROUPING        Enable output grouping\n\
                   SQUEEZE_STATS           Enable stats collection\n\
                   SQUEEZE_CACHE_TTL       Cache TTL in seconds\n"
            );
            true
        }
        Some(other) => {
            eprintln!("unknown argument: {other}");
            eprintln!("run `{BIN_NAME} --help` for usage");
            std::process::exit(2);
        }
        None => false,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if handle_cli_args() {
        return;
    }

    let config = Config::from_env();

    // Initialize compression engines (load filters + caveman rules)
    compression::init();

    let app = Router::new()
        .route("/health", get(proxy::health))
        .route("/stats", get(proxy::stats))
        .route("/api/stats/time", get(proxy::stats_time))
        .route("/dashboard", get(proxy::dashboard))
        .route("/", get(proxy::health))
        .fallback(any(proxy::handle))
        .with_state(config.clone());

    let addr = SocketAddr::from((config.host, config.port));
    let caveman_str = config.caveman_level.as_deref().unwrap_or("off");
    println!(
        "\n┌─────────────────────────────────────────────┐\n\
           │  squeeze-proxy-rs 已启动                    │\n\
           │  监听地址:  http://{}:{}{}\n\
           │  上游 API:  {}{}\n\
           │  RTK:       {}{}\n\
           │  Caveman:   {}{}\n\
           └─────────────────────────────────────────────┘",
        config.host,
        config.port,
        " ".repeat(24_usize.saturating_sub(format!("{}:{}", config.host, config.port).len())),
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
