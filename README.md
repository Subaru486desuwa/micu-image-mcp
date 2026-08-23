<p align="center">
  <img src="assets/banner.svg" alt="MICU IMAGE — GPT Image 2 MCP server" width="820">
</p>

# 米醋画图 MCP

把 [米醋](https://www.micuapi.ai) 的图像接口包装成 MCP server，让 Claude Code / Codex / Cursor 等 MCP 客户端直接生图、改图、批处理、多图参考。

当前仅支持 `gpt-image-2` / `gpt-image-2-openai`，`MICU_API_KEY` 必须能看到这两个模型。
Grok 生图渠道暂时关闭，待服务器支持后再启用；即使配置旧的 Grok 环境变量，安装器也不会写入，工具调用会在发出请求前拒绝 Grok 模型。

---

## 功能

| Tool | 说明 |
|---|---|
| `image_generate` | 文生图。米醋 image2 支持 1K / 2K / 4K |
| `image_edit` | 单图参考/编辑。走 `/v1/images/edits`，支持 1K / 2K / 4K |
| `image_batch_edit` | 多张图逐张同指令处理；1K 并发，2K / 4K 串行 |
| `image_multi_reference` | 2-10 张参考图融合成 1 张新图，支持 1K / 2K / 4K |
| `server_info` | 查看 base URL、模型、size 规则、重试策略、安全约束 |

第一次使用前，让 LLM 调一次 `server_info`，可以看到当前运行时配置和可用能力。

---

## 使用教程

面向 Cursor / Claude Code / Codex 用户的完整 MCP 使用指南见 [docs/MCP使用教程.md](docs/MCP使用教程.md)，
涵盖工具选型、尺寸规则、环境变量与故障排查（含 Clash/Surge fake-ip 落盘问题）。

---

## 当前模型范围

所有工具与压测脚本仅接受 `gpt-image-2` 和 `gpt-image-2-openai`。2K/4K 会自动切换到高质量线路 `gpt-image-2-openai`；Grok 相关实现暂时保留为休眠代码，服务器恢复支持后可重新开放。

---

> **2026-08-14 当前线路更新**：`gpt-image-2` / `gpt-image-2-openai` 的生成与编辑统一走当前 Images API；参考图 4K 的旧线路硬阻断已经移除。2K / 4K 会自动切到 `gpt-image-2-openai` 并串行进入高质量队列，不再需要先做 1K/2K、再文生图升 4K 的绕行步骤。同时保留对 `HTTP 400 + Too Many Requests` 与 `data:image/...;base64,...` 返回的兼容处理。

> **Windows 中文提示词**：MCP 会以原生 UTF-8 JSON 发送中文。自行编写 PowerShell 测试脚本时，不要把含中文的 here-string 直接通过管道喂给 `python -`；Windows PowerShell 的 `$OutputEncoding` 可能是 ASCII，导致中文在进入 MCP 前已变成 `?`。请将脚本保存为 UTF-8 文件后执行，或先设置 `$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()`。

## 安装

从 v0.3.0 起，`main` 与推荐安装入口是 Rust 原生单文件 MCP server：

- 默认 STDIO serve，无参数即可运行；
- 运行时不需要 Python、pip、httpx 或 Pillow；
- 提供 `install/reset/doctor/version`；
- Python v0.2.0 reference 永久保留在
  [`python-reference`](https://github.com/Subaru486desuwa/micu-image-mcp/tree/python-reference) 分支，
  main 中的兼容源码与差分测试也继续保留。

### Rust binary（推荐）

从 [最新 Release](https://github.com/Subaru486desuwa/micu-image-mcp/releases/latest) 下载平台对应文件并
核对 `SHA256SUMS`：

```bash
chmod +x /absolute/path/micu-image-mcp   # macOS/Linux
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
/absolute/path/micu-image-mcp install --yes
```

`install` 会把当前 binary 原子复制到稳定的 per-user data-local 目录，再让 Codex/Claude 指向该
副本；配置不会指向仓库的 `target/release`，移动仓库或 `cargo clean` 不会使 MCP 失效：

- macOS：`~/Library/Application Support/micu-image-mcp/bin/micu-image-mcp`
- Linux：`~/.local/share/micu-image-mcp/bin/micu-image-mcp`
- Windows：`%LOCALAPPDATA%\micu-image-mcp\bin\micu-image-mcp.exe`

Rust CLI：

```bash
micu-image-mcp                 # 等同 serve，STDIO MCP
micu-image-mcp serve
micu-image-mcp install --yes --no-claude
micu-image-mcp install --yes --binary-path /path/to/downloaded/binary
micu-image-mcp install --yes --dev --binary-path "$PWD/target/release/micu-image-mcp"
micu-image-mcp reset --yes
micu-image-mcp doctor
micu-image-mcp version
```

源码开发时可显式配置已编译 binary：

```bash
cargo build --release
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
target/release/micu-image-mcp install --yes --dev \
  --binary-path "$PWD/target/release/micu-image-mcp"
```

### Python reference（保留/回滚）

```bash
git clone --branch python-reference --depth 1 \
  https://github.com/Subaru486desuwa/micu-image-mcp.git micu-image-mcp-python
cd micu-image-mcp-python
python install.py
```

非交互：

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
python install.py --yes --runtime python
```

main 中的 `install.py` 只作为兼容/回滚工具；新安装应使用 Rust binary 自带的 `install`。
Python installer 会备份并合并 Claude/Codex 配置，`--reset` 只删除 `micu-image` 节。

### macOS Keychain

原 Keychain launcher 保留用于 Python 回滚。Rust binary 本身也能在启动时按 service/account 从
macOS Keychain 取 key，因此稳定 binary 可作为纯 `command`，不再需要 shell wrapper：

```bash
security add-generic-password \
  -U -a "$USER" -s ai.micuapi.mcp \
  -l "Micu Image MCP API Key" \
  -T /usr/bin/security -w
```

```toml
[mcp_servers.micu-image]
command = "/Users/you/Library/Application Support/micu-image-mcp/bin/micu-image-mcp"
args = []

[mcp_servers.micu-image.env]
MICU_KEYCHAIN_SERVICE = "ai.micuapi.mcp"
MICU_KEYCHAIN_ACCOUNT = "your-macos-account"
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

Codex 桌面、CLI 和 IDE 扩展共享 `~/.codex/config.toml`；修改后重启客户端。

### 验证与回滚

安装后让客户端调用 `server_info`，确认 `available_models`、base URL、save root 和
`api_key_configured`。Rust 还可先运行：

```bash
micu-image-mcp doctor
python tests/smoke_local.py --proto \
  --server-command '/absolute/path/micu-image-mcp'
```

明确回滚到 Python：

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
python install.py --yes --runtime python
```

完整迁移/backup 恢复说明见 [docs/migration-from-python.md](docs/migration-from-python.md)。

---

## Size 规则

image2 路径：

- W/H 必须是 16 的倍数
- 最长边不超过 3840；长宽比不超过 3:1
- 总像素必须在 655,360 到 8,294,400 之间
- 2K/4K 自动切 `gpt-image-2-openai`
- 2K/4K 强制 `n=1` 并加跨进程锁，避免多个 MCP 同时打爆高质量队列

推荐 size：

| 档位 | 推荐值 |
|---|---|
| 1K | `1024x1024`, `1280x720`, `720x1280`, `1024x1536`, `1536x1024` |
| 2K | `2048x2048`, `2048x1152`, `1152x2048` |
| 4K | `3840x2160`, `2160x3840` |

## 尺寸能力矩阵 / Size capability

2026-08-14 实测确认：两条当前 Image2 线路均可生成与编辑；高质量线路在 1536×1024、2048×1152、3840×2160 精确返回，标准线路的部分自定义尺寸会被后端重映射。当前 MCP 已同步开放参考图 4K，不再执行旧线路的本地拒绝。

| 场景 | 可靠性 | 实际输出 |
|---|---|---|
| 1K 纯文生图/编辑 | 可用 | 两模型 1024² 均已实测；实际像素见 `saved.actual_size` |
| 2K/4K 纯文生图（`image_generate`） | 可用 | 自动切 `gpt-image-2-openai`；实测 2048×1152 / 3840×2160 精确返回 |
| 单张参考图 2K/4K（`image_edit`） | 可用 | 统一走 `/v1/images/edits`；实测 2048×1152 / 3840×2160 精确返回 |
| 多图参考 1K/2K/4K（`image_multi_reference`） | 可用 | 走 `/v1/images/edits` + `image[]`；≥2K 自动切高质量线路，核对 `saved.actual_size` |
| 批量编辑 1K/2K/4K（`image_batch_edit`） | 可用 | 标准 1K 最多 5 并发；高质量线路逐张串行，避免队列拥塞 |

说明：

- `/v1/images/edits` 是米醋真正消费输入图的端点。当前单图参考的 1024²、2048×1152、3840×2160 edits 已通过实测。
- 旧的 `generations + reference_image` 和 `generations + image_urls` 路径已经废弃；所有 Image2 参考图请求都不会再转回旧路径或 `/v1/chat/completions`。
- 参考图 4K 可直接请求；2K/4K 自动切 `gpt-image-2-openai`，并使用进程内 + 跨进程双层锁串行访问高质量队列。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MICU_API_KEY` | 空 | 米醋 image2 token |
| `MICU_BASEURL` | `https://www.micuapi.ai` | 米醋 base URL |
| `MICU_MODEL` | `gpt-image-2` | image2 默认模型 |
| `MICU_SAVE_DIR` | `~/Pictures/micu-out` | 默认输出目录 |
| `MICU_SAVE_DIR_ROOT` | 同输出目录 | 输出安全根目录 |
| `MICU_INPUT_ROOT` | 空（不限制） | 可选输入图片白名单根；启用后阻止路径/符号链接逃逸 |
| `MICU_USE_SHELL_PROXY` | `0` | 设为 `1` 才读取 shell 代理 |
| `MICU_RESPONSE_FORMAT` | `auto` | `auto`（url→b64）、`url` 或 `b64_json` |
| `MICU_TRUSTED_DOWNLOAD_HOSTS` | `oss.filenest.top` | 可信 CDN host，逗号分隔 |
| `MICU_ALLOW_FAKE_IP_DOWNLOAD` | `1` | 仅 trusted host 可放行 198.18.0.0/15 fake-ip |

路径在 server 启动时只解析一次：相对 `MICU_SAVE_DIR` 和 tool `save_dir` 都以 save root 为基准；
设置 `MICU_INPUT_ROOT` 时，相对输入路径以 input root 为基准，否则以启动时捕获的 cwd 为基准。
只展开精确的 `~`、`~/...`、Windows `~\...`，`~someone` 会被拒。Python/Rust 兼容期共用
`~/.cache/micu-image/bigsize.lock`。

## 手动配置

Claude Code:

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "/absolute/path/micu-image-mcp",
      "args": [],
      "env": {
        "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
        "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out"
      }
    }
  }
}
```

Codex:

```toml
[mcp_servers.micu-image]
command = "/absolute/path/micu-image-mcp"
args = []

