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
cargo xtask release-check --tag v0.4.3
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
| 质量矩阵/付费图片验收 | `cargo xtask quality-matrix`、`cargo xtask live-stress`、既有 `examples/live_edit_session.rs` |
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

只验收一张默认 Flare 图片时，使用有界入口，不运行生成加编辑的旧 quick 示例：

```bash
cargo build --locked --release --all-features
MICU_RUN_LIVE_TESTS=1 cargo run --locked --release --example live_once -- \
  --live --config ~/.codex/config.toml --binary target/release/micu-image-mcp \
  --output "target/live-once-$(date -u +%Y%m%dT%H%M%SZ)"
```

该入口要求干净、已提交的候选工作树，复用 Codex 配置解析与禁止 UI 的产品凭据读取；只启动当前工作树的 release binary。固定调用一次 `image_generate`，默认 Flare、1024×1024、`n=1`、新会话，提示为白底蓝色陶瓷杯。可信 HTTPS relay 在转发前预留预算，关闭传输层自动重试，上游图像 POST 总数最多 2；失败后不再发其他付费调用。图片和脱敏报告写入 ignored `target/`，报告记录 revision、版本、实际 ID、请求计数/状态、像素、字节及耗时；不保存凭据、完整配置、任意工具错误或带签名 URL。`--live` 与环境开关缺一都会在读取凭据前停止。

常规 CI 不运行在线请求。`quality-matrix` 默认只输出计划，不读凭据；四模型、生成/编辑、省略和声明质量共 48 项 1K，也可通过 `--sizes 1024x1024,2048x2048,3840x2160` 规划 144 项。真正执行还需要 `--live`、`MICU_RUN_LIVE_TESTS=1`、`--limit` 和新的 `--output` 目录。stdin JSON 的字段为 `api_key`、可选 `origin`；只允许固定米醋 HTTPS 域名。每项最多四次实际 POST，含重试；报告记录实际请求数与状态，错误不冒充成功。

native harness 必须 `MICU_RUN_HARNESS_TESTS=1`；stdin JSON 为 `api_key` 和 `origin`，只允许既定米醋/Anthropic/DeepSeek HTTPS origins，最多 32 次模型 POST。图片默认为本地 fixture；`--live-images` 另外要求 `MICU_RUN_LIVE_TESTS=1` 和 stdin 的 `image_api_key`、可选 `image_origin`，最多 16 次图片 POST。`--cli`、`--model`、`--output` 必填。凭据只在 relay 内存中，不写客户端配置、argv 或报告；真实文字模型授权不代替图片授权。

## 平台与历史

Osaka 是 Linux ARM64，源代码构建和离线验证在该主机执行。正式资产仍为 Linux x86_64、macOS ARM64/x86_64、Windows x86_64；原生 CI 各自 build/test/STDIO smoke，不使用 Linux 来冒充 macOS/Windows 行为。历史性能、客户体验和 live 报告保留其日期与当时工具来源，不视为本次 fresh 验证。当前证据另存 ignored `target/reports/`。

`source-audit` 扫描 Git 管理与未忽略的源文件，拒绝 Python/TS/JS 文件、Python 包元数据和可执行脚本/工作流中的解释器入口；不扫描 ignored 依赖目录或历史分支。独立无 Python 环境验收使用 `devtools/no-python.Dockerfile`，并明确检查 Python/Python3 不存在。

付费压力入口 `cargo xtask live-stress --binary target/release/micu-image-mcp --output "target/live-stress-$(date -u +%Y%m%dT%H%M%SZ)"` 保留标准单进程 `n=5` 的实际五并发，另需 `MICU_RUN_LIVE_STRESS=1` 和 `MICU_RUN_LIVE_TESTS=1`。`--highres` 还需 `MICU_RUN_LIVE_HIGHRES_STRESS=1`，同步启动五个真实 MCP 进程，分别请求 2048x1152、1152x2048、2048x2048、3840x2160、2160x3840；验证旧模型自动切换、n 强制为 1、像素、唯一文件、统一版本/锁描述及至少四个跨进程锁等待提示。stdin 凭据字段与质量矩阵一致；先 GET /v1/models 检查凭据与所需模型，最多 20 次实际图片 POST，未授权时在读取凭据前退出。本次不运行付费请求。

原生 Windows 的系统目录 API 不读取 HOME/LOCALAPPDATA 覆盖值。显式 `MICU_CONTRACT_TESTING=1` 时，测试驱动指定临时 HOME 和数据目录，安装与共享锁验收不会使用个人配置；正常启动继续使用系统目录 API。测试比较规范化路径，包含 macOS 的 /var → /private/var 和 Windows extended path 前缀。

## 实际配置的只读启动验收

在服务器上用实际 Codex 配置启动一次独立 MCP 子进程，读取版本、五工具、模型目录与凭据加载状态：

```bash
cargo run --locked --example probe_config -- /absolute/path/to/config.toml
```

该入口复用安装器的 Codex 配置解析器，只调用 initialize、tools/list 和 server_info，不提交图像请求；输出仅包含选定的非敏感字段。失败时只输出固定提示，不输出配置内容、环境变量或原始错误。
