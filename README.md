# 米醋画图 MCP

**从第一张图开始记忆，接着聊、接着改。**

![Micu Image MCP v0.4.1 上下文记忆海报：沿用同一只小猫，继续细化图片](docs/assets/memory/context-memory-poster.png)

[快速安装](#安装) · [上下文记忆](#上下文记忆) · [调试与验证](#调试与验证) · [v0.4.1 Release](https://github.com/Subaru486desuwa/micu-image-mcp/releases/tag/v0.4.1)

把 [米醋](https://www.micuapi.ai) 的图像接口接入 Claude Code、Claude Desktop、Codex、Cursor 等 MCP 客户端。
生成、连续编辑、批处理、多图参考共用五个工具。v0.4.1 新增持久图片会话：保存成功版本、指令和有效约束，
让同一张图可以逐轮修改，也让一个聊天里的不同创作主题各自保留记忆。

> **新版重点：**首次生成就建立记忆；沿用 ID 继续编辑；新主题新建会话；重启 MCP 后仍可用原 ID 继续。

## 上下文记忆

先生成一只小猫，再换飞行装备；中途生成独立海景，最后回到小猫改日落。
客户端选对会话 ID，MCP 就会携带对应图片与编辑上下文。

![四次真实 MCP 调用：小猫会话 A 的 r1、r2，海景会话 B 的 r1，再返回 A 的 r3](docs/assets/memory/four-turn-demo.png)

| 你说的话 | 图片会话 | MCP 的处理 |
| --- | --- | --- |
| “生成一只小猫跳伞。” | A · r1 | 建立新会话，保存首张图和描述 |
| “给它换一种飞行装备。” | A · r2 | 上传 A 的上一版图片，沿用主角和有效约束 |
| “另外生成一张独立海景人像。” | B · r1 | 新建 B，使用这个主题自己的图片与约束 |
| “回到刚才的小猫，把背景改成日落。” | A · r3 | 返回 A，从 A 的成功历史继续编辑 |

这四张图来自 Osaka 上的 **OpenCode + DeepSeek Flash → micu-image-mcp → 米醋主站**真实调用，
对应四次成功操作。海景有一次上游失败，调整提示后重试成功；失败没有推进会话版本。
第二张实际画出了硬质滑翔翼，随后人工补充软翼形态约束，得到海报中的 **A · r4**。
海报与四步图分别标明版本，完整来源见[配图说明](docs/assets/memory/README.md)。

### 记忆如何接上下一轮

- **首次生成就开始记忆：**`image_generate` 或 `image_edit` 传 `session_id="new"`，记下返回的 `session.id`。
- **继续改同一张图：**沿用这个 ID；省略 `model`、`size`、`quality` 时继承所选版本，并携带成功编辑历史和当前约束。
- **换主题或回到旧图：**新主题用 `"new"`；回到旧图用原 ID。`A`、`B` 是上图简称，实际 ID 形如 `edit_<32位十六进制>`。
- **重启也能继续：**会话保存在用户电脑；保留原 ID、输出根目录和对应图片，即可接着编辑。指定 `parent_revision` 可从旧版本开分支。

选择哪条会话由客户端或模型负责，MCP 根据显式 ID 读取记忆；一次聊天可以拥有多条图片会话。
省略 `session_id` 仍按原有单次调用处理。每次成功才提交新版本，同一会话并发修改会返回忙。
图片记忆是本地版本记录；支持范围和存储管理见[连续编辑指南](docs/multi-turn-editing.md)、[会话存储说明](docs/session-storage.md)。

[下载海报 PNG](docs/assets/memory/context-memory-poster.png) · [可编辑 SVG](docs/assets/memory/context-memory-poster.svg) · [四步示例大图](docs/assets/memory/four-turn-demo.png)

---

## 功能

| Tool | 说明 |
|---|---|
| `image_generate` | 文生图；默认 Flare，当前主站 quality 使用 `low` / `auto` 或留空 |
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

## Claude Desktop 兼容性

v0.4.1 已修复 [issue #11](https://github.com/Subaru486desuwa/micu-image-mcp/issues/11) 的 `Connected · 0 tools`。
升级实际启动的 binary、重新连接 MCP，再调用 `server_info` 确认版本。

Desktop 的 Code 模式读取 `~/.claude.json`；普通 Chat 模式使用独立的 `claude_desktop_config.json`，
当前安装器不会自动写入后者。[兼容性指南](docs/claude-desktop-compatibility.md)包含根因、两种模式的配置和测试范围。

---

## 当前模型范围

当前支持 `gpt-image-2.5-flare`、`gpt-image-2.5-sunburst`、`gpt-image-2`、`gpt-image-2-openai`。
文生图默认 Flare，编辑、批处理和多图参考默认 Sunburst；四个图像工具都接受这四个模型。
实际可用模型以当前凭据的 `/v1/models` 为准。
2026-10-09 主站的 1K 生成和单图编辑实测：Flare / Sunburst 接受 `low`、`auto` 或省略
`quality`；`medium / high / xhigh / max` 均返回 HTTP 400（上游明确只支持 low）。
`gpt-image-2` 接受 `low / medium / high / auto` 或省略。本次凭据未列出 `gpt-image-2-openai`，
因此旧 `gpt-image-2` 的 2K/4K 自动切换模型前需要确认该凭据支持目标模型；Flare / Sunburst
的 2K/4K 保持所选模型并进入高分辨率串行队列。参数完整选项仍保留，用于兼容其他线路或后续支持。
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
`SHA256SUMS` 校验后调用 binary 自带的 `install --yes`。默认自动写入 Codex 与 Claude Code 的
`micu-image` MCP 配置（Claude Code 使用 `~/.claude.json`）；API key 不会写入配置文件。installer 会先检查 `MICU_API_KEY` 与系统
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

## 调试与验证

这次更新从“小猫改图后还能否回到同一条记忆”出发，逐步测试会话、真实 harness、协议兼容和发布包。

| 调试问题 | 定位与修正 | 验证结果 |
| --- | --- | --- |
| 连续编辑容易丢失主角、约束或版本来源 | 显式 ID、持久成功历史、分支约束；失败不提交，同会话加锁 | 生成、跨工具编辑、重启恢复、失败恢复与会话隔离均有离线覆盖 |
| DeepSeek 有时省略 `session_id` | 把“新主题新建、继续编辑复用 ID”放到工具说明最前面 | 三种原生 harness 组合在两组固定四轮场景中通过 **24/24** 个路由检查 |
| Claude Desktop 连接成功却显示零工具 | 复刻缺少 `ttlMs` / `cacheScope` 的目录校验错误，按协议补齐 | 真实 Claude 客户端首次注册五工具并调用 `server_info`；旧协议继续兼容，#11 已关闭 |
| 干净 CI 与本地结果不同 | 仅归一化 Pydantic 帮助链接版本；测试夹具使用标准化路径 | Python 3.10/3.13、Rust 最低版本和四个平台原生 CI 全部通过 |
| 视觉对象的细分形态不准确 | 检查真实输出，追加软翼与无金属支架的明确约束 | 同一会话继续修正，保留小猫与日落；人工精修单独标注 |

工具说明从 **23,127 → 3,256 字节**；DeepSeek 首轮工具上下文从 **9,924 → 3,607 tokens**。
固定 200 ms 本地接口替身下，四个独立会话串行约 **953 ms**，并发约 **248 ms**；增加工作线程没有可测收益。
这些是指定场景的实测结果，真实图像渲染、网络与模型等待另计。

发布前在 Osaka 验证 **Rust 90 项、Python 402 passed / 5 skipped**；
[完整 CI](https://github.com/Subaru486desuwa/micu-image-mcp/actions/runs/37884013213)与
[发布流程](https://github.com/Subaru486desuwa/micu-image-mcp/actions/runs/37884913631)均通过。
MacBook 已安装并校验正式包，本地五工具、新旧协议与 `server_info` 调用通过；Desktop 窗口生图验收仍由用户手动完成。

完整调试过程见[v0.4.1 调试笔记](docs/v0.4.1-debugging-notes.md)和[原生 harness 验收](docs/harness-validation.md)。

---

## 工程与验证文档

README 展示主要功能、真实案例、安装方法与调试结论；完整实现说明和验证记录见 `docs/` 与 CI：

- [Rust / Python 性能基准](docs/rust-benchmark.md)
- [v0.4.1 调试笔记](docs/v0.4.1-debugging-notes.md)
- [原生 harness 验收与耗时优化](docs/harness-validation.md)
- [Rust 兼容性矩阵](docs/rust-compatibility-matrix.md)
- [安全审计](docs/rust-security-review.md)
- [Python → Rust 迁移与回滚](docs/migration-from-python.md)
- [贡献与本地验证](CONTRIBUTING.md)

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=Subaru486desuwa/micu-image-mcp&type=Date)](https://star-history.com/#Subaru486desuwa/micu-image-mcp&Date)
