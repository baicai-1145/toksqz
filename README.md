# toksqz

**Token Squeeze Proxy** — A lightweight Rust proxy that compresses LLM prompts in-flight, reducing token usage and cost.

---

## English

### What is toksqz?

toksqz is a high-performance, low-overhead proxy that sits between your AI client and upstream API (OpenAI, Anthropic, etc.). It transparently compresses prompt messages before forwarding them, using two complementary engines:

- **RTK** (inspired by [Rust Token Killer](https://github.com/rtk-ai/rtk)) — compresses tool/terminal output by detecting command patterns, filtering noise, deduplicating lines, and smart truncation.
- **Caveman** (inspired by [Caveman](https://github.com/JuliusBrussee/caveman)) — removes filler words and redundant phrases from user messages using rule-based semantic condensation.

### Features

- **Tiny footprint**: ~2.5 MB binary, ~6 MB idle memory
- **Zero config**: works out of the box with sensible defaults
- **SSE streaming**: full support for Server-Sent Events passthrough
- **Compression stats**: returned via `X-Squeeze-Original-Tokens` / `X-Squeeze-Compressed-Tokens` response headers
- **Configurable**: toggle RTK/Caveman, adjust compression intensity via environment variables

### Quick Start

```bash
# Build
cargo build --release

# Run (point to your upstream API)
SQUEEZE_UPSTREAM=https://api.openai.com ./target/release/squeeze-proxy
```

Then point your AI client to `http://localhost:8787` instead of the upstream API.

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `SQUEEZE_UPSTREAM` | `https://your-newapi.example.com` | Upstream API base URL |
| `SQUEEZE_PORT` | `8787` | Local listen port |
| `SQUEEZE_RTK` | `true` | Enable RTK compression for tool output |
| `SQUEEZE_CAVEMAN` | `true` | Enable Caveman compression for user messages (`true`/`false`/intensity level) |
| `SQUEEZE_CAVEMAN_LEVEL` | `lite` | Caveman intensity: `lite`, `standard`, `aggressive` |
| `SQUEEZE_LOG` | `true` | Print compression stats to stdout |

### Endpoints

| Path | Method | Description |
|---|---|---|
| `/health` | GET | Health check with current config |
| `/*` | Any | Proxied to upstream with compression |

### How It Works

```
Client → toksqz (localhost:8787) → Upstream API
              │
              ├─ tool messages  → RTK engine (filter, dedup, truncate)
              └─ user messages  → Caveman engine (filler removal)
```

1. Client sends a standard OpenAI-format request to toksqz
2. toksqz parses the `messages` array
3. Tool-role messages are compressed by the RTK engine
4. User-role messages are compressed by the Caveman engine
5. The compressed request is forwarded to the upstream API
6. The upstream response (including SSE streams) is passed through unchanged
7. Compression stats are added to response headers

### Acknowledgements

- [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk) — Apache-2.0
- [Caveman](https://github.com/JuliusBrussee/caveman) — MIT

### License

Apache-2.0. See [LICENSE](./LICENSE) and [NOTICE](./NOTICE) for third-party attribution.

---

## 中文

### toksqz 是什么？

toksqz 是一个高性能、低开销的代理，位于 AI 客户端和上游 API（OpenAI、Anthropic 等）之间。它在转发请求之前透明地压缩 prompt 消息，使用两个互补引擎：

- **RTK**（灵感来自 [Rust Token Killer](https://github.com/rtk-ai/rtk)）— 通过检测命令模式、过滤噪声、去重行和智能截断来压缩工具/终端输出。
- **Caveman**（灵感来自 [Caveman](https://github.com/JuliusBrussee/caveman)）— 使用基于规则的语义压缩，移除用户消息中的填充词和冗余短语。

### 特性

- **极小体积**：~2.5 MB 二进制文件，~6 MB 空闲内存
- **零配置**：开箱即用，默认配置即可工作
- **SSE 流式**：完整支持 Server-Sent Events 透传
- **压缩统计**：通过 `X-Squeeze-Original-Tokens` / `X-Squeeze-Compressed-Tokens` 响应头返回
- **可配置**：通过环境变量开关 RTK/Caveman，调整压缩强度

### 快速开始

```bash
# 编译
cargo build --release

# 运行（指向你的上游 API）
SQUEEZE_UPSTREAM=https://api.openai.com ./target/release/squeeze-proxy
```

然后将 AI 客户端指向 `http://localhost:8787`，而非上游 API。

### 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `SQUEEZE_UPSTREAM` | `https://your-newapi.example.com` | 上游 API 基础 URL |
| `SQUEEZE_PORT` | `8787` | 本地监听端口 |
| `SQUEEZE_RTK` | `true` | 启用 RTK 压缩（工具输出） |
| `SQUEEZE_CAVEMAN` | `true` | 启用 Caveman 压缩（用户消息）（`true`/`false`/强度级别） |
| `SQUEEZE_CAVEMAN_LEVEL` | `lite` | Caveman 强度：`lite`、`standard`、`aggressive` |
| `SQUEEZE_LOG` | `true` | 在标准输出打印压缩统计 |

### 接口

| 路径 | 方法 | 说明 |
|---|---|---|
| `/health` | GET | 健康检查，返回当前配置 |
| `/*` | 任意 | 压缩后代理到上游 |

### 工作原理

```
客户端 → toksqz (localhost:8787) → 上游 API
              │
              ├─ tool 消息  → RTK 引擎（过滤、去重、截断）
              └─ user 消息  → Caveman 引擎（填充词移除）
```

1. 客户端向 toksqz 发送标准 OpenAI 格式请求
2. toksqz 解析 `messages` 数组
3. tool 角色消息由 RTK 引擎压缩
4. user 角色消息由 Caveman 引擎压缩
5. 压缩后的请求转发到上游 API
6. 上游响应（包括 SSE 流）原样透传
7. 压缩统计信息添加到响应头

### 致谢

- [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk) — Apache-2.0
- [Caveman](https://github.com/JuliusBrussee/caveman) — MIT

### 许可证

Apache-2.0。详见 [LICENSE](./LICENSE) 和 [NOTICE](./NOTICE) 中的第三方归属声明。
