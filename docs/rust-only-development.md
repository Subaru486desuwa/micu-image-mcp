# Rust-only 开发与验收

2026-10-10 起，main 的产品入口、安装器、测试和工程工具均由 Rust 维护。Python 源码、包元数据与解释器依赖退出主线；必要 Shell/PowerShell 下载入口保留。静态历史 JSON 不是可执行参考实现。五个 MCP 工具与现有会话、配置、凭据和安全边界保持兼容。

工程工具属于 workspace 的 `micu-devtools`，通过 `cargo xtask` 调用，不进入产品 Release 二进制。Rust MSRV 为 1.88。

```bash
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo build --locked --workspace
cargo test --locked --workspace --all-targets --all-features
cargo build --locked --release --all-features
cargo xtask source-audit
cargo xtask smoke --binary target/release/micu-image-mcp
cargo xtask release-check --tag v0.4.1
```

Windows 的 binary 路径带 `.exe`。测试通过 Cargo 指定的真实产品 binary 发送 STDIO，缺少 binary 直接失败。离线驱动清空继承环境，使用临时 HOME、独立输出目录、本地 HTTP fixture、占位凭据；不读取个人 API 配置，不向付费服务发请求。新旧 MCP 目录、参数矩阵、HTTP JSON/multipart、重试、响应上限、SSRF、输入/输出路径、持久会话和安装配置都在 Rust suite 中运行。

## 工具迁移映射

| 旧工具/覆盖 | Rust 入口 |
| --- | --- |
| smoke、dry-run | `cargo xtask smoke` |
| 参数矩阵、协议与目录 | `tests/provider_contract.rs`、`tests/mcp_protocol.rs` |
| sessions、paths、installer | `tests/image_sessions.rs`、`tests/input_boundaries.rs`、`tests/installer_cli.rs` |
| 共享锁、STDIO 压力 | `tests/concurrency_stdio.rs`、`tests/rust_lock_multiprocess.rs`、`cargo xtask stress` |
| runtime/RSS benchmark | `cargo xtask runtime-benchmark` |
| SDK 会话并发 benchmark | `cargo xtask session-benchmark`（原生 Rust STDIO 驱动） |
| Claude catalog probe | `cargo xtask catalog-probe --cli /absolute/path/claude --output target/reports/catalog` |
| MCP audit proxy | `cargo xtask audit-proxy --binary /absolute/path/server --audit target/reports/rpc.jsonl` |
| 质量矩阵/付费图片验收 | `cargo xtask quality-matrix`、既有 `examples/live_edit_session.rs` |
| native model harness | `cargo xtask native-harness` |
| release/branch 编排 | `cargo xtask release-check`、`cargo xtask maintain-branches` |

旧差分与冻结 Python 私有实现测试退出；原先永久 skip 的 38-case 差分和路径 baseline 不计入新验收数量。有效场景改由现行 Rust 公共行为断言维护，不重新生成快照来代替正确性判断。旧 Python 包安装/PEP 668 测试随旧安装产品退出。

## 性能与客户端

```bash
cargo xtask stress --processes 4 --rounds 1 --output target/reports/stress.json
cargo xtask runtime-benchmark --binary target/release/micu-image-mcp --repeat 3 --output target/reports/runtime.json
cargo xtask session-benchmark --binary target/release/micu-image-mcp --repeat 3 --output target/reports/session.json
```

报告记录 binary SHA-256、commit、工作树脏状态、主机架构、工具链和原始采样。runtime 包含 cold initialize、目录、server_info、idle/peak/final RSS、24MiB URL、17MiB base64、接近 JSON 上限、约 8MiB multipart 和四进程 idle RSS；没有自动性能阈值。Linux 从 `/proc` 采样，macOS 使用原生 `ps`，Windows 使用原生 PowerShell；不支持的采样记 null，不伪造为零。

catalog probe 使用真实 Claude Code CLI 与本地 Messages/SSE fixture，分别去掉/保留现代目录缓存字段。验证五个工具注册且 server_info 仅调用一次；旧协议客户端的失败对照按其实际协议判断。探针不等同于 Desktop 窗口验收。audit proxy 只在 stdout 转发 JSON-RPC；报告只保留验收所需元数据，忽略任意错误正文和提示词。`--catalog` 可覆盖描述，但不能改变工具身份或参数 schema。

native harness 支持 `--kind claude|opencode`，四轮独立 CLI 进程通过客户端 session 继续对话，验收 A,A,B,A、版本 1,2,1,3 和实际前图路径。旧 OpenCode persistent server 性能模式由上述 benchmark 替代；工具的命令行不是产品 API，旧参数不保留别名。真实客户端是否安装及支持指定选项必须另行在原生主机验证。

## 显式在线入口

常规 CI 不运行在线请求。`quality-matrix` 默认只输出计划，不读凭据；四模型、生成/编辑、省略和声明质量共 48 项 1K，也可通过 `--sizes 1024x1024,2048x2048,4096x4096` 规划 144 项。真正执行还需要 `--live`、`MICU_RUN_LIVE_TESTS=1`、`--limit` 和新的 `--output` 目录。stdin JSON 的字段为 `api_key`、可选 `origin`；只允许固定米醋 HTTPS 域名。每项最多四次实际 POST，含重试；报告记录实际请求数与状态，错误不冒充成功。

native harness 必须 `MICU_RUN_HARNESS_TESTS=1`；stdin JSON 为 `api_key` 和 `origin`，只允许既定米醋/Anthropic/DeepSeek HTTPS origins，最多 32 次模型 POST。图片默认为本地 fixture；`--live-images` 另外要求 `MICU_RUN_LIVE_TESTS=1` 和 stdin 的 `image_api_key`、可选 `image_origin`，最多 16 次图片 POST。`--cli`、`--model`、`--output` 必填。凭据只在 relay 内存中，不写客户端配置、argv 或报告；真实文字模型授权不代替图片授权。

## 平台与历史

Osaka 是 Linux ARM64，源代码构建和离线验证在该主机执行。正式资产仍为 Linux x86_64、macOS ARM64/x86_64、Windows x86_64；原生 CI 各自 build/test/STDIO smoke，不使用 Linux 来冒充 macOS/Windows 行为。历史性能、客户体验和 live 报告保留其日期与当时工具来源，不视为本次 fresh 验证。当前证据另存 ignored `target/reports/`。

`source-audit` 扫描 Git 管理与未忽略的源文件，拒绝 Python/TS/JS 文件、Python 包元数据和可执行脚本/工作流中的解释器入口；不扫描 ignored 依赖目录或历史分支。独立无 Python 环境验收使用 `devtools/no-python.Dockerfile`，并明确检查 Python/Python3 不存在。
