# Python → Rust 迁移与回滚

main 只维护 Rust，已移除 `server.py`、Python 安装器和包元数据。历史 Python reference 仍在独立 `python-reference` 分支；它不参与当前 build/test/release。不得将旧 runtime 合回 main。

安装与凭据配置使用 Rust binary：

```bash
micu-image-mcp install
micu-image-mcp doctor
```

源码开发：

```bash
cargo build --locked --release
MICU_SAVE_DIR="$HOME/Pictures/micu-out" target/release/micu-image-mcp install --yes --dev \
  --binary-path "$PWD/target/release/micu-image-mcp"
```

installer 备份并合并 Claude/Codex 配置，保留其他 MCP server。默认复制 binary 到稳定 per-user 目录；显式 dev 模式直接使用所选构建。macOS Rust binary 自行读取配置的 Keychain service/account，Shell launcher 也只启动 Rust。

安装后重新连接 MCP，再调用 `server_info` 确认版本、可用模型、输出根和凭据是否已配置。离线验收使用 `cargo xtask smoke --binary /absolute/path/micu-image-mcp`，不使用个人凭据或付费图片 API。

需要回滚时，先结束当前请求，保留图片、`.micu-sessions` 和配置备份。恢复已有的可信 Rust binary 与对应配置备份；检查 `doctor` 后重启客户端。若确需历史 Python 版本，在独立目录检出 `python-reference` 并遵循该分支当时的文档，不把当前 main 当作 Python 安装源。

卸载当前 MCP 配置使用 `micu-image-mcp reset`，只移除 `micu-image` 配置节；图片与会话应由用户自行归档，不因 reset 删除。历史 Release/tag 保持可用。

开发、契约与工具命令见 [Rust-only 开发](rust-only-development.md)，历史恢复点见 [分支策略](branch-release-policy.md)。
