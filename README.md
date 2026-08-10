<p align="center">
  <img src="assets/banner.svg" alt="MICU IMAGE — 米醋 gpt-image-2 MCP server" width="820">
</p>

# 米醋画图 MCP

把 [米醋](https://www.micuapi.ai) 的图像接口包装成 MCP server，让 Claude Code / Codex / Cursor 等 MCP 客户端直接生图、改图、批处理、多图参考。

当前仅支持 `gpt-image-2` / `gpt-image-2-pro`，`MICU_API_KEY` 必须能看到这两个模型。
Grok 生图渠道暂时关闭，待服务器支持后再启用；即使配置旧的 Grok 环境变量，安装器也不会写入，工具调用会在发出请求前拒绝 Grok 模型。

---

## 功能

| Tool | 说明 |
|---|---|
| `image_generate` | 文生图。米醋 image2 支持 1K / 2K / 4K |
| `image_edit` | 单图参考/编辑。走 `/v1/images/edits`（1K ~1.57MP，2K best-effort 真 2K） |
| `image_batch_edit` | 多张图逐张同指令处理 |
| `image_multi_reference` | 2-10 张参考图融合成 1 张新图 |
| `server_info` | 查看 base URL、模型、size 规则、重试策略、安全约束 |

第一次使用前，让 LLM 调一次 `server_info`，可以看到当前运行时配置和可用能力。

---

## 使用教程

面向 Cursor / Claude Code / Codex 用户的完整 MCP 使用指南见 [docs/MCP使用教程.md](docs/MCP使用教程.md)，
涵盖工具选型、尺寸规则、环境变量与故障排查（含 Clash/Surge fake-ip 落盘问题）。

---

## 当前模型范围

所有工具与压测脚本仅接受 `gpt-image-2` 和 `gpt-image-2-pro`。2K/4K 会自动切换到 pro；Grok 相关实现暂时保留为休眠代码，服务器恢复支持后可重新开放。

---

## 一键安装

方式一：用 Git 下载源码（推荐）。

```bash
git clone --depth 1 https://github.com/Subaru486desuwa/micu-image-mcp.git micu-image-mcp
cd micu-image-mcp
python install.py
```

以后更新同一个目录：

```bash
cd micu-image-mcp
git pull --ff-only
python install.py
```

方式二：用 npm 临时下载源码（适合没有 `git` 命令的环境）。这个项目不是 npm 包，不要用 `npm install micu-image-mcp`；下面的命令只是通过 `tiged` 从 GitHub 拉取源码，仍需要当前网络能访问 GitHub。

```bash
npm exec --yes tiged -- github:Subaru486desuwa/micu-image-mcp#main micu-image-mcp
cd micu-image-mcp
python install.py
```

脚本会：

1. 检查 Python >= 3.10
2. 安装依赖
3. 交互配置米醋 Image2 分组 API key、输出目录
4. 写入 `~/.claude.json` 和 `~/.codex/config.toml`
5. 启动 server 做一次 initialize 握手

安装脚本会用 `/v1/models` 做轻量校验，尽量在安装阶段发现 key 分组粘错的问题。

非交互安装：

```bash
MICU_API_KEY=sk-... \
MICU_SAVE_DIR=~/Pictures/micu-out \
python install.py --yes
```

`--yes` 模式下如果 `MICU_API_KEY` 看不到 `gpt-image-2` / `gpt-image-2-pro`，安装会直接失败，避免写入错误配置。

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

## 尺寸能力矩阵 / Size capability

实测确认的真实能力（image2 路径）。1K 档可靠输出 ~1.57MP；纯文生图 2K/4K 真分辨率可用（pro + 重试）；带参考图 2K 为 best-effort 真 2K。

| 场景 | 可靠性 | 实际输出 |
|---|---|---|
| ≤1.57MP（1K 档，所有 tool） | 可靠、快 | ~1.57MP（福利档） |
| 2K/4K 纯文生图（`image_generate`，无参考图） | 真 2K/4K 可用 | 自动切 pro + MCP 重试吸收瞬时 524，实测真返回 2048² / 3840×2160，~80s/张（高负载偶慢/偶失败） |
| 带参考图 2K（`image_edit` / `image_multi_reference`） | best-effort 真 2K | 走 `/v1/images/edits`，约 2/3 成功真返回 2048²；524 时 fallback chat → ~1.57MP |
| 带参考图 4K | 已禁用 | 入口拒绝（origin > 120s 撞 CF 524） |

说明：

- `/v1/images/edits` 是米醋唯一真正消费输入图的端点。1K 档稳定输出 ~1.57MP；2K 档自动切 pro 后 best-effort 真 2K（压测约 2/3 成功真返回 2048×2048，524 时 fallback chat stream → ~1.57MP，较慢 2-4 分钟/单次）。
- 旧的 `generations + reference_image`（单图 2K）和 `generations + image_urls`（多图）路径要么 524 断流、要么参考图被静默忽略，已废弃。
- 带参考图想要真 4K 用**两步法**：先出一张 ~1.57MP/2K 的综合/编辑图 → 再用 `image_generate` 描述同场景升 4K（`image_generate` 4K 真分辨率可用，自动切 pro + MCP 重试吸收瞬时 524）。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `MICU_API_KEY` | 空 | 米醋 image2 token |
| `MICU_BASEURL` | `https://www.micuapi.ai` | 米醋 base URL |
| `MICU_MODEL` | `gpt-image-2` | image2 默认模型 |
| `MICU_SAVE_DIR` | `~/Pictures/micu-out` | 默认输出目录 |
| `MICU_SAVE_DIR_ROOT` | 同输出目录 | 输出安全根目录 |
| `MICU_USE_SHELL_PROXY` | `0` | 设为 `1` 才读取 shell 代理 |

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
        "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
        "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out"
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
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

---

## 性能 / 压力测试

`tests/` 下两个独立脚本，直接 in-process import `server.py` 调 `image_generate`，不走 stdio MCP（避免子进程开销污染样本）。需要至少一个有效 key 才能跑真实请求；不带 key 用 `--dry-run` 也能验证脚本/导入/校验链路。

报告默认落到 `tests/reports/<title>_<ts>.{json,md}`，已被 `.gitignore` 排除。生成的图扔到 `/tmp/micu-bench/<label>/`，不会污染你的 `~/Pictures/micu-out`。

### 性能基线 `tests/perf_bench.py`

串行跑 `gpt-image-2` / `gpt-image-2-pro` 在不同 `size` 下的 `image_generate`，记录单次延迟、actual_size 偏差、保存后字节数。

```bash
# smoke（默认）：两个 Image2 模型各 1 张
python tests/perf_bench.py

# 完整 sweep, 每组重复 3 次
python tests/perf_bench.py --full --repeat 3

# 干跑 (不打 API, 只验证脚本链路)
python tests/perf_bench.py --dry-run
```

报告 markdown 表头：`group | n | ok | fail | rate | p50_ms | p95_ms | mean_ms | actual_match`。`actual_match` 是 PNG header 读出的实际像素严格等于请求 size 的比例；1K 福利档预期会偏低（被代理压到 ~1.57MP），2K/4K 严格 1:1。

### 并发压力 `tests/stress_concurrent.py`

验证：
1. 1K 单进程多并发 → 进程内不卡，吞吐近似线性
2. ≥2K 多进程并发 → 进程内 `asyncio.Semaphore(1)` + 跨进程 `flock` 双层锁串行
3. CF 524 / 上游 5xx → 重试/fail-fast 策略
4. `--model` 仅接受 `gpt-image-2` / `gpt-image-2-pro`

```bash
# in-process 并发 (默认 smoke, image2 1K x 3)
python tests/stress_concurrent.py

# 验证 ≥2K 锁串行
python tests/stress_concurrent.py --size 2048x2048 --concurrency 4

# 跨进程模式 (spawn N 个子进程, 模拟多 Claude Code 窗口)
python tests/stress_concurrent.py --mode multiprocess --concurrency 3 --size 2048x2048

```

报告关键派生指标：

| 指标 | 含义 |
|---|---|
| `total_wall_ms` | 整批耗时（从 gather 到全部返回） |
| `serial_estimate_ms` | 所有成功请求 wall_ms 之和（串行下界） |
| `concurrency_efficiency` | `total_wall_ms / serial_estimate_ms`。≈ 1 → 强串行（锁生效）；≈ 1/N → 强并发；中间 → 部分排队 |
| `lock_wait_observed` | notes 里出现 “等待跨进程 ≥2K 锁” 的请求数（>2s 才记） |

> 提醒：image2 真实并发会按米醋后台 pro 队列限流计费，跑 `--concurrency` ≥ 3 之前先确认账户额度。dry-run / 401 路径不计费。
