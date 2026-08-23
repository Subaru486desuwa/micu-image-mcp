# Rust 重写基准

测试时间：2026-08-23 16:36 +08:00

基线仓库：`578b32ace77bbc732462b4d6483f2259969668ef` + 本次迁移工作树

结论：Rust 达到本次启动、RSS 与 URL 流式下载目标；默认实现仍不能仅凭本机基准切换。

## 环境

| 项目 | 值 |
|---|---|
| OS | Darwin 25.2.0 / macOS 26.2 |
| 架构 | arm64 |
| Python | 3.13.14 |
| Rust | rustc 1.93.0 |
| Cargo | 1.93.0 |
| Python MCP/http/image | mcp 1.28.x / httpx 0.28.1 / Pillow 12.2.0 |
| Rust profile | `release`, thin LTO, `codegen-units=1`, stripped |
| Rust binary | 8,144,480 bytes（7.77 MiB） |

构建和测试命令：

```bash
cargo build --release --all-features
MICU_RUN_LIVE_TESTS=0 .venv/bin/python -m tests.contract.benchmark_runtime \
  --output /tmp/micu-rust-benchmark.json
```

脚本拒绝在 `MICU_RUN_LIVE_TESTS=1` 时运行。全部调用都指向同进程启动的
`127.0.0.1` mock Micu API/forward proxy；没有真实生图请求，也没有把远端渲染延迟计入结果。

RSS 由 macOS `ps -o rss=` 以 5–10ms 级轮询采样。协议启动项目重复 3 次并报告中位数；大
fixture 路径各运行 1 次。KiB 为 `ps` 原始值。

## 汇总

| 指标 | Python | Rust | Rust 变化 |
|---|---:|---:|---:|
| initialize 中位数 | 300.909 ms | 7.149 ms | 快 42.09× |
| tools/list 中位数 | 2.446 ms | 0.400 ms | 快 6.12× |
| idle RSS 中位数 | 66,080 KiB | 9,504 KiB | 降低 85.62% |
| server_info 后 RSS | 66,192 KiB | 9,664 KiB | 降低 85.40% |
| 24 MiB URL 图下载峰值 | 152,496 KiB | 12,336 KiB | 降低 91.91% |
| 17 MiB 图片 b64 JSON 峰值 | 179,776 KiB | 34,880 KiB | 降低 80.60% |
| 约 8 MiB 多图上传峰值 | 80,112 KiB | 10,496 KiB | 降低 86.90% |
| 接近 25 MiB JSON cap 峰值 | 158,128 KiB | 37,840 KiB | 降低 76.07% |
| 4 个 idle MCP 总 RSS | 240,112 KiB | 36,848 KiB | 降低 84.65% |

Rust idle RSS 约为 Python 的 14.4%，明显超过“至少降低约 50%”的目标。Rust 的启动与
`tools/list` 也都快于 Python。

## 协议原始样本

### Python

| 样本 | initialize ms | tools/list ms | idle RSS KiB | server_info ms | server_info RSS KiB |
|---:|---:|---:|---:|---:|---:|
| 1 | 577.133 | 2.561 | 66,080 | 4.508 | 66,192 |
| 2 | 300.909 | 2.426 | 66,080 | 2.916 | 66,208 |
| 3 | 296.819 | 2.446 | 66,032 | 2.551 | 66,144 |

### Rust

| 样本 | initialize ms | tools/list ms | idle RSS KiB | server_info ms | server_info RSS KiB |
|---:|---:|---:|---:|---:|---:|
| 1 | 29.756 | 0.917 | 9,504 | 0.863 | 9,664 |
| 2 | 6.279 | 0.348 | 9,504 | 0.624 | 9,696 |
| 3 | 7.149 | 0.400 | 9,424 | 0.663 | 9,648 |

第一样本包含首次动态库、文件系统与 page-cache 冷启动效应，因此门槛判断使用预先声明的
3 次中位数，同时保留第一样本而不隐藏。

## 大 fixture 原始数据

| 路径 | 实现 | idle KiB | peak KiB | 增量 KiB | final KiB | wall ms | 结果 |
|---|---|---:|---:|---:|---:|---:|---|
| 24 MiB URL 图 | Python | 65,904 | 152,496 | 86,592 | 150,176 | 153.398 | 成功 |
| 24 MiB URL 图 | Rust | 9,344 | 12,336 | 2,992 | 12,336 | 45.472 | 成功 |
| 17 MiB 图的 b64 JSON | Python | 65,984 | 179,776 | 113,792 | 179,776 | 279.557 | 成功 |
| 17 MiB 图的 b64 JSON | Rust | 9,360 | 34,880 | 25,520 | 34,880 | 263.712 | 成功 |
| 两张合计约 8 MiB reference | Python | 66,064 | 80,112 | 14,048 | 70,384 | 183.319 | 成功 |
| 两张合计约 8 MiB reference | Rust | 9,376 | 10,496 | 1,120 | 10,496 | 210.784 | 成功 |
| 25 MiB−4 KiB JSON body | Python | 65,856 | 158,128 | 92,272 | 158,128 | 106.271 | 预期无图片 |
| 25 MiB−4 KiB JSON body | Rust | 9,344 | 37,840 | 28,496 | 37,840 | 44.504 | 预期无图片 |

URL fixture 是完整可解码的小像素 PNG，带 24 MiB 合法 ancillary chunk，使传输体接近上限
而不把像素解码内存混入结论。Rust 在该路径仅增加 2,992 KiB，说明响应没有先聚合成 chunks
再 join，也没有产生接近响应体 2 倍的额外常驻内存。

b64 fixture 的原始图片为 17 MiB，编码后 JSON 仍低于 25 MiB API body cap。Rust 保留一个
受限 JSON buffer，并从借用的 base64 字符串流式解码到临时文件；峰值显著低于 Python，但仍
高于 URL 路径，主要成本就是不可避免的 JSON/base64 响应 buffer。

多图 fixture 为两张各略低于 4 MiB、合计略低于 8 MiB 的合法 PNG。Rust 会创建受保护的磁盘
快照并从独立 file handle 流式构造 multipart；没有把多图收集成 `Vec<Vec<u8>>`。

## 多进程原始数据

| 实现 | 进程 1 | 进程 2 | 进程 3 | 进程 4 | 总计 KiB |
|---|---:|---:|---:|---:|---:|
| Python | 55,440 | 55,520 | 63,120 | 66,032 | 240,112 |
| Rust | 9,216 | 9,248 | 9,216 | 9,168 | 36,848 |

## 解释与限制

- 本结果是同一台 Apple Silicon Mac 的本机证据，不代替 Linux/Windows 原生 runner。
- 大 fixture 各只有一次运行，适合验证内存结构和数量级，不应当当作精密微基准。
- Rust 多图 wall time略高于 Python（210.784 vs 183.319 ms），原因是安全输入快照与流式
  multipart；内存从 80,112 KiB 降到 10,496 KiB。这里没有为了速度取消安全快照。
- RSS final 没有强制 allocator 归还页面；峰值比较仍是进程实际可见 RSS，而非理论 allocation。
- 所有图片来自确定性本地 fixture，未提交生成图片，未消耗额度。

就本机门槛而言，内存、启动、tools/list 和 URL 流式目标全部通过。默认入口是否切换仍由完整
release gate 决定，尤其是尚未实际执行的 GitHub macOS Intel、Linux、Windows 原生 CI。
