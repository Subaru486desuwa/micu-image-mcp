# 原生 harness 验收与耗时优化

2026-10-09 在 Osaka（Linux ARM64、2 vCPU、约 12 GiB RAM）验证。项目代码与依赖、构建、测试均在 Osaka；
Mac 只读取已有 provider 配置和查看图片。未切换 CC Switch 的日常 provider，也未发布新版本或替换 Mac 二进制。

## 原生客户端与场景

| Harness | 推理模型 | 连接方式 |
| --- | --- | --- |
| OpenCode 1.18.35 | deepseek-flash | DeepSeek 官方 Chat Completions |
| Claude Code 2.1.295 | claude-sonnet-4-6 | 现有米醋订阅入口，原生 Claude 请求 |
| Claude Code 2.1.295 | deepseek-v4-pro | DeepSeek 官方 Anthropic-compatible 接口 |

模型凭据来自用户已有配置，测试进程通过 SSH stdin 和内存中的固定源转发器使用，不写入报告或临时 harness 配置。
Harness 使用隔离配置目录，禁用无关工具与插件；图片接口采用本地替身，以便只测工具选择和上下文管理。
测试不是手工指定 image session ID：模型自己读取工具结果，再决定后续 ID。

两组四轮场景：

1. 小猫跳伞 → 换成滑翔伞 → 独立海景人像 → 回到小猫改日落。
2. 小狗骑摩托 → 头盔改黄色 → 独立森林旅行者 → 回到小狗改雨夜。

三种组合均通过：24 个用户图片操作按 A → A → B → A 路由，版本 1 → 2 → 1 → 3。
额外检查父版本、成功历史长度、实际上传的前一版本路径和新任务约束隔离。
这是小规模控制场景结果，不能保证所有模型、措辞和超长聊天都正确；客户端自动绑定仍未实现。

## Claude 兼容故障及修复

Claude Code 采用 2026-07-28 协议，成功连接后持续重试 tools/list，却未向模型提供五个图片工具。
客户端日志精确报错：目录结果缺少 ttlMs 和 cacheScope。Rust SDK 的默认列表构造器未填这两个可选字段。

服务端现在根据当前请求协议版本，为 tools/list、prompts/list、resources/list、resources/templates/list
补充 ttlMs=0、cacheScope=private。旧握手版本保持原有响应格式，不添加这些字段。
这符合[官方 TypeScript SDK 的新版协议说明](https://ts.sdk.modelcontextprotocol.io/v2/migration/support-2026-07-28)。

回归测试先复现失败再修复，同时验证新协议目录元数据与旧协议格式。
修复后原生 Claude 在首次读取目录时拿到五个工具，不再发生 250/500/1000 ms 的错误重试。
这两个字段管理工具目录的协议缓存，不是图片会话 JSON 的过期或清理策略。

测试转发器另外修正了两处验收环境问题：保留原生 CLI 的非认证请求头（包括 x-app），
以及让图片替身按请求返回 1024x1024，避免尺寸偏差诱发额外生成。

## 提示与过程开销

原先长工具说明把会话规则埋在使用手册后面，DeepSeek 有一组测试省略了全部可选 session_id。
说明已改为简洁、操作导向的定义，将新任务必须创建会话、编辑复用会话放在最前。
五个工具名称、输入及输出 schema 未改变；完整参数和使用说明仍在文档与 server_info 中。

| 指标 | 原说明 | 精简说明 |
| --- | ---: | ---: |
| 五个 description 总字节 | 23,127 | 3,256 |
| DeepSeek 请求中工具定义字节 | 31,050 | 10,640 |
| 首个工具推理请求的上下文 tokens | 9,924 | 3,607 |

OpenCode 每轮启动新 CLI 的三次后续调用为 6.65、6.59、7.10 秒；
改用常驻 opencode serve 与 run --attach 后，同主题后续调用为 4.97、3.87、3.87 秒。
另一组小狗主题为 4.46、3.97、5.83 秒。首次调用仍包含服务启动和标题生成等费用，应单独计入。
这些是小样本实测，云端排队、缓存和输出长度会影响结果，不能把每次耗时变化全归因于提示优化。

本地图片替身上的 MCP 工具 RPC 中位数约 36–68 ms；模型请求中位数约 1–3 秒，
原生客户端启动又增加额外时间。真实图片渲染等待单独计量，不把它冒充本地代码耗时。

## 并发与线程

使用 Python MCP SDK 并发请求同一 Rust 进程，固定本地 API 延迟 200 ms，各设置重复 3 次：

| Tokio 工作线程 | 四任务串行中位数 | 四个独立会话并发中位数 |
| ---: | ---: | ---: |
| 1 | 952.57 ms | 247.59 ms |
| 2 | 953.58 ms | 247.74 ms |
| 4 | 954.68 ms | 248.27 ms |

独立会话可以重叠等待 API。增加线程没有带来可测收益，保留运行时默认配置。
同一会话的两次并发请求只有一次进入上游，另一轮返回忙，版本只推进一次。
高分辨率的跨进程锁及路径、图片、大小校验继续生效，没有为性能放宽保护。

## 复跑

模型测试需要显式 MICU_RUN_HARNESS_TESTS=1；native_harness_eval 从 stdin 读取配置好的 provider 凭据，
不支持通过 argv 传密钥。默认图片为本地替身；--live-images 还必须显式 MICU_RUN_LIVE_TESTS=1，
并从现有 Codex micu-image 配置读取主站图片凭据。使用新的 --output 目录，避免重复或覆盖证据。

```bash
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo build --locked --release --all-features
.venv/bin/python -m pytest -q
MICU_RUN_LIVE_TESTS=0 .venv/bin/python -m tests.contract.benchmark_session_concurrency \
  --repeat 3 --output tests/reports/session-concurrency.json
```

当前验证：Debug/Release 构建、格式、严格 Clippy、Rust 全量 90 项通过；Python 401 passed / 5 skipped。
五项跳过属于原有在线或冻结参考测试。图片、native traces 与 JSON 报告位于被 Git 忽略的 tests/reports。
Mac 和 Windows 的新版构建尚未在本轮执行。

## 真实生图抽测

OpenCode + DeepSeek Flash 使用生产米醋图片入口完成了小猫、滑翔伞修改、独立海景、回小猫日落的链路，
全部成功操作为 A → A → B → A，父版本与上传图片一致。海景第一次请求上游返回 400，模型调整提示词后重试成功；
失败没有提交版本。原生链路实际 5 次图片 POST、4 张成功图片，均为 1024x1024。

人工查看还发现：第二步虽准确传入 paraglider，但上游画成了带金属支架的三角滑翔翼。
验收流程额外显式调用一次 image_edit(A)，补充软翼、弯月形翼室和无金属支架的几何约束，
得到正确软翼并保留同一小猫、日落和构图。该校正不是模型自动路由样本，不计入上述 24 个决策。
这说明会话连续性与视觉物体精度需要分开验收，必要时须给出更明确的形态约束。

本轮真实图片合计 6 次 POST，按用户给出的 0.1 元/次名义计算 0.6 元；实际后台扣费未核对。
文字模型调用另按各 provider 的规则计费，不将图片单价套用于文字推理。
