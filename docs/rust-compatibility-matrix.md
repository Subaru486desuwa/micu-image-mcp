# Python / Rust 兼容性矩阵

更新时间：2026-08-23  
Python reference：仓库 HEAD `578b32a` 上保留的 `server.py` / `micu_image_mcp/`  
Rust：`micu-image-mcp 0.2.0` + 官方 `rmcp 3.1.4`

状态定义：

- **Exact**：冻结 JSON/HTTP/filesystem contract 无差异；
- **Semantic**：字段/行为相同，允许已列出的实现标识或 validator 文案差异；
- **Security delta**：有意收紧且已测试；
- **Unverified**：代码/workflow 已有，但没有当前真实运行证据。

## MCP 与工具 schema

| 项目 | 状态 | 证据/差异 |
|---|---|---|
| STDIO transport | Exact | Python/Rust smoke 均通过，stdout 每行均为 JSON-RPC |
| 2024-11-05 initialize lifecycle | Semantic | protocol/capabilities/name 相同；`serverInfo.version` 是 Python FastMCP `1.28.0` vs Rust 项目 `0.2.0` |
| 2026-07-28 stateless lifecycle | Rust 通过 | `server/discover`、带完整 `_meta` 的 tools/list/tools/call 通过；Python 仅作为 legacy reference，不要求支持该修订 |
| 五个 tool 名称与顺序 | Exact | `image_generate`, `image_edit`, `image_batch_edit`, `image_multi_reference`, `server_info` |
| tools/list descriptions | Exact | Rust binary 编译进冻结 catalog；完整字符串相等 |
| tools/list inputSchema | Exact | properties、type/null、required、default、title 全部相等 |
| tools/list outputSchema | Exact | 五个工具均保持 Python `additionalProperties: true` contract |
| JSON text + structuredContent | Exact/Semantic | 同时返回 pretty JSON text 与 structuredContent；比较时只忽略 object key 顺序 |
| 参数类型 validator 诊断 | Semantic | 错误均为 MCP `isError=true`；Pydantic URL/行文与 serde 诊断不同，因此差分只规范化 validator 文案，不忽略字段或错误状态 |
| `n=true` 历史 coercion | Exact | Rust custom deserializer 保留 Pydantic `true -> 1` 行为，然后到达相同缺-key/执行路径 |

`tests/contract/fixtures/python/tools-list.json` 与 Rust `tools-list.json` 的 `result.tools` 当前直接
相等，不需要白名单。

## 公共业务行为

| 行为 | 状态 | 说明 |
|---|---|---|
| 支持模型 allowlist | Exact | 仅 `gpt-image-2` / `gpt-image-2-openai`，精确字符串，空白包裹也拒绝 |
| Grok 公共错误与 server_info 状态 | Exact | 调用前、文件读取前拒绝；channel disabled/status/compatibility keys 保留 |
| ≥1600 自动高质量线路 | Exact | route note、effective model 相同 |
| 2K/4K generate 强制 n=1 | Exact | requested_n 与中文 note 相同 |
| 1K 标准 generate n>1 | Exact | 最多 5 in-flight，结果按 index 排序；6 请求实测 max active=5 |
| 高质量 generate 串行 | Exact | concurrency=1 |
| batch 标准/高质量 | Exact | 标准 max active=5；高质量 max active=1 且第二请求 start gap ≥1.4s |
| edit endpoint | Exact | 始终 `/v1/images/edits` multipart，image part 文件名/MIME/sha256 相同 |
| multi-reference endpoint | Exact | `/v1/images/edits` + 重复 `image[]`，part 数/顺序/文件名/MIME 相同 |
| 不 fallback chat/completions | Exact | provider 只有 Images generations/edits 两种 endpoint |
| response_format auto | Exact | URL 落盘失败后重新请求 API `b64_json`；顺序与 notes 关键语义相同 |
| data image URL | Exact | 解包为 base64 并落盘 |
| 文件命名与 collision | Exact | generate `_1`，edit/multi basename，batch timestamp 形状，O_EXCL `_2` 冲突结果相同 |
| actual_size / actual_megapixels / size_honored | Exact | 36 场景差分包括 exact/mismatch/无输出 |
| error/errors/notes 字段 | Exact/Semantic | 业务中文文本与字段存在性相同；HTTP client 自带的 disconnect/redirect 底层英文由差分规范化 |

## 尺寸、图片与路径

| 项目 | 状态 | 说明 |
|---|---|---|
| WxH、边长、16 对齐、像素、3:1 | Exact | 纯逻辑 literals 与入口差分均通过 |
| prompt size inference 与优先级 | Exact | 明确像素 > K > shape；含 Python banker rounding 的 1000→992 |
| quality enum | Exact | auto/low/medium/high；非法值公共错误相同 |
| n 1..10 | Exact | bool/coercion、上限和 burn-quota 文本已覆盖 |
| 单图 4 MiB / 多图 8 MiB | Exact | 无 HTTP 请求即拒；多图使用合法 padding fixture |
| 输出/API body 25 MiB | Exact | Content-Length 与 streamed overflow 都中断；接近 cap fixture 已测 RSS |
| 多图 2..10 | Exact | too few/too many 与成功 multipart 均覆盖 |
| basename | Exact | ASCII 集、100 字符、路径分量、前导点、`..` |
| save_dir root / symlink | Exact + stronger implementation | 公共拒绝文本相同；Rust 使用 cap-std capability + no-clobber hard link |
| MICU_INPUT_ROOT / symlink | Exact + stronger implementation | 公共拒绝文本相同；Rust 上传已验证磁盘快照，不重新按不可信路径读取 |
| truncated/malformed/spoof/bomb | Security delta applied to both | Python reference 在冻结 baseline 后也加强为 full load + guards；差分通过 |
| mask 尺寸/alpha | Exact | PNG、同尺寸、IHDR color type 4/6 |

