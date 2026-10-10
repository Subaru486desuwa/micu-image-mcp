# Rust 行为验收矩阵

当前维护实现为 Rust 0.4.1。Python reference 与静态历史 JSON 只保留来源，不作为本次验收基准或运行时依赖。此前 v0.3.x 的 Python 差分报告是历史证据；永久 skip 的差分不计为已执行覆盖。

| 边界 | 当前 Rust 验收 |
| --- | --- |
| 五工具顺序、schema、未知参数 | `tests/mcp_protocol.rs`，对 Rust-owned `src/contracts/` 断言 |
| 旧 initialize、新 stateless discovery、四类目录 cache | `tests/mcp_protocol.rs` |
| 四模型、尺寸、quality、n、UTF-8、缺省字段 | `tests/provider_contract.rs`，实际 JSON/multipart 与请求前拒绝 |
| 单图/mask、多图 image[]、batch | `tests/provider_contract.rs`，文件 SHA/MIME/名称/数量与 endpoint |
| base64/data URL/URL、回退与无认证下载 | `tests/provider_contract.rs` |
| HTTP 重试、Retry-After、断连/半包、响应上限、错误脱敏 | `tests/provider_contract.rs`，真实本地 HTTP 请求数；质量线路启用状态码重试 |
| 4MiB/8MiB、格式、mask、输入/输出 root、symlink、collision | `tests/input_boundaries.rs` 和 `src/fs/`、`tests/sandbox_paths.rs` |
| 创建/继续/选旧版本、重启/独立会话/失败不推进 | `tests/image_sessions.rs` |
| 标准五并发、质量跨进程串行、默认锁路径、取消 | `tests/concurrency_stdio.rs`、`tests/rust_lock_multiprocess.rs`、`src/fs/lock.rs` |
| 八独立会话、同一会话竞争 | `tests/image_sessions.rs`、`cargo xtask session-benchmark` |
| 真实 install/reset/dev CLI、稳定 binary、配置保护 | `tests/installer_cli.rs`、`tests/config_roundtrip.rs`、`tests/installer_idempotency.rs` |
| Windows 大小写/分量/junction/UNC | 原生 Windows CI 的 `path_matrix`、`sandbox_paths`、`config_roundtrip` |
| macOS Keychain | Rust 配置加载器；macOS 原生 CI/build，真实个人 Keychain 验收另行授权 |
| 工程工具与付费守卫 | `devtools/tests/cli.rs` 与 `cargo xtask` |
| 构建/发布不依赖 Python | source-audit、独立无 Python 容器、Rust-only CI/release 校验 |

领域级 Rust 单测继续覆盖 routing、size、DNS pinning、reserved IP、锁取消、输出原子性与配置 round-trip；黑盒测试补齐实际产品进程与 HTTP wire，二者不能相互冒充。假 HTTP 图片不证明真实上游模型质量；原生 CLI 探针不证明 Desktop 界面。

正式平台维持 Linux x86_64、macOS ARM64/x86_64、Windows x86_64。Osaka Linux ARM64 单独提供源码检查。当前命令、工具替代关系和证据口径见 [Rust-only 开发](rust-only-development.md)。本次实际结果与明确候选版本记录在该文档及 CI artifacts；历史 Release/tag 保留。