[mcp_servers.micu-image.env]
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

不要手工把 Windows 路径拼进 TOML 字符串。Rust installer 使用 `toml_edit` AST，临时写入后会
再用 TOML parser 校验 `command`/`args`/env 的 PathBuf round-trip；单引号 literal string 和正确
转义的双引号 basic string 都合法，关键是 parser 回读值完全一致。API key 不持久化到上述
JSON/TOML；由客户端进程环境、macOS Keychain 或 tool 的既有 `api_key` 参数提供。

迁移期若要手动使用 Python reference，把 command 改为 Python、args 改为绝对
`server.py` 路径即可；五工具 schema 保持相同。

---

## 性能 / 压力测试

Rust/Python 同机启动与 RSS 原始数据见 [docs/rust-benchmark.md](docs/rust-benchmark.md)。当前 arm64
Mac 的 Rust idle RSS 中位数为 9,504 KiB，Python 为 66,080 KiB；该结果不代替尚未运行的
跨平台 release CI。

`tests/` 下两个独立脚本，直接 in-process import `server.py` 调 `image_generate`，不走 stdio MCP（避免子进程开销污染样本）。真实请求需要有效 key，并且只有精确设置 `MICU_RUN_LIVE_TESTS=1` 才会启动；不带 key 用 `--dry-run` 也能验证脚本/导入/校验链路。

