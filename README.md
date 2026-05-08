# 米醋画图 MCP

把 [米醋](https://www.openclaudecode.cn) 的 `gpt-image-2` / `gpt-image-2-pro` 代理包装成 MCP server，让 Claude Code / Codex / Cursor 等任意 MCP 客户端都能直接调起来生图、改图、批处理、多图融合。

LLM 不用关心选模型 / 走哪条端点 / 怎么重试 / 多图并发会不会把 origin 干爆 —— server 全自动。

---

## 功能

| Tool | 一句话 | 典型耗时 |
|---|---|---|
| `image_generate` | 文生图（1K / 2K / 4K） | 1K 30s · 2K 40-60s · 4K 50-80s |
| `image_edit` | 单图编辑（1K 支持 alpha mask；≥2K 走 generations + reference_image） | 1K 10s · 2K 50s · 4K 60-90s |
| `image_batch_edit` | N 进 N 出，每张同指令独立处理（仅 1K） | 1K non-pro 5 并发 · 1K pro 串行 |
| `image_multi_reference` | N 进 1 出，综合 2-10 张参考图画一张新图 | 1K 30-100s |
| `server_info` | 路由规则 / size 矩阵 / 重试策略 / 安全约束诊断 | — |

**LLM 第一次用本 server 之前，调一次 `server_info` 就能拿到完整路由策略。** 每个 tool 的参数、约束、用法示例都在 `server.py` 的 docstring 里。

---

## 一键安装（推荐）

```bash
git clone https://github.com/Subaru486desuwa/micu-image-mcp.git
cd micu-image-mcp
python install.py
```

脚本会：

1. 检查 Python ≥ 3.10
2. 自动 `pip install` 依赖（`mcp[cli]` + `httpx`）
3. 交互问你 API key + 输出目录
4. 自动写入 `~/.claude.json` 和 `~/.codex/config.toml`（已有配置先备份再合并）
5. 启动 server 跑一次 initialize 握手验证

跑完重启 Claude Code / Codex 即可。让 LLM 说"调用 server_info"就能验证装好了。

### 选项

```bash
python install.py --no-codex                 # 只写 Claude Code 配置
python install.py --no-claude                # 只写 Codex 配置
python install.py --mirror tsinghua          # pip 走清华镜像（国内加速）
python install.py --yes                      # 非交互（从 env 读）
MICU_API_KEY=sk-... MICU_SAVE_DIR=~/Pictures/micu-out python install.py --yes
```

### Windows

PowerShell / cmd / WSL 都行：

```powershell
git clone https://github.com/Subaru486desuwa/micu-image-mcp.git
cd micu-image-mcp
python install.py
```

如果 `python` 不在 PATH，用 `py -3 install.py` 或 `python.exe` 绝对路径。

---

## 用法

直接对 LLM 说人话：

```
画一张 1024x1024 的赛博朋克猫咪
画 4K 海报：东方美人，水墨风
把 ~/Pictures/cat.png 的背景换成海边
给这 10 张产品图统一加米醋水印
结合这 3 张参考图，画一张同风格的全新场景
```

LLM 自己选 tool / size / model；要看路由细节就调 `server_info`。

---

## Size 路由表（关键，必看）

米醋 origin 对 size 有两段截然不同的行为，server 全自动处理：

| 档位 | 触发条件 | 实际输出 | 模型 |
|---|---|---|---|
| **1K 福利档** | 总像素 ≤ 2.25 MP（如 1024² / 1280×720 / 1500² / 1920×1080） | 等比拉到 ~1.57 MP | gpt-image-2 |
| **2K 严格档** | 总像素 ≥ 4 MP 且 max edge ≥ 1600（如 2048² / 2048×1152） | 严格 1:1 输出 | 自动锁 gpt-image-2-pro |
| **4K 严格档** | 3840×2160 / 2160×3840 | 严格 1:1 输出 | 自动锁 gpt-image-2-pro |

**踩坑**：要 1080p 海报但写 `1920x1080` 会被压成 ~1.57 MP（约 1672×940），不是真 1080p；要真 1080p 必须 ≥ 2K 档（如 `2048×1152`）。

W 和 H 都必须是 **8 的整数倍**（米醋实测约束）。详见 `server_info().size_rules`。

---

## ≥2K 并发自动串行（重要）

米醋 origin 的 `gpt-image-2-pro` 在底层是**串行队列**，单张 4K 渲染 ~50-80s。客户端如果并发 N 张 4K 请求，origin 队列会堆成 N×60s，超过 Cloudflare 120s read timeout → CF 524 雪球。

**Server 内置进程级 `asyncio.Semaphore(1)`**：任意时刻只放一张 ≥2K 请求进 origin，其余 client 端透明排队。客户端可以放心并发，不用关心限流。

```
客户端  ──┬─→ 4K_h ─┐
         ├─→ 4K_v ─┤  Semaphore(1)  ──→ origin pro 队列（串行）
         └─→ 2K_sq┘
```

实测：1×4K横 + 1×4K竖 + 1×2K方 一次性并发，三张全成功，actual_size 严格 1:1，零 524。

1K 请求不走锁，可任意并发（`image_generate` N>1 自动 5 并发，`image_batch_edit` 同理）。

---

## 安全约束（默认开启）

`server.py` 自带轻量沙箱，防 key 外泄 / 任意文件外传 / burn quota：

- **`base_url` 锁定在启动期 env**，运行期 tool **不接受** `base_url` 参数（防 LLM 被注入指向攻击者 host 偷 key）
- **输出目录强制在 `MICU_SAVE_DIR_ROOT` 之下**（默认 `~/Pictures/micu-out`），传 root 之外路径直接拒
- **`basename` 仅允许 `[A-Za-z0-9_-.]`**，禁含 `/` 和 `..`（防路径穿越）
- **输入图按 magic bytes 校验**为 PNG/JPEG/WebP/GIF，单图 ≤ 4 MB，`image_multi_reference` 总和 ≤ 8 MB
- **`image_generate` 的 n ∈ [1, 10]**，超出立即拒（防 burn quota）
- **响应 ≤ 25 MB**，超过中断不落盘

---

## 环境变量

| 变量 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `MICU_API_KEY` | ✅ | — | 米醋后台拿 |
| `MICU_BASEURL` | ❌ | `https://www.openclaudecode.cn` | 改这个意味着改代理，慎用 |
| `MICU_MODEL` | ❌ | `gpt-image-2` | 默认模型，≥2K 自动切 pro |
| `MICU_SAVE_DIR` | ❌ | `~/Pictures/micu-out` | 实际落盘目录 |
| `MICU_SAVE_DIR_ROOT` | ❌ | 同 `MICU_SAVE_DIR` | 沙箱根，所有 `save_dir` 必须在此之下 |
| `MICU_USE_SHELL_PROXY` | ❌ | `0` | 设 `1` 让 httpx 拾取 shell 的 HTTPS_PROXY |

`install.py` 自动把 `MICU_SAVE_DIR_ROOT` 设成你选的输出目录，不会因沙箱拒掉自定义路径。

---

## 故障排查

| 症状 | 排查 |
|---|---|
| Claude Code 里看不到工具 | 重启客户端；检查 `~/.claude.json` 里 `mcpServers.micu-image` 是否写入 |
| `MICU_API_KEY 未配置` | 重跑 `install.py`，或手动改配置文件里 `env.MICU_API_KEY` |
| 4K 图返回 ~1254×1254 / 1672×940 | 命中 1.57 MP 福利档；要真 4K 必须 ≥ 4 MP（`2048²` / `3840×2160`） |
| ≥2K 多图并发出现 CF 524 | 已通过进程级 Semaphore(1) 自动串行，不应再出现；若仍命中，origin 真的挂了，等 1-2 分钟 |
| `image_multi_reference` ≥2K 间歇 500 | 米醋 origin `image_urls + ≥2K` 状态不稳；建议先 1K 出综合图，再用 `image_generate` 升 4K |
| Windows pip install 慢/失败 | 国内换源：`python install.py --mirror tsinghua` |
| 图生不出来还卡很久 | 米醋 origin 排队；server 自动 60s 重试一次（≥2K）/ 4s+8s 重试两次（1K） |

更细约束（CF 120s、mask 在 ≥2K 不可用、1K 福利档具体行为等）调一次 `server_info` 全有。

---

## 卸载 / 回滚

`install.py` 写之前会备份原 `~/.claude.json` 和 `~/.codex/config.toml`：

```bash
ls ~/.claude.json.bak.*                       # 看备份时间戳
cp ~/.claude.json.bak.YYYYMMDD_HHMMSS ~/.claude.json
```

要换 key / 输出目录，重跑 `python install.py` 即可（自动覆盖旧 micu-image 节，先备份）。

---

## 手动配置（不用 install.py）

只想自己改的话：

1. `pip install -e .`
2. 编辑 `~/.claude.json`：
   ```json
   {
     "mcpServers": {
       "micu-image": {
         "command": "python",
         "args": ["<你 clone 的绝对路径>/server.py"],
         "env": {
           "MICU_API_KEY": "sk-...",
           "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
           "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out"
         }
       }
     }
   }
   ```
3. 重启 Claude Code

`server.py` 的 docstring 写了所有 tool 的输入输出契约，LLM 调一次就懂。

---

## 许可

如需引用 / 修改请先联系。
