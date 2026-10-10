# Claude Desktop 兼容性与 issue #11

## 报告与根因

[issue #11](https://github.com/Subaru486desuwa/micu-image-mcp/issues/11) 报告 Claude Code 可用，但
Desktop 无法调用 `server_info`，工具数为零。附图提供了两个更具体的信号：

- MCP 面板显示 `Connected · 0 tools`，配置来源为 `~/.claude.json`。
- 日志显示 `tools/list`、`resources/list`、`prompts/list` 的响应被拒绝；缺少 `ttlMs`、`cacheScope`。

这是 Desktop 内 Claude Code 的工具目录校验失败。MCP 进程启动成功并不表示工具注册成功。
原生 Claude Code 2.1.295 使用 2026-07-28 协议，以 `server/discover` 发现服务；该版协议要求
目录返回 `ttlMs` 和 `cacheScope`。Rust SDK 的列表默认构造器未自动填这两个字段。
字段要求见 [官方 TypeScript SDK 文档](https://ts.sdk.modelcontextprotocol.io/v2/migration/support-2026-07-28#cache-fields-and-cache-hints)。

当前 `src/mcp_server.rs` 根据每个请求的协议版本补齐：

```json
{"resultType":"complete","ttlMs":0,"cacheScope":"private","tools":["... five tool objects ..."]}
```

适用于 tools、prompts、resources、resource templates 四类目录。旧协议不添加这些字段，五个工具名称、
输入/输出 schema 和图片会话保持兼容。`ttlMs=0` 禁用目录缓存，`private` 避免共享目录缓存；
它们不控制图片会话记录的保存时间。

## 2026-10-09 协议定位阶段实测

探针运行真实 Claude 客户端，模型响应使用本地 Anthropic-compatible fixture；只触发 `server_info`，
不会请求真实文字模型或图片接口。失败对照只从服务响应删掉两个缓存字段，保持 binary、工具 catalog、
配置和权限相同。

| 客户端 / 验证主机 | 生命周期 | 缺字段对照 | 当前响应 |
| --- | --- | --- | --- |
| Claude Code 2.1.295 / Osaka Linux ARM64 | server/discover，2026-07-28 | 四次 tools/list，模型可见 0 工具；server_info 不可调用；日志出现两个字段校验错误 | 首次 tools/list 注册五个工具；server_info 成功 |
| Desktop 1.32352.1 自带 Claude Code 2.1.170 / Mac | initialize，旧协议 | 五个工具和 server_info 均可用 | 五个工具和 server_info 均可用 |

Mac 验证直接运行 Desktop 自带的原生 CLI，MCP 通过 SSH 连接 Osaka 的当前 Debug binary；
是 Mac 客户端与 Osaka 服务端的调用验证，没有使用 Mac 旧仓库源码，也没有构建 Mac binary。
Desktop 窗口验收因 Mac 锁屏暂未完成。上述 CLI 验证不代表普通 Chat 模式或 Desktop 界面已经验收。
也未执行 Windows 原生客户端验收。

Osaka 全量离线测试：401 passed / 5 skipped；Rust 90 项通过，Release 构建、格式、严格 Clippy 和 diff 检查通过。新版元数据回归测试同时断言四类目录与旧协议格式。
探针失败对照仅用于复刻已知错误；生产服务始终返回与协议对应的完整字段。

## 离线复跑

安装好真实 Claude Code 后：

```bash
cargo xtask catalog-probe \
  --cli /absolute/path/to/claude \
  --output tests/reports/issue11-fresh-run
cargo test --locked --test mcp_protocol
```

输出目录必须是新的目录；默认 MCP 为仓库 `target/debug/micu-image-mcp`。
`--binary` 指定本机的 Rust server；跨主机验收另行在实际客户端主机执行。
JSON 报告保存在被 Git 忽略的目录，fixture 仅使用无效的本地占位凭据；客户端 stdout/stderr 不落盘。

## Desktop 两种模式及更新

Desktop 的 Code 模式使用 Claude Code 用户配置 `~/.claude.json`。当前 installer 会更新这个文件及
Codex `~/.codex/config.toml`，配置中的 command 指向稳定 per-user binary。

Desktop 的普通 Chat 模式使用独立文件，当前 installer 不会自动写入：

- macOS：`~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows：`%APPDATA%\Claude\claude_desktop_config.json`

需要手工合并 `mcpServers.micu-image`，使用已安装 binary 的绝对路径、`args: []`，保留其他 server；
配置环境中的图片目录、API endpoint、Keychain service/account 应与现有安装一致，不复制明文 API key。
先在同一用户下配置系统安全凭据，再重新启动或重新连接客户端。

源码修复不会自动更新已经安装或已运行的旧 binary。安装修正版后查看实际 command 对应 binary 的
`version`，重新连接 MCP，再调用 `server_info`。修复包含在 v0.4.1；
旧版本需要更新实际安装的 binary，修改 API key 或重复连接旧 binary 无法修复此目录校验问题。


## v0.4.1 发布与 Mac 部署

后续已完成 [v0.4.1 发布](https://github.com/Subaru486desuwa/micu-image-mcp/releases/tag/v0.4.1)，issue #11 已关闭。
Osaka 发布验收为 Rust 90 项、Python 402 passed / 5 skipped；Linux、两种 macOS 架构与 Windows 原生 CI 通过。
MacBook 已安装正式 ARM64 包，SHA-256 与公开 Release 一致；使用 Desktop 的实际配置完成五工具、
新旧协议和 server_info 调用，并用 Desktop 自带 CLI 验证本地 binary 可调用。
Desktop Chat 与 Code 的配置均已补好；窗口生图仍交由用户手动验收。
