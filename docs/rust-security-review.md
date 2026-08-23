# Rust 重写安全评审

评审日期：2026-08-23  
范围：Rust `src/`、迁移期 Python 安全补丁、安装器、STDIO/HTTP/文件路径 contract tests  
结论：本机自动化安全检查通过；跨平台 CI 尚未实际运行，因此当前仍不允许切换默认实现。

## 威胁模型

主要不可信输入包括 MCP tool arguments、prompt、输入/输出路径、上游 JSON、图片 URL、DNS
结果、HTTP header/body、图片编码和本机多个 MCP 进程。攻击目标包括：

- 把 API key 引到攻击者 base URL；
- 在 stderr/stdout、错误文本或 Debug 中泄露 key/base64；
- 通过 SSRF 访问 loopback、内网、云 metadata 或重定向后的私网；
- 通过路径穿越/符号链接读写安全根之外；
- 通过文件覆盖竞争、半写文件、配置截断破坏数据；
- 通过超大 body、base64、图片炸弹或 chunk 聚合耗尽内存；
- 通过取消/异常留下跨进程锁或 file descriptor；
- 通过高分辨率并发打爆上游队列。

不把同一操作系统用户已经拥有的任意调试/内存读取能力视为本程序可抵御的边界。明文 MCP
客户端配置也不能抵御同一用户读取；Rust 不再把 key 写入客户端配置，macOS 可直接从 Keychain
读取，旧 launcher 仅保留用于 Python 回滚。

## 配置与 secret

- `Config` 只在进程启动时读取环境；tool 参数结构使用 `deny_unknown_fields` 且没有
  `base_url`。base URL 不可能在一次 tool call 中变化。
- Rust server 进一步拒绝非 HTTPS 的远端 base URL；HTTP 只允许 localhost、127.0.0.1、
  `::1` 或 `*.localhost`，以支持离线 mock/本地代理。
- `MICU_API_KEY`、legacy Grok key 和 tool-level key override 进入 `secrecy::SecretString`；
  `Debug` 固定显示 `[REDACTED]`。
- `Authorization` 只在 request 构造时从 secret wrapper 生成。错误清洗会替换当前 key、Bearer
  token 和长度 ≥64 的 base64-like run。
- server 不接受宽泛 `RUST_LOG=trace` 来打开 rmcp/reqwest/hyper transport trace；这些 target
  固定关闭，避免依赖层记录原始 JSON-RPC arguments。对应 mock 在 `RUST_LOG=trace` 下仍通过。
- `generate_error_redacts_key_and_base64` mock 会让上游主动回显 key 和 PNG base64；Python/Rust
  差分结果均只包含 `[REDACTED]` / `[REDACTED_BASE64]`，stderr 也不含两者。
- 为兼容冻结的五工具 schema，`api_key` tool 参数暂时保留。推荐仍是进程环境或 Keychain。

## 文件系统

### 输入

启用 `MICU_INPUT_ROOT` 时，路径先 canonicalize，再相对于 canonical root 检查，并通过
`cap-std::fs::Dir` capability 打开。符号链接指向 root 外会在读取/上传前拒绝。

输入文件处理顺序固定为：

1. 打开并读取 metadata；
2. 4 MiB 单文件硬上限；
3. 建立随机 0600 临时快照，后续校验与所有 retry 都使用该快照；
4. magic 检查；
5. 8,192 边长、16 MiPixel、96 MiB decoder allocation guard；
6. 完整像素 decode；
7. 只接受 PNG/JPEG/WebP；mask 还必须是 PNG、同尺寸、IHDR color type 4/6。

快照避免“校验后替换输入路径”让不同内容被上传。多图不保留 `Vec<Vec<u8>>`，而是保留最多
10 个受保护的磁盘快照及 file handle 元数据；multipart 每次 retry 独立 reopen 并流式读取。

### 输出

- `MICU_SAVE_DIR_ROOT` 创建后 canonicalize，并以 `cap-std::fs::Dir` 作为 capability root。
- save_dir 的 `..`、绝对越界及 symlink escape 均拒绝。
- URL/base64 都先写 root 内 `O_EXCL` 临时文件；失败或 future 取消由 RAII 删除。
- 完整 decode 通过后，用同一文件系统内的 hard-link no-clobber 提交最终名称。已有文件不会
  覆盖，冲突按 `_2`…`_1000` 递增。
- 配置文件使用同目录临时文件、`sync_all`；先以真实 TOML/JSON parser 验证临时文件和 PathBuf
  round-trip，再生成 0600 backup 并 atomic replace。Claude JSON/Codex TOML 只修改 micu-image
  节，不持久化 API key。

## SSRF 与 DNS rebinding

下载 URL 仅接受 `http`/`https`，拒绝 URL credentials，且 reqwest redirect policy 固定为
`none`。IP 检查覆盖：

