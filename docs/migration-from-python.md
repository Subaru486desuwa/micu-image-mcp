# 从 Python reference 迁移到 Rust binary

当前状态：阶段 A/B 功能已实现，但默认入口尚未切换。原因不是功能缺失，而是本次没有 push，
所以 Linux、macOS Intel、Windows 原生 GitHub runner 没有真实通过记录。

## 选择合适的路径

- 想保持当前稳定配置：继续运行 Python `server.py`，无需修改。
- 想试用本机已编译/下载的 Rust：显式配置 binary；Python reference 保留用于回滚/差分。
- 最终用户不需要 Rust toolchain：从 release artifact 下载对应单文件 binary。

release workflow 计划产出：

- `micu-image-mcp-linux-x86_64`
- `micu-image-mcp-macos-x86_64`
- `micu-image-mcp-macos-arm64`
- `micu-image-mcp-windows-x86_64.exe`
- `SHA256SUMS`

在远端 workflow 尚未实际运行前，不应把当前本机 binary 当作正式跨平台 release。

## 阶段 A：用 install.py 显式选择 Rust

`install.py` 的默认仍是 Python reference：

```bash
python install.py --runtime python
```

试用已有 Rust binary：

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
python install.py --yes \
  --runtime rust \
  --rust-binary /absolute/path/to/micu-image-mcp
```

Rust 模式跳过 pip/server dependency 安装，写入的 MCP command 直接指向 binary，args 为空。脚本
仍会备份并只合并 `micu-image`，不会删除其他 MCP server。

## 阶段 B：直接使用 Rust CLI

binary 无参数等于 `serve`：

```bash
/absolute/path/micu-image-mcp
/absolute/path/micu-image-mcp serve
```

安装：

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
/absolute/path/micu-image-mcp install --yes
```

常用 flags：

```bash
micu-image-mcp install --no-codex
micu-image-mcp install --no-claude
micu-image-mcp install --baseurl https://www.micuapi.ai
micu-image-mcp install --save-dir /absolute/output/path
micu-image-mcp reset --yes
micu-image-mcp reset --yes --no-claude
micu-image-mcp doctor
micu-image-mcp version
```

非交互 `--yes` 必须从 `MICU_API_KEY` 读 key。交互模式使用无回显输入。安装日志只显示脱敏
key。base URL 仅允许 HTTPS，或 localhost/127.0.0.1/`::1` HTTP。

## 手动配置

Claude JSON：

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "/absolute/path/micu-image-mcp",
      "args": [],
      "env": {
        "MICU_API_KEY": "sk-...",
        "MICU_SAVE_DIR": "/absolute/output/path",
        "MICU_SAVE_DIR_ROOT": "/absolute/output/path"
      }
    }
  }
}
```

Codex TOML：

```toml
[mcp_servers.micu-image]
command = "/absolute/path/micu-image-mcp"
args = []

[mcp_servers.micu-image.env]
MICU_API_KEY = "sk-..."
MICU_SAVE_DIR = "/absolute/output/path"
MICU_SAVE_DIR_ROOT = "/absolute/output/path"
```

客户端保存配置后需要重启，让它重新 spawn STDIO server。

## macOS Keychain

原 launcher 保留。若要让它启动 Rust，只额外设置 `MICU_MCP_BINARY`：

```toml
[mcp_servers.micu-image]
command = "/absolute/path/repo/scripts/run-mcp-macos-keychain.sh"
args = []

[mcp_servers.micu-image.env]
MICU_MCP_BINARY = "/absolute/path/micu-image-mcp"
MICU_KEYCHAIN_SERVICE = "ai.micuapi.mcp"
MICU_KEYCHAIN_ACCOUNT = "your-macos-account"
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

不设置 `MICU_MCP_BINARY` 时 launcher 继续使用 Python reference，因此现有 Keychain 用户不会被
迁移工作树静默切换。

## 验证

先做 CLI/协议验证：

```bash
micu-image-mcp doctor
python tests/smoke_local.py --proto \
  --server-command '/absolute/path/micu-image-mcp'
```

然后在 Claude/Codex 中调用 `server_info`，确认：

- `available_models` 只有两个 Image2 model；
- `api_key_configured` 为 true；
- base URL 与 save root 正确；
- retry lock 描述包含 tokio Semaphore 与 fs4。

开发/发布验证：

```bash
.venv/bin/python -m pytest -q
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build
MICU_RUN_CONTRACT_TESTS=1 MICU_RUN_LIVE_TESTS=0 \
  .venv/bin/python -m pytest -q \
  tests/contract/test_python_rust_differential.py \
  tests/contract/test_latest_protocol.py
cargo audit
```

这些 contract tests 只运行本地 mock API，不消耗图片额度。

## 明确回滚

### 回到 Python reference（推荐回滚命令）

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
python install.py --yes --runtime python
```

该命令会再次备份当前配置，然后只把 `micu-image` command 改回当前 Python + `server.py`；其他
MCP server 不动。

### 完全移除 micu-image 节

```bash
/absolute/path/micu-image-mcp reset --yes
# 或 Python reference installer：
python install.py --reset
```

### 恢复某个完整备份

安装/reset 会在原文件旁创建 `.bak.<timestamp>`。确认目标时间戳后：

```bash
cp "$HOME/.claude.json.bak.<timestamp>" "$HOME/.claude.json"
cp "$HOME/.codex/config.toml.bak.<timestamp>" "$HOME/.codex/config.toml"
chmod 600 "$HOME/.claude.json" "$HOME/.codex/config.toml"
```

Windows 用 PowerShell `Copy-Item` 恢复对应 backup。恢复后重启客户端。

## 数据与兼容状态

Python 实现、Python tests 和 `install.py` 都没有删除。Rust 输出文件与 Python 使用同一 size、
basename、collision 和 root 规则；迁移不移动已有图片。详见
[`rust-compatibility-matrix.md`](rust-compatibility-matrix.md) 和
[`rust-security-review.md`](rust-security-review.md)。

