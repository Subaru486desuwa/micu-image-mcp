# Rust 渐进式重写设计

状态：实现前设计冻结（2026-08-23）  
基线 HEAD：`578b32ace77bbc732462b4d6483f2259969668ef`  
目标协议 SDK：官方 `rmcp 3.1.4`（crates.io 稳定版，MSRV 1.88）  
目标 MCP 修订：`2026-07-28`，并保留 `2024-11-05` initialize lifecycle

## 1. 迁移原则

这次迁移把 Python 视为 reference implementation，而不是待逐行翻译的源码。公开行为先由
`tests/contract/fixtures/python/` 的 STDIO 快照和本地 mock Micu API 固定，再通过同一组黑盒
请求分别驱动 Python 与 Rust。对象 key 顺序会规范化；字段、默认值、必选项、description、
错误结构、中文关键语义、HTTP 请求和落盘结果不会被忽略。

Python 文件在整个迁移期保留。Rust 未满足切换门槛前，`install.py` 继续默认安装 Python
reference；编译好的 Rust binary 只能通过显式选项配置。

## 2. 已确认的公开 seam

测试只跨下列 seam，不测试内部私有函数：

1. MCP STDIO：`initialize` / `server/discover`、`tools/list`、`tools/call`、stdout 纯净度。
2. Provider HTTP：请求 URL、method、非敏感 header、JSON、multipart 文件 part 和重试顺序。
3. 受控文件系统：输入文件、输出 root、文件名冲突、原子无覆盖、实际保存内容。
4. 安装器配置：Claude JSON 与 Codex TOML 的合并、备份、原子写入和 reset。
5. CLI：无参数/`serve`、`install`、`reset`、`doctor`、`version`。

这些 seam 已由迁移需求明确，不新增针对 Rust 私有实现细节的镜像式测试。

## 3. 深模块与接口

### 3.1 `Config`

一个进程只构造一次不可变 `Config`。它读取并规范化所有 `MICU_*` 环境变量，持有：

- `SecretString` API key；
- 已验证的 base URL；
- save/input root；
- response-format、proxy、trusted-host/fake-ip 策略；
- HTTP timeout、连接池和锁文件路径。

工具参数中没有 `base_url`。base URL 只能在进程启动时进入 `Config`。`api_key` 参数因现有
工具 schema 兼容性继续保留，但进入同一个 secret wrapper，永不实现 `Debug` 明文输出。

### 3.2 `Validation`

`validation` 是纯逻辑深模块，接口只接受原始工具参数并返回已验证值或公共中文错误：

- `Size`：格式、边长、16 对齐、像素总数、长宽比、size tier；
- `ModelRoute`：精确 allowlist、Grok 公共拒绝语义、≥1600 自动高质量路由；
- `Basename`：字符集、100 字符、路径分量、前导点和 `..`；
- `ValidatedImage`：保持已打开文件 handle、真实格式/MIME/尺寸/alpha、长度。

已打开 handle 而不是重新按路径读取，避免“校验后替换文件”把不同内容上传。

### 3.3 `Storage`

`Storage` 隐藏输出牢笼、临时文件、图片验证和 no-clobber 命名。调用方只提交 payload 与
basename，得到 `SavedImage`：

- URL 下载直接流入目标目录内的临时文件；
- b64 通过流式 decoder 写临时文件，不先生成完整 decoded `Vec<u8>`；
- 达到硬上限立即停止；
- 完整图片解码验证通过后，使用 `persist_noclobber`/等价原子操作提交；
- 冲突按 `name.ext`、`name_2.ext` … 递增；
- 错误、取消或进程退出时临时文件由 RAII 清理。

输出文件不会使用 `exists() -> write()`。保存目录先 canonicalize 并验证位于 root 内；安全
评审同时记录跨平台路径交换的剩余风险及 capability-based filesystem 的采用结果。

### 3.4 `HttpExecutor`

进程级共享 `reqwest::Client` 只负责 Micu API；连接池和 keep-alive 由该 client 复用。
默认显式禁用环境代理。只有 `MICU_USE_SHELL_PROXY=1` 时，启动期读取代理环境并构造代理。

`HttpExecutor` 的小接口接收可重复构造请求体的 request factory，并负责：

- JSON/错误响应单 `Vec<u8>` 流式追加和 25 MiB 硬上限；不使用 `Vec<Vec<u8>>` 再 join；
- 网络错误一次独立免费重试；
- 400 + Too Many Requests 归一为 429；
- Retry-After 数字秒/HTTP-date，最大 120 秒；
- 1K 4s/8s+jitter；大尺寸 60s 单次重试；大尺寸 524 fail-fast；
- 敏感内容清洗后才生成公共错误或 stderr 诊断。

### 3.5 `BigRequestGate`

大尺寸请求从第一次 HTTP attempt 到最后一次 retry 始终持有同一个 gate：

1. `tokio::sync::Semaphore(1)` 提供进程内串行；
2. `fs4` 跨平台文件锁提供跨进程串行。

文件锁只使用 `try_lock`。失败后 `tokio::time::sleep(100ms)`，不在 Tokio worker 上阻塞等待。
锁 guard 拥有 file handle；future 取消、错误 return、panic unwind 或正常退出都会经 Drop 释放。

### 3.6 `ImageProvider`

Provider seam 表达领域动作，而不是通用 fetch：

- `generate(request)`；
- `edit(request, images, optional_mask)`。

当前只有 `Image2Provider` 生产 adapter；本地 mock HTTP server 是第二个测试 adapter。
Grok 不可达代码不搬运，但 trait 保留未来重新增加 provider 的位置。Image2 provider 永不产生
`chat/completions` request，单图 edit 与多图 reference 都只产生 `/v1/images/edits`。

