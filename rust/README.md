# 米醋画图 MCP — Rust port

Python 主线（`../server.py`）的 Rust 重写版，用 `rmcp 1.6` + `tokio` + `reqwest` + `fs2`。

## 状态

| 模块 | 状态 |
|---|---|
| 配置（env / 沙箱 / size 路由） | ✅ 完整 |
| 双层锁（tokio Mutex + fs2 跨进程 flock/LockFileEx） | ✅ 完整 |
| 重试（1K 4s/8s+jitter / ≥2K 60s 单次 / 网络层免费 1 次） | ✅ 完整 |
| `image_generate`（含 ≥2K N=1 强制） | ✅ 完整 |
| `image_edit`（1K multipart + 2K generations + 4K 拒） | ✅ 完整 |
| `image_batch_edit`（1K non-pro 5 并发 / pro 串行 + 1.5s gap） | ✅ 完整 |
| `image_multi_reference`（主路径 generations + image_urls） | ✅ 完整 |
| `server_info` | ✅ 完整 |
| `image_edit` / `image_multi_reference` 的 chat-stream fallback | ❌ 暂不实现（主路径生产可用，米醋当前 origin 较稳） |

跟 Python 主线相比：
- 启动延迟 ~150ms → ~5ms
- 内存 ~40MB → ~10MB
- 单 binary 分发，不需要 Python venv
- 路由 / 重试 / 双层锁 / 沙箱 / 错误信息全部对齐

## Build

```bash
cd rust
cargo build --release
# binary 在 target/release/micu-image-mcp（Mac arm64/x86_64 / Linux）
# Windows 在 target/release/micu-image-mcp.exe
```

## 安装到 Claude Code / Codex

仓库根目录 `install.py` 加 `--rust` 选项即可：

```bash
cd ..
python install.py --rust
# 自动 cargo build --release + 把 binary 路径写进 ~/.claude.json
```

或手动改 `~/.claude.json`：

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "/绝对路径/rust/target/release/micu-image-mcp",
      "args": [],
      "env": {
        "MICU_API_KEY": "sk-...",
        "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
        "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out"
      }
    }
  }
}
```

## 跨进程锁

跟 Python 主线**同一把锁**（`~/.cache/micu-image/bigsize.lock`），所以 Rust 进程和 Python 进程混跑也能跨进程串行 ≥2K 请求。

- POSIX：`fs2::FileExt::lock_exclusive`（底层 `flock(2)`）
- Windows：`fs2` 自动用 `LockFileEx`

`fs2` 的实现进程崩溃由内核回收 fd，不留死锁。

## 限制

`image_edit` / `image_multi_reference` 的 chat-stream SSE fallback 未实现。如果米醋 origin 主路径返回 5xx，当前 Rust port 直接返回错误而不会回落到 chat stream（Python 主线会 fallback 到 1.57MP 输出）。生产稳定性以观察为准；如有 origin 退化把这条路径加上即可。

## 工程对比

| 项 | Python 主线 | Rust port |
|---|---|---|
| 行数 | ~1700 | ~1100（4 tool 实现 + 基础设施） |
| 依赖 | `mcp[cli]` + `httpx` | `rmcp` + `tokio` + `reqwest` + `fs2` + `image` 等 |
| 第一次冷编译 | n/a (pip ~10s) | ~90s |
| 类型安全 | 运行时校验 | 编译期校验（`schemars` 自动生 JSON Schema） |
