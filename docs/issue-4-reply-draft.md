# Issue #4 reply draft

> Draft only. Do not post or close the issue automatically.

感谢反馈。根因确实是 Windows 路径被当作 TOML 双引号 basic string 手工拼接时，反斜杠会被
解释为 TOML escape，因而出现非法的 `\P`、`\U` 等序列。

本次修复不只是把双引号替换成单引号：

- Rust installer 在 `src/installer/codex.rs` 中使用 `toml_edit` AST 写入纯 binary `command` 和
  独立的 `args = []`，不拼 shell command，也不依赖 cmd/PowerShell wrapper；
- 配置先写到同目录临时文件，再由真实 TOML parser 重解析并精确核对
  `PathBuf -> AST -> serialize -> parse -> PathBuf`；只有 round-trip 完全一致才备份并原子替换
  原配置；
- Claude JSON 使用同样的写后重解析/字段 round-trip；
- 更新时保留其他 MCP server、未知字段、注释，重复 install 幂等，reset 只删除 micu-image；
- binary 会复制到稳定的 per-user data-local 目录，不再默认让配置指向仓库
  `target/release`；
- API key 不写入 Codex/Claude 配置。

新增单独回归测试
`codex_config_windows_backslash_regression_issue_4`，并覆盖：

- `C:\Python313\python.exe`
- 含空格的 `C:\Program Files\...`
- 中文目录
- `#`、`=`、单引号、双引号
- UNC `\\server\share\...`
- extended-length `\\?\C:\...`
- drive root、尾随分隔符
- POSIX 空格/中文/单双引号路径
- Windows 原生 root 大小写与 `C:\safe` / `C:\safe2` 前缀混淆
- Windows junction 与 UNC 根外逃逸

对应修复提交：`aec146b` (`fix: unify cross-platform path handling`)；审查修复与真实 UNC/42-case
矩阵补充：`9e7e262` (`fix: address path refactor review findings`)。

更新/重装：

```powershell
cargo build --release
.\target\release\micu-image-mcp.exe install --yes `
  --binary-path .\target\release\micu-image-mcp.exe
.\target\release\micu-image-mcp.exe doctor
```

正式 release artifact 发布后不需要 Rust toolchain，直接用下载的 exe 执行相同的 `install` 和
`doctor` 即可。单引号 literal string 与正确转义的双引号 basic string 都是合法 TOML；修复的
契约是 parser round-trip 后路径与原值完全一致，而不是固定某一种引号风格。
