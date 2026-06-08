<p align="center">
  <img src="assets/banner.svg" alt="MICU IMAGE — 米醋 gpt-image-2 / grok image MCP server" width="820">
</p>

# 米醋画图 MCP

把 [米醋](https://www.micuapi.ai) 的图像接口包装成 MCP server，让 Claude Code / Codex / Cursor 等 MCP 客户端直接生图、改图、批处理、多图参考。

默认使用 `gpt-image-2` / `gpt-image-2-pro`。可选配置 `MICU_GROK_API_KEY` 后，也能走米醋 Grok 图像通道。

注意：米醋后台的 Image2 分组和 Grok 分组通常是两把不同的 key：

- `MICU_API_KEY`: Image2 分组 token，必须能看到 `gpt-image-2` / `gpt-image-2-pro`
- `MICU_GROK_API_KEY`: Grok 图像分组 token，必须能看到 `grok-imagine-image-*`

如果把 Grok 分组 token 填进 `MICU_API_KEY`，运行时会出现类似 `分组 grok 下模型 gpt-image-2 无可用渠道` 的错误。

当前实测 Grok 图像模型包括：

- `grok-imagine-image-lite`
- `grok-imagine-image`
- `grok-imagine-image-pro`
- `grok-imagine-image-edit`

---

## 功能

| Tool | 说明 |
|---|---|
| `image_generate` | 文生图。米醋 image2 支持 1K / 2K / 4K；Grok 支持 1K / 2K 路由 |
| `image_edit` | 单图参考/编辑。image2 走 edits 或 `reference_image`；Grok 走 `reference_image` |
| `image_batch_edit` | 多张图逐张同指令处理 |
| `image_multi_reference` | 2-10 张参考图融合成 1 张新图；Grok 走 `image_urls` |
| `server_info` | 查看 base URL、模型、size 规则、重试策略、安全约束 |

第一次使用前，让 LLM 调一次 `server_info`，可以看到当前运行时配置和可用能力。

---

## Grok 与 GPT Image2 功能差异

| 能力 | `gpt-image-2` / `gpt-image-2-pro` | 米醋 Grok 图像模型 |
|---|---|---|
| 默认用途 | 主通道，覆盖文生图、图生图、批量编辑、多图参考 | 可选通道，适合快速文生图、单图参考、多图参考 |
| 可选模型 | `gpt-image-2`, `gpt-image-2-pro` | `grok-imagine-image-lite`, `grok-imagine-image`, `grok-imagine-image-pro`, `grok-imagine-image-edit` |
| `image_generate` 文生图 | 支持 1K / 2K / 4K；2K/4K 自动切 pro，强制 `n=1` | 支持 1K / 2K 路由；`n` 会传给后端，实际返回张数以响应为准 |
| `image_edit` 单图参考/编辑 | 1K 走 `/v1/images/edits`；2K 走 `reference_image`；4K 参考图入口拒绝 | 走 `/v1/images/generations` + `reference_image`；4K 会映射到 `resolution=2k` |
| 局部 mask | 仅 1K edits multipart 支持 alpha mask；2K 不支持 | 当前不支持 mask，传入会忽略并写入 `notes` |
| `image_multi_reference` 多图参考 | 2-10 张参考图；1K 稳定，2K 可能 fallback，4K 入口拒绝 | 2-10 张参考图走 `image_urls`；实测可用，按 `resolution` + `aspect_ratio` 映射 |
| `image_batch_edit` 批量逐张编辑 | 支持 1K；non-pro 5 并发，pro 串行 | 当前不支持 Grok 批量逐张编辑 |
| size 校验 | `WxH`，边长 256-4096，W/H 必须是 8 的倍数 | 只校验 `WxH` 正整数，不强制 8 倍数和 4096 边长 |
| 实际输出尺寸 | ≥4MP 通常严格 1:1；≤2.25MP 会被代理处理到约 1.57MP | 不保证等于请求 `WxH`，以 `saved.actual_size` 为准 |
| 重试/限流 | 2K/4K 使用跨进程锁，避免多个 MCP 同时打 pro 队列 | 不走高分辨率锁；可恢复错误仍自动重试并记录到 `notes` |
| 配置变量 | `MICU_API_KEY`, `MICU_MODEL`, `MICU_BASEURL` | `MICU_GROK_API_KEY`, `XAI_MODEL`；默认复用 `MICU_BASEURL` |

---

## 一键安装

```bash
git clone https://github.com/Subaru486desuwa/micu-image-mcp.git
cd micu-image-mcp
python install.py
```

脚本会：

1. 检查 Python >= 3.10
2. 安装依赖
3. 交互配置米醋 Image2 分组 API key、输出目录
4. 可选配置米醋 Grok 生图分组 token
5. 写入 `~/.claude.json` 和 `~/.codex/config.toml`
6. 启动 server 做一次 initialize 握手

安装脚本会用 `/v1/models` 做轻量校验，尽量在安装阶段发现 key 分组粘错的问题。

非交互安装：

```bash
MICU_API_KEY=sk-... \
MICU_GROK_API_KEY=sk-... \
MICU_SAVE_DIR=~/Pictures/micu-out \
python install.py --yes
```

`--yes` 模式下如果 `MICU_API_KEY` 看不到 `gpt-image-2`，或 `MICU_GROK_API_KEY` 看不到 `grok-imagine-image-*`，安装会直接失败，避免写入错误配置。

常用选项：

```bash
python install.py --no-codex
python install.py --no-claude
python install.py --mirror tsinghua
python install.py --baseurl https://www.micuapi.ai
```

卸载/重置（仅删 MCP 配置节，不动 pip 包）：

```bash
python install.py --reset
# 想顺手卸 pip 包再加:
python -m pip uninstall -y micu-image-mcp
```

`--reset` 会备份原配置后，从 `~/.claude.json` 移除 `mcpServers.micu-image`、从 `~/.codex/config.toml` 移除 `[mcp_servers.micu-image]` 整节，其他 MCP server 节点保持不动。

安装完成后会自动跑一次 `initialize` 握手 + `tools/list`，预期能看到 5 个 tool：`image_generate / image_edit / image_batch_edit / image_multi_reference / server_info`。安装日志里看到这 5 个名字才算装好。然后重启 Claude Code / Codex，让 LLM 调 `server_info` 验证。

---

## Grok 路径

Grok 走米醋中转，base URL 默认仍是：

```text
https://www.micuapi.ai
```

只需要额外配置：

```bash
MICU_GROK_API_KEY=sk-...
XAI_MODEL=grok-imagine-image-lite
MICU_GROK_SIZE_MODE=contain
```

Grok 的 `size` 不套用 image2 的 8 倍数和 4096 边长约束。本地只检查 `WxH` 格式，然后映射为：

- `resolution`: `1k` 或 `2k`
- `aspect_ratio`: 最接近的比例，如 `1:1`、`16:9`、`9:16`

注意：Grok 后端返回像素不保证严格等于请求的 `WxH`。MCP 默认会在保存前用 Pillow 把 Grok 输出归一化到请求尺寸，`MICU_GROK_SIZE_MODE` 可选：

| 值 | 行为 |
|---|---|
| `contain` | 默认。等比缩放，补边到请求尺寸，不裁主体 |
| `cover` | 等比缩放并居中裁切，铺满请求尺寸 |
| `stretch` | 直接拉伸到请求尺寸，可能变形 |
| `backend` | 不做本地后处理，保留 Grok 后端原始像素 |

建议仍优先用常见比例和不太小的边长，例如 `1024x1024`、`1536x1024`、`1024x1536`、`1501x1001`。过小或很奇异的比例可能被米醋 Grok 后端返回 500，MCP 会自动重试并在 `notes` 里记录。

---

## Size 规则

image2 路径：

- W/H 必须是 8 的倍数
- W/H 必须在 256 到 4096 范围内
- 1K 福利档可能被代理处理到约 1.57MP
- 2K/4K 自动切 `gpt-image-2-pro`
- 2K/4K 强制 `n=1` 并加跨进程锁，避免多个 MCP 同时打爆 pro 队列

推荐 size：

| 档位 | 推荐值 |
|---|---|
| 1K | `1024x1024`, `1280x720`, `720x1280`, `1024x1536`, `1536x1024` |
| 2K | `2048x2048`, `2048x1152`, `1152x2048` |
| 4K | `3840x2160`, `2160x3840` |

Grok 路径：

- 不强制 8 倍数
- 当前按 1K / 2K 路由
- 4K 请求会映射到 `resolution=2k`

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MICU_API_KEY` | 空 | 米醋 image2 token |
| `MICU_BASEURL` | `https://www.micuapi.ai` | 米醋 base URL |
| `MICU_MODEL` | `gpt-image-2` | image2 默认模型 |
| `MICU_GROK_API_KEY` | 空 | 米醋 Grok 图像 token |
| `XAI_MODEL` | `grok-imagine-image-lite` | Grok 默认模型 |
| `MICU_GROK_SIZE_MODE` | `contain` | Grok 保存前尺寸归一化策略：`contain` / `cover` / `stretch` / `backend` |
| `MICU_SAVE_DIR` | `~/Pictures/micu-out` | 默认输出目录 |
| `MICU_SAVE_DIR_ROOT` | 同输出目录 | 输出安全根目录 |
| `MICU_USE_SHELL_PROXY` | `0` | 设为 `1` 才读取 shell 代理 |

兼容旧 Grok 变量 `XAI_API_KEY` / `GROK_API_KEY`，但推荐新配置统一使用 `MICU_GROK_API_KEY`。

---

## 手动配置

Claude Code:

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "/path/to/python",
      "args": ["/absolute/path/to/micu-image-mcp/server.py"],
      "env": {
        "MICU_API_KEY": "sk-...",
        "MICU_GROK_API_KEY": "sk-...",
        "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
        "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out",
        "XAI_MODEL": "grok-imagine-image-lite"
      }
    }
  }
}
```

Codex:

```toml
[mcp_servers.micu-image]
command = "/path/to/python"
args = ["/absolute/path/to/micu-image-mcp/server.py"]

