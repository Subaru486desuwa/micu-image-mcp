# 连续编辑图片

Rust 的 `image_generate` 和 `image_edit` 共用持久编辑会话。首次生成即可保存描述、约束和
图片；同一聊天后续只需传返回的会话 ID 和本轮指令，就会继承该分支的上下文与所选图片。

## 从第一次生成开始记忆

```json
{
  "prompt": "画一个穿红衣服的女孩，日落海边，保持自然的摄影构图",
  "session_id": "new",
  "context": {"identity": "同一女孩的面容和发型", "clothing": "红色衣服", "background": "日落海边"}
}
```

成功返回 `saved[0].path` 及 `session.id`，生成描述作为版本 1 写入会话。第二轮可以继续使用
`image_generate`，也可以改用 `image_edit`：

```json
{
  "prompt": "衣服改成蓝色，保留背景和人物面容",
  "session_id": "edit_<替换为返回的32位标识>",
  "context": {"clothing": "蓝色衣服"}
}
```

已有会话的 `image_generate` 会上传所选版本图片并调用参考编辑，避免重新从零生成丢失视觉状态；
返回的 `saved` 仍是列表。`image_edit` 返回的 `saved` 仍是单个对象。
会话生成要求 `n=1`；多张候选可以先单次生成，再选一张通过编辑工具开始会话。

MCP 客户端需要将 `session.id` 关联到当前聊天，后续每轮持续传入。MCP 不会自动收到宿主的
完整聊天记录，也不会用全局最近会话猜测聊天身份。不同 ID 的记忆相互隔离；省略 ID 保留单次调用行为。

## 从已有图片开始记忆

已有图片也可直接通过 `image_edit` 创建会话：

```json
{
  "prompt": "背景改成日落海边，保留人物面容、红色衣服和构图",
  "image_path": "/absolute/path/generated.png",
  "session_id": "new",
  "context": {
    "identity": "保留同一人物的面容、发型和姿势",
    "clothing": "红色衣服",
    "background": "日落海边",
    "composition": "保留原来的取景和人物位置"
  }
}
```

成功返回 `saved.path`、`session.id`、`session.revision` 和当前有效的 `session.context`。
把返回的 ID 用于下一轮，无需再次传图片路径：

```json
{
  "prompt": "衣服改成蓝色，保留海边背景和人物面容",
  "session_id": "edit_<替换为返回的32位标识>",
  "context": {"clothing": "蓝色衣服"}
}
```

`context` 按键继承：新值覆盖同名旧值，`null` 移除对应要求。移除约束只取消后续必须保持
该属性的要求，不会自动撤销图片中已有的修改。本轮 `prompt` 优先于有效约束及历史指令；
如果本轮改变长期属性，应同步更新对应键，避免后续轮次继续沿用旧要求。

已有会话中省略 `model`、`size`、`quality` 时继承所选版本；新会话及单次编辑保持原有默认值。
首次生成省略 `size` 时继续从 prompt 推断尺寸。
蒙版只用于本轮，不继承。省略 `session_id` 时仍是原有单次编辑，不保存会话记录。

## 版本与失败处理

- 省略 `parent_revision` 时使用最近成功提交的版本。
- 指定已有版本号可从该版本继续，创建新分支。上下文只包含该分支的祖先。
- 新版本号在会话内递增；`session.parent_revision` 表示实际编辑来源。
- 图片生成、下载或校验失败均不会提交版本或保存失败轮次的约束。
- 图片保存后若会话元数据提交失败，返回 `ok: false`、已保存的 `saved` 和未推进的会话状态。
- 同一会话有正在执行的调用时返回忙，不发起另一笔上游请求；稍后重试。
- 会话最多 128 个版本；约束最多 32 项、8192 字符，单项值最多 2048 字符；
  合成后的提示词最多 32000 字符。达到限制后，可以用所选图片路径创建新会话。

## 上下文保存方式

首次生成调用 `/v1/images/generations` 并记录描述与约束；会话后续轮次调用 Image2 的 `/v1/images/edits`：上传所选版本图片，把有效约束、该分支的成功编辑
历史和本轮指令组织成提示词。历史明确作为背景说明；本轮指令和当前约束优先，已替换的旧要求
不应再次应用。这个方案不需要上游提供 `previous_response_id`。

会话元数据保存在 `MICU_SAVE_DIR_ROOT/.micu-sessions/<session-id>/revision-<n>.json`，
重启 MCP 后可继续。记录不保存 API key；输出图片仍位于正常输出目录。会话使用跨进程文件锁，
版本以完整写入后的不可覆盖记录提交。元数据也有读取大小限制和版本关系校验。

