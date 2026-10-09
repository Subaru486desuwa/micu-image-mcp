# v0.4.1 上下文记忆配图

这些图片用于 README 和项目宣传，来自 2026-10-09 的真实 MCP 验收。
图像由 Osaka 的 `micu-image-mcp` 调用 `https://www.micuapi.ai` 生成或编辑；文字模型负责选择工具和图片会话。

## 海报与四步图

- `context-memory-poster.png`：1600 × 900 分享海报；同名 SVG 为可编辑排版源文件。
- `four-turn-demo.png`：1600 × 750 原生调用示例；同名 SVG 展示完整四张实际结果。
- SVG 内嵌 JPEG，离线打开无需依赖外部图片地址。PNG 固定排版与字体，适合 README 和转发。

| 公开素材 | 会话 / 版本 | 来源 |
| --- | --- | --- |
| cat-parachute.jpg | A · r1 | 原生 OpenCode + DeepSeek Flash 首次生成 |
| cat-wing.jpg | A · r2 | 原生客户端继续编辑；实际返回硬质滑翔翼 |
| beach-new-topic.jpg | B · r1 | 原生客户端另开主题；上游失败一次后重试成功 |
| cat-sunset.jpg | A · r3 | 原生客户端回到小猫，继续编辑日落背景 |
| cat-soft-paraglider.jpg | A · r4 | 原生测试结束后，人工追加软翼形态约束的精修 |

A/B 是便于阅读的会话简称，r 表示成功版本号。海报主图使用 A · r4，小图为 A · r1，
不将它们标为相邻两次自动调用。四步图保留原生运行的 A → A → B → A 顺序。
原生链路有五次图片 POST、四张成功图片，人工精修另有一次 POST；失败没有提交版本。

JPEG 仅压缩原始图片，保留原构图和像素尺寸；展示拼图采用等比例排版。
`provenance.json` 保存原图文件名、SHA-256、版本与调用来源，不包含凭据或私有日志。
完整测试目录与报告继续保留在被 Git 忽略的 `tests/reports/`；这里只公开经挑选的文档配图。
本次 README 更新复用已有图片，没有新增付费图像请求。
