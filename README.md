# toksqz

**Token Squeeze Proxy** — A lightweight Rust proxy that compresses LLM prompts in-flight, reducing token usage and cost.

[中文文档](./README_CN.md)

---

### What is toksqz?

toksqz is a high-performance, low-overhead proxy that sits between your AI client and upstream API (OpenAI, Anthropic, Google, etc.). It transparently compresses prompt messages before forwarding them, using two complementary engines:

- **RTK** (inspired by [Rust Token Killer](https://github.com/rtk-ai/rtk)) — compresses tool/terminal output by detecting command patterns, filtering noise, deduplicating lines, and smart truncation.
- **Caveman** (inspired by [Caveman](https://github.com/JuliusBrussee/caveman)) — removes filler words and redundant phrases from user messages using rule-based semantic condensation.

### Features

- **Tiny footprint**: ~2.5 MB binary, ~2.5 MB idle memory, ~7 MB under load
- **Zero config**: works out of the box with sensible defaults
- **SSE streaming**: full support for Server-Sent Events passthrough
- **Dashboard**: built-in web dashboard (`/dashboard`) for real-time token savings visualization
- **Multi-format**: supports OpenAI Chat, Anthropic Messages, Google Gemini, and OpenAI Responses API
- **LRU cache**: compression results cached with 24h TTL, 99.8% hit rate
- **Compression stats**: returned via `X-Squeeze-Original-Tokens` / `X-Squeeze-Compressed-Tokens` response headers
- **Configurable**: toggle RTK/Caveman, adjust compression intensity via environment variables

### Quick Start

```bash
# Build
cargo build --release

# Run (point to your upstream API)
SQUEEZE_UPSTREAM=https://api.openai.com ./target/release/toksqz
```

Then point your AI client to `http://localhost:8787` instead of the upstream API.

Open `http://localhost:8787/dashboard` to view the real-time compression dashboard.

### Docker

```bash
docker build -t toksqz .
docker run --rm -p 8787:8787 \
  -e SQUEEZE_UPSTREAM=https://api.openai.com \
  toksqz
```

The container image sets `SQUEEZE_HOST=0.0.0.0` so the proxy is reachable from the host machine.

### npm

```bash
npm install -g toksqz
toksqz --version
```

The npm package is a thin wrapper that downloads the matching prebuilt binary from GitHub Releases during `postinstall`.

For publishing, configure npm trusted publishing for:

- GitHub user or org: `baicai-1145`
- Repository: `toksqz`
- Workflow filename: `release.yml`
- Allowed action: `npm publish`

After OIDC is configured, `NPM_TOKEN` is no longer needed in GitHub Actions.

### Homebrew

Generate the formula from a published GitHub Release, then commit it into your tap repository:

```bash
python packaging/homebrew/generate_formula.py 0.1.2 -o toksqz.rb
```

The generated formula expects a tap such as `baicai-1145/homebrew-tap`.

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `SQUEEZE_UPSTREAM` | `https://your-newapi.example.com` | Upstream API base URL |
| `SQUEEZE_HOST` | `127.0.0.1` | Listen host (`0.0.0.0` for containers) |
| `SQUEEZE_PORT` | `8787` | Local listen port |
| `SQUEEZE_RTK` | `true` | Enable RTK compression for tool output |
| `SQUEEZE_CAVEMAN` | `false` | Enable Caveman compression for user messages (`true`/`false`/intensity level). Off by default: for coding agents the savings are marginal (<1%) and it rewrites instruction text, which can alter model behavior |
| `SQUEEZE_CAVEMAN_LEVEL` | `lite` | Caveman intensity: `lite`, `standard`, `aggressive` |
| `SQUEEZE_LOG` | `true` | Print compression stats to stdout |
| `SQUEEZE_GROUPING` | `true` | Enable output grouping/aggregation |
| `SQUEEZE_STATS` | `true` | Enable stats collection and dashboard |
| `SQUEEZE_CACHE_TTL` | `86400` | Cache entry TTL in seconds (default 24h) |

### Endpoints

| Path | Method | Description |
|---|---|---|
| `/health` | GET | Health check with current config |
| `/stats` | GET | JSON statistics summary |
| `/api/stats/time` | GET | Time-series stats (hourly/daily/monthly) |
| `/dashboard` | GET | Web dashboard for real-time monitoring |
| `/*` | Any | Proxied to upstream with compression |

### How It Works

```
Client → toksqz (localhost:8787) → Upstream API
              │
              ├─ tool messages  → RTK engine (filter, dedup, truncate)
              └─ user messages  → Caveman engine (filler removal)
```

1. Client sends a standard API request to toksqz
2. toksqz parses the `messages` / `contents` / `input` array
3. Tool-role messages are compressed by the RTK engine
4. User-role messages are compressed by the Caveman engine
5. The compressed request is forwarded to the upstream API
6. The upstream response (including SSE streams) is passed through unchanged
7. Compression stats are added to response headers

### Memory Profile

| Phase | RSS |
|---|---|
| Startup (lazy init) | ~2.5 MB |
| After first request | ~7 MB |
| Steady state | ~7 MB |
| Cache full | +5 MB |

### Acknowledgements

- [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk) — Apache-2.0
- [Caveman](https://github.com/JuliusBrussee/caveman) — MIT

### License

Apache-2.0. See [LICENSE](./LICENSE) and [NOTICE](./NOTICE) for third-party attribution.