### 3.7 `ToolService`

`ToolService` 是 MCP adapter，业务实现位于 `tools/`。它不会让 schemars 的版本差异改变公开
schema：`tools/list` 的 Tool catalog 从冻结的 Python contract 编译进 binary，运行时不依赖
外部 JSON 文件。工具参数再反序列化为 Rust 类型并走相同 validation 模块。

返回值保持 Python 的 JSON 字段形状，并作为 MCP text content 输出。公共 tool 错误继续区分：

- 入口校验：`ok=false` JSON；
- 缺 key/执行异常：MCP tool error content；
- HTTP/单张失败：各工具原有的 `error` / `errors` / `results` 形状。

## 4. 协议兼容

`rmcp::ServiceExt::serve((tokio::io::stdin(), tokio::io::stdout()))` 支持两种 opener：

- 旧 client 发送 `initialize` 时，协商并回显已知版本，包括 `2024-11-05`；
- `2026-07-28` client 直接发送带完整 `_meta` 的 `server/discover`/工具请求时，使用无状态 lifecycle。

contract tests 会分别验证旧 lifecycle 与最新 lifecycle。Rust 的 `serverInfo.version` 反映项目
版本，不伪装成 Python FastMCP SDK 的 `1.28.0`；这是初始化快照中唯一预先允许的实现标识差异。

所有 server 模式日志由 `tracing-subscriber` 写 stderr。CLI 启动 server 前不输出 banner；
stdout 的每一行都必须是 rmcp 产生的 JSON-RPC。

## 5. 图片与内存策略

输入处理顺序固定为：metadata 大小检查 → magic → decoder limits → 完整 decode → 业务约束。
decoder limit 同时限制边长、总像素和最大 allocation，拒绝伪装格式、截断文件和解压炸弹。

API JSON 只保留一个有上限的字节 buffer。响应提取使用借用反序列化，让 `b64_json` 借用该
buffer；base64 decoder 直接写临时文件，从而不同时常驻 base64 clone、decoded bytes 和最终
输出 buffer。URL 路径也直接写临时文件。

Multipart 上传从已验证的 file handle clone 创建 streaming body。多图引用不会把全部文件
读取进 `Vec<Vec<u8>>`；8 MiB 限制按 metadata 累计，并在上传期间逐文件流式读取。

## 6. SSRF 与 DNS rebinding

下载 URL 仅允许 HTTP/HTTPS且默认不跟随重定向。IP literal 与异步 DNS 的全部结果都执行：

- IPv4-mapped IPv6 先还原；
- 拒绝 loopback/private/link-local/multicast/unspecified/reserved；
- `198.18.0.0/15` 只有 trusted hostname 且显式允许 fake-ip 时放行。

只做“解析后检查、随后按 hostname 连接”存在 DNS rebinding TOCTOU。Rust 下载 client 会把已
验证地址 pin 到本次连接（或同一验证结果的缓存 client）；测试 resolver 在第一次返回公网、
第二次返回内网时，连接不得再次解析到内网。代理模式单独记录：origin DNS 可能由代理完成，
因此仍先本地验证，且不把这条检查表述为端到端 DNS pinning 保证。

## 7. 安装与切换

阶段 A：

- `install.py` 新增显式 Rust binary 选项；默认仍是 Python；
- shell/Keychain launcher 可显式 exec binary，Python fallback 仍可用；
- 不改动其他 MCP server 配置。

阶段 B：

- binary 提供 `serve/install/reset/doctor/version`；无参数等于 `serve`；
- JSON/TOML 通过结构化 parser 合并；备份后 temp + atomic replace；尽量 0600；
- 只有所有切换门槛都有真实结果时，才把安装默认值改为 Rust。

本地不能替代 GitHub 原生 Windows/Linux runner 结果。在对应 CI 未实际完成前，文档和安装器
必须继续标记“尚不可切换默认实现”。

## 8. crate 与 feature 选择

依赖使用 crates.io 稳定版并提交 `Cargo.lock`。初始锁定范围：

- `rmcp 3.1.4`，仅 server/STDIO 所需 feature；
- `tokio 1.53.1`；
- `reqwest 0.13.4`，rustls platform verifier，显式 proxy，JSON/multipart/stream；
- `serde 1.0.229`、`serde_json`、`schemars 1.2.2`；
- `thiserror 2.0.20`、`tracing 0.1.44`；
- `image 0.25.10`，只启用 PNG/JPEG/WebP/GIF decoder；
- `base64 0.23.1`（关闭默认 unsafe-SIMD feature）、`url 2.5.8`、`ipnet 2.12.1`；
- `fs4 1.1.0`、`secrecy 0.10.3`、`httpdate 1.0.3`；
- `tempfile 3.27.0`、`toml_edit 0.25.13`、`clap 4.6.6`。

crate root 启用 `#![forbid(unsafe_code)]`。生产 `src/` 禁止 `unwrap()`、`expect()` 和无说明
panic；CI 除 fmt/clippy/test 外运行 `cargo audit`（当前 CLI 稳定版 0.22.2）。

## 9. 切换判定

Rust 可执行文件完成不等于切换完成。默认入口保持 Python，直到以下证据同时存在：Python
原测试、Rust tests、黑盒差分、五工具 schema、安全测试、stdout 检查、benchmark，以及
macOS arm64/x86_64、Linux x86_64、Windows x86_64 原生构建均通过。任何一项未运行会在
兼容矩阵和最终报告中明确标为未验证。