## HTTP、SSRF、retry 与锁

| 项目 | 状态 | 说明 |
|---|---|---|
| 进程级 HTTP client/keep-alive | Rust 通过 | reqwest client 共享；连接池 20，redirect none |
| base URL 显示/拼接 | Security delta | Rust 去掉末尾 `/` 后拼 endpoint，避免 `//v1`；因此显式带 trailing slash 的 server_info 文本会比 Python 少一个 `/` |
| 默认不读 shell proxy | Exact | 仅 `MICU_USE_SHELL_PROXY=1` 读取 HTTP(S)/ALL/NO_PROXY |
| URL scheme 与 redirect | Exact | 仅 http/https；302 private redirect 不跟随并触发既有 b64 fallback |
| private/loopback/link-local/reserved | Exact/Security delta | loopback 与 IPv4-mapped IPv6 黑盒相同；Rust reserved 范围更显式 |
| fake-ip + trusted host | Exact | Python/Rust 单测覆盖 trusted/untrusted/disabled/IP literal |
| DNS 异步与 pinning | Security delta | Rust async lookup 后 `resolve_to_addrs` pin；Python reference 仍是 resolve-check-connect TOCTOU；proxy caveat 见安全评审 |
| Retry-After seconds/HTTP-date/120s | Exact | 0 秒 fixture 避免测试等待，parser 单测覆盖 cap |
| network free retry | Exact | disconnect 与 50ms test timeout 都是 2 attempts，2.0s note |
| 1K 4s/8s+jitter | Exact | pure schedule 单测；黑盒 retry 用 Retry-After=0 保持测试快速 |
| 2K/4K 60s 单次 | Exact | pure schedule 单测 |
| 524 large fail-fast | Exact | 1 HTTP request，无 retry note |
| 400 Too Many Requests→429 | Exact | 2 请求，note 显示 HTTP 429 |
| 进程内 Semaphore | Exact semantics | Python asyncio vs Rust tokio；并发 black-box 相同 |
| 跨进程文件锁 | Rust 通过 | fs4 try_lock poll；取消测试、独立 gate、两个真实子进程 2×400ms critical section 均通过 |

## 安装与发布

| 项目 | 状态 | 说明 |
|---|---|---|
| Python reference 保留 | Exact | `server.py`、package、Python tests 未删除 |
| install.py Phase A | 已实现 | 默认仍 Python；`--runtime rust --rust-binary ...` 才配置 binary |
| Rust 无参数/serve | 已实现并 smoke | 无参数默认为 STDIO serve |
| Rust install/reset/doctor/version | 本机单测/CLI help 通过 | JSON/TOML merge/reset、backup、atomic write、0600、flags 与 masking |
| macOS Keychain launcher | 已实现 | 未设 `MICU_MCP_BINARY` 保持 Python；设置后 exec Rust `serve` |
| Linux x86_64 release | Unverified remote | workflow 已添加；本次未 push，runner 未执行 |
| macOS x86_64 release | Unverified remote | 使用当前官方 `macos-15-intel` label；runner 未执行 |
| macOS arm64 release | 本机通过 / remote unverified | 本机 release 通过；`macos-15` workflow 未执行 |
| Windows x86_64 release | Unverified remote | workflow 已添加；runner 未执行 |
| checksums | Unverified remote | release workflow 生成 SHA256SUMS；尚无实际 artifact |

## 差分测试范围

当前 live differential 共 36 个 mock case，另有 initialize、完整 tools/list、server_info 和入口
validation snapshot。case 覆盖 URL/b64/data URL、generations/edits/multiple `image[]`、retry/status、
timeout/disconnect、body cap、非法 JSON、无 payload、输入/输出路径、图片损坏、mask、5 并发、
1.5s gap、filename collision、key/base64 redaction。

差分规范化只允许：

1. JSON object key 顺序；
2. Python SDK vs Rust project implementation version；
3. Pydantic vs serde 的类型诊断正文；
4. disconnect/redirect 库自带英文；
5. server_info 中 Rust 锁与完整解码的真实说明。

请求 URL/method/header 语义、JSON body、multipart、请求次数/顺序、返回字段、中文业务文本、notes
关键语义和实际文件都不在忽略列表。

## 当前切换结论

**不可以切换默认实现。** 本机 Python/Rust 测试、差分、安全、RSS 与 macOS arm64 release 均已
通过，但 release gate 明确要求 macOS/Linux/Windows build 都有真实通过结果。本次不 push，
所以 GitHub 原生 runner 结果不存在。`install.py` 因而仍默认 Python，Rust 必须显式选择。
