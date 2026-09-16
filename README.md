<p align="center">
  <img src="assets/banner.svg" alt="MICU IMAGE — GPT Image 2 MCP server" width="820">
</p>

# 米醋画图 MCP

把 [米醋](https://www.micuapi.ai) 的图像接口包装成 MCP server，让 Claude Code / Codex / Cursor 等 MCP 客户端直接生图、改图、批处理、多图参考。

当前支持 `gpt-image-2.5-flare`、`gpt-image-2.5-sunburst`、`gpt-image-2` 和
`gpt-image-2-openai`。文生图默认使用 Flare，编辑、批量编辑和多图参考默认使用 Sunburst。
Grok 生图渠道暂时关闭，待服务器支持后再启用；即使配置旧的 Grok 环境变量，安装器也不会写入，工具调用会在发出请求前拒绝 Grok 模型。

---

## 功能

| Tool | 说明 |
|---|---|
| `image_generate` | 文生图；默认 Flare，2.5 支持 quality 到 `max` |
| `image_edit` | 单图参考/编辑；默认 Sunburst，走 `/v1/images/edits` |
| `image_batch_edit` | 多张图逐张同指令处理；默认 Sunburst 串行 |
| `image_multi_reference` | 2-10 张参考图融合成 1 张新图；默认 Sunburst |
| `server_info` | 查看 base URL、模型、size 规则、重试策略、安全约束 |

第一次使用前，让 LLM 调一次 `server_info`，可以看到当前运行时配置和可用能力。

---

## 使用教程

面向 Cursor / Claude Code / Codex 用户的完整 MCP 使用指南见 [docs/MCP使用教程.md](docs/MCP使用教程.md)，
涵盖工具选型、尺寸规则、环境变量与故障排查（含 Clash/Surge fake-ip 落盘问题）。

---

## 当前模型范围

四个图像工具都接受上述四个模型。GPT Image 2.5 支持
`auto / low / medium / high / xhigh / max`；旧模型最高支持 `high`。旧 `gpt-image-2`
的 2K/4K 请求仍自动切换到 `gpt-image-2-openai`；Flare 与 Sunburst 的 2K/4K
会保持所选模型并进入高分辨率串行队列。
Grok 相关实现继续保持休眠。

---

> **Windows 中文提示词**：MCP 会以原生 UTF-8 JSON 发送中文。自行编写 PowerShell 测试脚本时，不要把含中文的 here-string 直接通过管道喂给 `python -`；Windows PowerShell 的 `$OutputEncoding` 可能是 ASCII，导致中文在进入 MCP 前已变成 `?`。请将脚本保存为 UTF-8 文件后执行，或先设置 `$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()`。

## 安装

从 v0.3.0 起，`main` 与推荐安装入口是 Rust 原生单文件 MCP server：

- 默认 STDIO serve，无参数即可运行；
- 运行时不需要 Python、pip、httpx 或 Pillow；
- 提供 `install/reset/doctor/version`；
- Python v0.2.0 reference 永久保留在
  [`python-reference`](https://github.com/Subaru486desuwa/micu-image-mcp/tree/python-reference) 分支，
  main 中的兼容源码保持冻结；新模型与新参数只在 Rust 实现维护。

### 一键安装（推荐）

macOS / Linux：

```bash
curl -fsSL https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.sh | sh
```

Windows PowerShell：

```powershell
irm https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.ps1 | iex
```

一键脚本会识别当前平台，从最新 GitHub Release 下载 Rust binary，使用 Release 中的
`SHA256SUMS` 校验后调用 binary 自带的 `install --yes`。默认自动写入 Codex 与 Claude 的
`micu-image` MCP 配置；API key 不会写入配置文件。installer 会先检查 `MICU_API_KEY` 与系统
安全凭据存储；首次安装且两者都没有时，会在交互式终端中隐藏输入地询问一次 API key。
只有通过 `sk-` 前缀、20–512 字符长度和 ASCII 字符集校验后才会写入 macOS Keychain、Windows
Credential Manager 或 Linux Secret Service。之后 MCP 启动时自动读取，不需要重复输入。默认
endpoint 仍是 `https://www.micuapi.ai`；需要自定义时使用 `--baseurl` 或 `MICU_BASEURL`。

macOS / Linux 需要只配置某个客户端时，可把参数传给内置 installer：

```bash
curl -fsSL https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.sh \
  | sh -s -- --no-claude
```

脚本源码在 [`scripts/install.sh`](scripts/install.sh) 和
[`scripts/install.ps1`](scripts/install.ps1)，可先审阅再执行。

### 手动下载 Rust binary

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

旧 GPT Image 2 路径：

- W/H 必须是 16 的倍数
- 最长边不超过 3840；长宽比不超过 3:1
- 总像素必须在 655,360 到 8,294,400 之间
- 2K/4K 自动切 `gpt-image-2-openai`
- 2K/4K 强制 `n=1` 并加跨进程锁，避免多个 MCP 同时打爆高质量队列

GPT Image 2.5 的 Flare 与 Sunburst 已实测支持 `1024x1024`、`2048x1152` 和
`3840x2160`，且返回像素与请求一致。2K/4K 保持所选 2.5 模型，强制 `n=1` 并使用跨进程锁。

推荐 size：

| 档位 | 推荐值 |
|---|---|
| 1K | `1024x1024`, `1280x720`, `720x1280`, `1024x1536`, `1536x1024` |
| 2K | `2048x2048`, `2048x1152`, `1152x2048` |
| 4K | `3840x2160`, `2160x3840` |

## 尺寸与路由行为

- `/v1/images/edits` 负责单图编辑、多图参考与批量编辑。
- `gpt-image-2` 的 2K/4K 请求会自动切换到 `gpt-image-2-openai`。
- GPT Image 2.5 的 2K/4K 请求保持所选 Flare / Sunburst 模型。
- ≥2K 请求强制 `n=1`，并通过进程内与跨进程锁串行访问高质量队列。
- 返回的真实像素以响应中的 `saved.actual_size` 为准。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MICU_API_KEY` | 空 | 米醋 image2 token |
| `MICU_BASEURL` | `https://www.micuapi.ai` | 米醋 base URL |
| `MICU_KEYCHAIN_SERVICE` | `micu-image-mcp` | 系统安全凭据 service；兼容旧自定义 Keychain 项 |
| `MICU_KEYCHAIN_ACCOUNT` | `image2-api-key` | 系统安全凭据 account |
| `MICU_MODEL` | 空 | 可选全局覆盖；未设置时生成默认 Flare，编辑类默认 Sunburst |
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

## 工程与验证文档

README 只保留用户安装、配置和调用所需内容。实现细节、兼容性矩阵和测试数据统一放在 `docs/`
与 CI 中：

- [Rust / Python 性能基准](docs/rust-benchmark.md)
- [Rust 兼容性矩阵](docs/rust-compatibility-matrix.md)
- [安全审计](docs/rust-security-review.md)
- [Python → Rust 迁移与回滚](docs/migration-from-python.md)
- [贡献与本地验证](CONTRIBUTING.md)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=Subaru486desuwa/micu-image-mcp&type=Date)](https://star-history.com/#Subaru486desuwa/micu-image-mcp&Date)