输入路径白名单、文件大小和图片格式校验继续生效。如果设置 `MICU_INPUT_ROOT`，它需要涵盖
待编辑的输出图片，否则继续编辑会被拒绝。不要通过放宽校验绕过这个限制。

## 验证

新增会话功能的离线测试全部使用 Rust 和本地替身，不产生图片费用：

```bash
cargo build --locked --bin micu-image-mcp
.venv/bin/python -m pytest -q tests/contract/test_image_sessions.py
cargo test --locked --lib tools::edit
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

测试通过真实 MCP STDIO 与本地 HTTP 替身检查首次生成记忆、跨工具连续调用、进程重启恢复、
上游输入图哈希、上下文继承与覆盖、失败不推进、分支与独立会话隔离、
重启恢复、会话锁、凭据脱敏和路径边界。完整仓库检查另有用于旧实现兼容的 Python 测试。

视觉验收应连续检查：原图 → 修改背景 → 修改人物内容；最后一轮需要保留前一轮背景、
人物身份和构图。离线测试证明请求与状态正确，视觉效果需要单独授权的在线验证。

仓库提供 Rust 验收程序，发起三次图片工具调用，从首次生成建立会话，并在每轮之间重启 MCP：

```bash
cargo build --locked --bin micu-image-mcp
MICU_RUN_LIVE_TESTS=1 cargo run --locked --example live_edit_session -- /tmp/micu-edit-acceptance
```

程序读取 `MICU_API_KEY`；没有配置时使用隐藏终端输入。只连接固定的米醋地址，凭据不进入
命令行参数或报告。结果与 `live-report.json` 保存到指定目录；程序完成后仍需查看图片验收。

2026-10-07 的旧版验收程序在 Osaka 完成了 1K 原图生成、海边背景编辑、重启后的蓝衣编辑，人物与
海边背景保持一致。该次米醋接口对 GPT Image 2.5 的 `quality=high` 返回 HTTP 400，明确要求
`low`；验收程序因此默认使用 `low`。这是本次上游线路的实际限制，生产工具仍保留原有参数范围。
程序可通过 `MICU_LIVE_QUALITY`、`MICU_LIVE_GENERATE_MODEL` 和 `MICU_LIVE_EDIT_MODEL` 指定
后续验收参数。设置 `MICU_LIVE_CHECK_MODELS=1` 时只查询可用模型，不发起图片请求。

新版程序改为从首次生成开始记忆。2026-10-09 已使用主站、Flare、quality=low 验证生成起始会话及两轮连续编辑，每轮重启 MCP；图片视觉检查通过。

2026-10-09 在 Osaka（Linux）完成本轮验证：Python 离线套件 400 passed / 5 skipped，
Rust 全量 90 项通过，`cargo fmt --check`、严格 Clippy、二进制构建和 `git diff --check` 通过。
新增协议测试覆盖生成起始记忆、跨工具连续调用、进程重启、旧版本分支、独立会话隔离、
API 与落盘失败、原始约束长度、输入路径限制，以及 2026-07-28 无状态 MCP 请求。
Mac 和 Windows 运行尚未执行；Osaka 付费验收结果见下文。

本地占用、保存位置与清理方式参见[会话存储管理](session-storage.md)。

2026-10-09 在线质量矩阵和连续会话实测详见[会话存储管理](session-storage.md#在线质量档位验证)。

## 同一聊天中的多张图片

图片会话按图片任务管理，同一聊天可以有多个 ID。用户只需自然表达生成、修改和切换图片，
GPT/客户端负责从成功工具返回值中保存 ID，再在工具参数中选择对应会话。

| 用户意图 | 工具调用 | 会话结果 |
| --- | --- | --- |
| 生成小猫跳伞的图片 | image_generate，session_id=new | A，版本 1 |
| 把它的降落伞换成滑翔伞 | image_edit，session_id=A | A，版本 2 |
| 再生成成年女性在沙滩海景晒太阳的图片 | image_generate，session_id=new | B，版本 1 |
| 回头修改之前的小猫图 | image_edit，session_id=A | A，版本 3 |

工具说明已写入上述规则，避免将“同一聊天”误解为“始终使用同一图片会话”。新任务只传其自身约束，
不默认沿用旧图的主体或背景要求。只有指代不清时才向用户确认图片；用户无需抄写 ID。

当前选择由 GPT/调用方完成，服务端按传入 ID 执行；没有自动接收宿主聊天 ID，
也没有实现独立的语义分类器或客户端“聊天 ID + 图片任务 ID → session_id”绑定管理器。
因此说明能引导正确选择，但不能保证模型不会漏传或误选。后续要由程序强制控制时，需要客户端适配层。
