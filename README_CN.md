# toksqz

**Token Squeeze Proxy** — 轻量级 Rust 代理，实时压缩 LLM 请求中的 prompt，降低 token 用量和成本。

[English](./README.md)

---

### toksqz 是什么？

toksqz 是一个高性能、低开销的代理，部署在 AI 客户端和上游 API（OpenAI、Anthropic、Google 等）之间。它在转发请求之前透明地压缩 prompt 消息，使用两个互补引擎：

- **RTK**（灵感来自 [Rust Token Killer](https://github.com/rtk-ai/rtk)）— 通过检测命令模式、过滤噪声、去重行和智能截断来压缩工具/终端输出。
- **Caveman**（灵感来自 [Caveman](https://github.com/JuliusBrussee/caveman)）— 使用基于规则的语义压缩，移除用户消息中的填充词和冗余短语。

### 特性

- **极小体积**：~2.5 MB 二进制文件，~2.5 MB 启动内存，~7 MB 运行内存
- **零配置**：开箱即用，默认配置即可工作
- **SSE 流式**：完整支持 Server-Sent Events 透传
- **仪表盘**：内置 Web 仪表盘（`/dashboard`），实时可视化 token 节省情况
- **多格式支持**：支持 OpenAI Chat、Anthropic Messages、Google Gemini 和 OpenAI Responses API
- **LRU 缓存**：压缩结果缓存，24 小时 TTL，99.8% 命中率
- **压缩统计**：通过 `X-Squeeze-Original-Tokens` / `X-Squeeze-Compressed-Tokens` 响应头返回
- **可配置**：通过环境变量开关 RTK/Caveman，调整压缩强度

### 快速开始

```bash
# 编译
cargo build --release

# 运行（指向你的上游 API）
SQUEEZE_UPSTREAM=https://api.openai.com ./target/release/toksqz
```

然后将 AI 客户端指向 `http://localhost:8787`，而非上游 API。

打开 `http://localhost:8787/dashboard` 查看实时压缩仪表盘。

### Docker

```bash
docker build -t toksqz .
docker run --rm -p 8787:8787 \
  -e SQUEEZE_UPSTREAM=https://api.openai.com \
  toksqz
```

容器镜像默认设置了 `SQUEEZE_HOST=0.0.0.0`，这样宿主机可以直接访问代理服务。

### npm

```bash
npm install -g toksqz
toksqz --version
```

npm 包本身只是一个很薄的包装层，会在 `postinstall` 阶段从 GitHub Releases 下载匹配平台的预编译二进制。

发布时请在 npm 后台配置 trusted publishing：

- GitHub 用户或组织：`baicai-1145`
- Repository：`toksqz`
- Workflow filename：`release.yml`
- Allowed action：`npm publish`

切到 OIDC 之后，GitHub Actions 里就不再需要 `NPM_TOKEN` 了。

### Homebrew

先基于已经发布的 GitHub Release 生成 formula，再提交到你的 tap 仓库：

```bash
python packaging/homebrew/generate_formula.py 0.1.2 -o toksqz.rb
```

生成出来的 formula 适合放进类似 `baicai-1145/homebrew-tap` 这样的 tap 仓库。

### 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `SQUEEZE_UPSTREAM` | `https://your-newapi.example.com` | 上游 API 基础 URL |
| `SQUEEZE_HOST` | `127.0.0.1` | 监听地址（容器中用 `0.0.0.0`） |
| `SQUEEZE_PORT` | `8787` | 本地监听端口 |
| `SQUEEZE_RTK` | `true` | 启用 RTK 压缩（工具输出） |
| `SQUEEZE_CAVEMAN` | `false` | 启用 Caveman 压缩（用户消息）（`true`/`false`/强度级别）。默认关闭：对 coding agent 收益极小（<1%）且会改写指令文本，有改变模型行为的风险 |
| `SQUEEZE_CAVEMAN_LEVEL` | `lite` | Caveman 强度：`lite`、`standard`、`aggressive` |
| `SQUEEZE_LOG` | `true` | 在标准输出打印压缩统计 |
| `SQUEEZE_GROUPING` | `true` | 启用输出分组聚合 |
| `SQUEEZE_STATS` | `true` | 启用统计收集和仪表盘 |
| `SQUEEZE_CACHE_TTL` | `86400` | 缓存条目 TTL（秒），默认 24 小时 |

### 接口

| 路径 | 方法 | 说明 |
|---|---|---|
| `/health` | GET | 健康检查，返回当前配置 |
| `/stats` | GET | JSON 统计数据摘要 |
| `/api/stats/time` | GET | 时间序列统计（每小时/每天/每月） |
| `/dashboard` | GET | Web 仪表盘，实时监控 |
| `/*` | 任意 | 压缩后代理到上游 |

### 工作原理

```
客户端 → toksqz (localhost:8787) → 上游 API
              │
              ├─ tool 消息  → RTK 引擎（过滤、去重、截断）
              └─ user 消息  → Caveman 引擎（填充词移除）
```

1. 客户端向 toksqz 发送标准 API 请求
2. toksqz 解析 `messages` / `contents` / `input` 数组
3. tool 角色消息由 RTK 引擎压缩
4. user 角色消息由 Caveman 引擎压缩
5. 压缩后的请求转发到上游 API
6. 上游响应（包括 SSE 流）原样透传
7. 压缩统计信息添加到响应头

### 内存占用

| 阶段 | 内存 (RSS) |
|---|---|
| 启动（延迟初始化） | ~2.5 MB |
| 首次请求后 | ~7 MB |
| 稳态运行 | ~7 MB |
| 缓存满载 | +5 MB |

### 致谢

- [RTK (Rust Token Killer)](https://github.com/rtk-ai/rtk) — Apache-2.0
- [Caveman](https://github.com/JuliusBrussee/caveman) — MIT

### 许可证

Apache-2.0。详见 [LICENSE](./LICENSE) 和 [NOTICE](./NOTICE) 中的第三方归属声明。