[mcp_servers.micu-image.env]
MICU_API_KEY = "sk-..."
MICU_GROK_API_KEY = "sk-..."
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
XAI_MODEL = "grok-imagine-image-lite"
```

---

## 性能 / 压力测试

`tests/` 下两个独立脚本，直接 in-process import `server.py` 调 `image_generate`，不走 stdio MCP（避免子进程开销污染样本）。需要至少一个有效 key 才能跑真实请求；不带 key 用 `--dry-run` 也能验证脚本/导入/校验链路。

报告默认落到 `tests/reports/<title>_<ts>.{json,md}`，已被 `.gitignore` 排除。生成的图扔到 `/tmp/micu-bench/<label>/`，不会污染你的 `~/Pictures/micu-out`。

### 性能基线 `tests/perf_bench.py`

串行跑 image2 / Grok 在不同 `size` 下的 `image_generate`，记录单次延迟、actual_size 偏差、保存后字节数。

```bash
# smoke (默认): image2 + grok 各 2 张, 共 4 张 → ~3-5 分钟
python tests/perf_bench.py

# 完整 sweep, 每组重复 3 次
python tests/perf_bench.py --full --repeat 3

# 只跑某一通道
python tests/perf_bench.py --channels image2
python tests/perf_bench.py --channels grok