报告默认落到 `tests/reports/<title>_<ts>.{json,md}`，已被 `.gitignore` 排除。生成的图扔到 `/tmp/micu-bench/<label>/`，不会污染你的 `~/Pictures/micu-out`。

### 性能基线 `tests/perf_bench.py`

串行跑 `gpt-image-2` / `gpt-image-2-openai` 在不同 `size` 下的 `image_generate`，记录单次延迟、actual_size 偏差、保存后字节数。

```bash
# smoke（默认）：两个 Image2 模型各 1 张；必须显式允许 live/付费请求
MICU_RUN_LIVE_TESTS=1 python tests/perf_bench.py

# 完整 sweep, 每组重复 3 次
MICU_RUN_LIVE_TESTS=1 python tests/perf_bench.py --full --repeat 3

# 干跑 (不打 API, 只验证脚本链路)
python tests/perf_bench.py --dry-run
```

报告 markdown 表头：`group | n | ok | fail | rate | p50_ms | p95_ms | mean_ms | actual_match`。`actual_match` 是图片 header 读出的实际像素严格等于请求 size 的比例；不要假定后端一定遵守自定义尺寸。

### 并发压力 `tests/stress_concurrent.py`

验证：
1. 1K 单进程多并发 → 进程内不卡，吞吐近似线性
2. ≥2K 多进程并发 → 进程内 `asyncio.Semaphore(1)` + 跨进程 `flock` 双层锁串行
3. CF 524 / 上游 5xx → 重试/fail-fast 策略
4. `--model` 仅接受 `gpt-image-2` / `gpt-image-2-openai`