- IPv4 private、loopback、link-local、multicast、unspecified、broadcast、CGNAT、文档网段、
  IETF special-use 与 reserved；
- IPv6 loopback、ULA、link-local、multicast、unspecified、文档/ORCHID/6to4 等 special-use；
- IPv4-mapped IPv6 先还原为 IPv4，再应用同一策略；
- `198.18.0.0/15` 只有 `MICU_ALLOW_FAKE_IP_DOWNLOAD=1` 且 hostname 精确/子域匹配 trusted
  host 时放行，IP literal 不会借用 trusted-host 例外。

DNS 使用 `tokio::net::lookup_host`，不阻塞 Tokio worker。所有答案只要有一个受限地址就拒绝。
通过验证的地址集合进入 `reqwest::ClientBuilder::resolve_to_addrs`，并以 host/port/address set
缓存 download client，从而让本次验证和连接使用同一地址集合，收窄 DNS-rebinding TOCTOU。

代理模式有一个明确限制：HTTP/SOCKS proxy 可能由代理端重新解析 origin hostname，本地
`resolve_to_addrs` 不能约束远端代理的 DNS。实现仍先做本地全部地址检查、拒绝 redirect，且
只有 `MICU_USE_SHELL_PROXY=1` 才启用代理。部署方若需要端到端 DNS pinning，应禁用 shell
proxy 或使用可信、可审计的代理。

## HTTP、内存与重试

- Micu API 共享一个进程级 reqwest client；默认无环境代理，redirect disabled。
- API JSON 只用一个有上限 `Vec<u8>` 增量追加；不使用 `Vec<Vec<u8>>` 再 join。
- URL body 直接流入输出临时文件。Content-Length 和无 Content-Length 两条路径都执行
  25 MiB 硬上限。
- `b64_json` 从受限 JSON buffer 借用 `&str`，以 8 KiB base64/6 KiB decoded chunk 写文件；
  不同时常驻 base64 clone、decoded image 和输出 buffer。
- 2K/4K 的整个 request+retry 在 `tokio::sync::Semaphore(1)` 与 fs4 跨进程 guard 内。
  fs4 只做非阻塞 `try_lock`，竞争时 async sleep 100ms；取消/错误/Drop 会释放锁和 handle。
- 网络错误独立免费 retry 一次；Retry-After 数字/HTTP-date 上限 120s；1K 4s/8s+jitter；
  大尺寸 60s 单次；大尺寸 524 fail-fast；400 Too Many Requests 归一为 429。

## Python reference 的明确安全增强

在冻结 HEAD 的 initialize/tools/schema/入口快照并记录 baseline 后，Python reference 只做了两
项明确安全增强，未混入模型/路由迁移：

1. 输入与输出图片从“magic + Pillow verify”加强为显式像素/边长 guard、verify + full load；
   原 baseline 曾接受 40-byte 截断 PNG，增强后会拒绝并按既有 auto 策略请求 b64 fallback。
2. `_error_detail` 增加当前 key、Bearer token 与长 base64 清洗。

两项增强都有 Python/Rust 同场景黑盒差分，并保留相同公共中文关键语义。

## 自动化证据

当前已实际通过：

- Python/Rust 38 场景 live differential（含 malformed JPEG/WebP）；
- SSRF loopback、IPv4-mapped IPv6、private redirect、fake-ip/trusted-host 单测；
- 4 MiB/8 MiB/25 MiB 上限与无 Content-Length streaming cap；
- truncated/malformed/bomb/mask 校验；
- save/input symlink escape 与 atomic collision；
- file-lock cancellation、两个独立 gate、两个真实子进程串行；
- API timeout、disconnect、408/429/500/524、Retry-After 两种格式；
- stdout JSON-only 与 key/base64 stderr 扫描；
- `cargo clippy --lib --bins ... -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic`；
- `cargo audit`：加载 1,225 条 advisory，扫描 281 个 lockfile dependency，退出码 0。

crate root 和 binary root 都有 `#![forbid(unsafe_code)]`；生产 `src/` 的严格 Clippy 检查没有
`unwrap()`、`expect()` 或 `panic!()`。

## 尚未关闭的风险

- GitHub Linux/Windows/macOS Intel/arm64 workflow 已添加，但本次没有 push，故没有真实 runner
  结果；Windows ACL 下的“0600 等价”只能由原生 runner/人工复核确认。
- shell proxy 模式不能对远端代理 DNS 提供端到端 pinning，见上文。
- 17 MiB b64 fixture 的 Rust 峰值仍为 34,592 KiB；这是受限 JSON/base64 body 的固有成本，
  但显著低于 Python 179,232 KiB，且没有 decoded output buffer 常驻。
- release artifact 的签名/公证不在当前要求内；已有 SHA-256 workflow，但尚未实际产出远端 artifact。