# 干跑 (不打 API, 只验证脚本链路)
python tests/perf_bench.py --dry-run
```

报告 markdown 表头：`group | n | ok | fail | rate | p50_ms | p95_ms | mean_ms | actual_match`。`actual_match` 是 PNG header 读出的实际像素严格等于请求 size 的比例 — 对 image2 1K 福利档预期会偏低（被代理压到 ~1.57MP），2K/4K 严格 1:1，Grok 由 `MICU_GROK_SIZE_MODE` 决定。

### 并发压力 `tests/stress_concurrent.py`

验证：
1. 1K 单进程多并发 → 进程内不卡，吞吐近似线性
2. ≥2K 多进程并发 → 进程内 `asyncio.Semaphore(1)` + 跨进程 `flock` 双层锁串行
3. CF 524 / 上游 5xx → 重试/fail-fast 策略
4. Grok 路径不走 ≥2K 锁，可线性并发

```bash
# in-process 并发 (默认 smoke, image2 1K x 3)
python tests/stress_concurrent.py

# 验证 ≥2K 锁串行
python tests/stress_concurrent.py --size 2048x2048 --concurrency 4

# 跨进程模式 (spawn N 个子进程, 模拟多 Claude Code 窗口)
python tests/stress_concurrent.py --mode multiprocess --concurrency 3 --size 2048x2048

# Grok 并发
python tests/stress_concurrent.py --model grok-imagine-image-lite --concurrency 5
```

报告关键派生指标：

| 指标 | 含义 |
|---|---|
| `total_wall_ms` | 整批耗时（从 gather 到全部返回） |
| `serial_estimate_ms` | 所有成功请求 wall_ms 之和（串行下界） |
| `concurrency_efficiency` | `total_wall_ms / serial_estimate_ms`。≈ 1 → 强串行（锁生效）；≈ 1/N → 强并发；中间 → 部分排队 |
| `lock_wait_observed` | notes 里出现 “等待跨进程 ≥2K 锁” 的请求数（>2s 才记） |

> 提醒：image2 真实并发会按米醋后台 pro 队列限流计费，跑 `--concurrency` ≥ 3 之前先确认账户额度。dry-run / 401 路径不计费。