```bash
# in-process 并发 (默认 smoke, image2 1K x 3)
MICU_RUN_LIVE_TESTS=1 python tests/stress_concurrent.py

# 验证 ≥2K 锁串行
MICU_RUN_LIVE_TESTS=1 python tests/stress_concurrent.py --size 2048x2048 --concurrency 4

# 跨进程模式 (spawn N 个子进程, 模拟多 Claude Code 窗口)
MICU_RUN_LIVE_TESTS=1 python tests/stress_concurrent.py --mode multiprocess --concurrency 3 --size 2048x2048

```

报告关键派生指标：

| 指标 | 含义 |
|---|---|
| `total_wall_ms` | 整批耗时（从 gather 到全部返回） |
| `serial_estimate_ms` | 所有成功请求 wall_ms 之和（串行下界） |
| `concurrency_efficiency` | `total_wall_ms / serial_estimate_ms`。≈ 1 → 强串行（锁生效）；≈ 1/N → 强并发；中间 → 部分排队 |
| `lock_wait_observed` | notes 里出现 “等待跨进程 ≥2K 锁” 的请求数（>2s 才记） |

> 提醒：Image2 真实并发会按米醋后台线路限流计费，跑 `--concurrency` ≥ 3 之前先确认账户额度。dry-run / 401 路径不计费。

### 离线 contract / 差分测试

先构建 Rust，然后运行相同 MCP STDIO 与本地 mock Micu API 矩阵：

```bash
cargo build
MICU_RUN_LIVE_TESTS=0 \
  .venv/bin/python -m tests.contract.compare_parameter_matrix \
  --output /tmp/micu-parameter-matrix.json
MICU_RUN_LIVE_TESTS=0 MICU_RUN_CONTRACT_TESTS=1 \
  .venv/bin/python -m pytest -q \
  tests/contract/test_path_refactor_baseline.py \
  tests/contract/test_python_rust_differential.py \
  tests/contract/test_latest_protocol.py
```

冻结的 42 项 size/model/quality/route 参数 nodeid、source hash 和执行结果会先做 before/after；随后
38 个黑盒场景比较 tools schema、HTTP JSON/multipart、retry 顺序、URL/b64/data URL、
SSRF、损坏图片、body cap、并发、文件冲突和实际落盘内容。mock 只监听 `127.0.0.1`，不调用
真实生图 API。安全与兼容细节见：

- [docs/rust-rewrite-design.md](docs/rust-rewrite-design.md)
- [docs/rust-compatibility-matrix.md](docs/rust-compatibility-matrix.md)
- [docs/rust-security-review.md](docs/rust-security-review.md)
- [docs/migration-from-python.md](docs/migration-from-python.md)
