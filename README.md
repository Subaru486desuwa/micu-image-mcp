# 米醋画图 MCP

把 [米醋](https://www.openclaudecode.cn) 的 `gpt-image-2` / `gpt-image-2-pro` 代理包装成 MCP server，让 Claude Code / Codex / Cursor 等任意 MCP 客户端都能直接调起来生图、改图、批处理、多图融合。

> 米醋是一个 OpenAI 兼容代理，只暴露两个图像模型；本 MCP 把网页版里跑通的"size 自动选 model + 路由策略 + 重试限流"逻辑搬到了 server 端，让 LLM 一句话就能画图，不用关心模型该选哪个、走哪个端点。

---

## 功能一览

| Tool | 用途 | 典型场景 |
|---|---|---|
| `image_generate` | 文生图 | "画一张赛博朋克风格猫咪" |
| `image_edit` | 单图编辑（可选 alpha mask） | "把这张猫的背景换成日落沙滩" |
| `image_batch_edit` | N 进 N 出，每张独立同指令处理 | "把这 10 张产品图统一加水印" |
| `image_multi_reference` | N 进 1 出，综合多图风格画一张新图 | "结合这三张参考图画一张电影海报" |
| `server_info` | 诊断：当前路由规则、size 矩阵、能力说明 | 不知道怎么调时先看这个 |

### 自动路由（用户不用关心，server 自己选）

| 触发条件 | 自动行为 |
|---|---|
| `size` 边长 ≥ 1600 | 强制 `gpt-image-2-pro`（≥2K / 4K 必需） |
| `size` 边长 ≤ 1536 | 用 `gpt-image-2`（更快更便宜） |
| `image_edit` 在 ≥2K | 自动绕开 `/v1/images/edits`，走 generations + `reference_image`（米醋 `/v1/images/edits` 会压回 1.57MP） |
| `image_batch_edit` non-pro | 5 并发 |
| `image_batch_edit` pro | 串行 + 1.5s gap（米醋对 pro 并发会拒） |
| 任何模型 5xx | 自动重试 4s / 8s 两次 |
| `size` 留空 | 从 prompt 关键字推断（4K/竖屏/海报…），失败 fallback `1024x1024` |

### 米醋 size 约束（server 已强校验，违反直接拒）

- 宽高都必须是 **8 的倍数**，范围 `[256, 4096]`。
- ≤ 2.25MP 的请求会被代理压到 ~1.57MP "福利档"（实测会出 1254×1254）。
- ≥ 4MP 才能拿到真分辨率（如 `2048×2048` / `3840×2160`）。
- ≥ 2K 强制 `n=1`（米醋限流）。

---

## 安装

```bash
# 1. 克隆到任意目录（下文统称 <MCP_ROOT>）
git clone https://github.com/<owner>/micu-image-mcp.git
cd micu-image-mcp

# 2a. 推荐：独立 conda env
conda create -n micu-mcp python=3.12 -y
conda activate micu-mcp
pip install -e .

# 2b. 或直接用现有环境
pip install "mcp[cli]>=1.0.0" "httpx>=0.27.0"
```

> Windows 用户：上述命令在 PowerShell / cmd / WSL 都能跑，记得把 `<MCP_ROOT>` 替换成你 clone 的实际目录（如 `C:\Users\<you>\Developer\micu-image-mcp`）。

---

## 配置

把 `.env.example` 复制成你客户端配置里的 env 字段就行。需要的变量：

| 变量 | 必填 | 说明 |
|---|---|---|
| `MICU_API_KEY` | ✅ | 米醋 key（`sk-...`） |
| `MICU_BASEURL` | ❌ | 默认 `https://www.openclaudecode.cn` |
| `MICU_MODEL` | ❌ | 默认模型，server 会按 size 覆盖；通常不用动 |
| `MICU_SAVE_DIR` | ❌ | 输出目录；不填用 `~/Pictures/micu-out` |
| `MICU_SAVE_DIR_ROOT` | ❌ | 沙箱根目录，所有 `save_dir` 都被锁在这下面（防路径逃逸） |

> 下文用 `<MCP_ROOT>` 代表你 clone 这个仓库的本地绝对路径，`<SAVE_DIR>` 代表你想存图的目录。
> - **macOS / Linux** 例：`<MCP_ROOT>` = `~/Developer/micu-image-mcp`、`<SAVE_DIR>` = `~/Pictures/micu-out`
> - **Windows** 例：`<MCP_ROOT>` = `C:\\Users\\<you>\\Developer\\micu-image-mcp`、`<SAVE_DIR>` = `C:\\Users\\<you>\\Pictures\\micu-out`
>   （JSON / TOML 里 Windows 路径要写两遍反斜杠或用正斜杠 `C:/Users/...`）

### Claude Code

编辑 `~/.claude.json`（或项目根 `.mcp.json`）：

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "python",
      "args": ["<MCP_ROOT>/server.py"],
      "env": {
        "MICU_API_KEY": "sk-...",
        "MICU_SAVE_DIR": "<SAVE_DIR>"
      }
    }
  }
}
```

如果用了独立 conda env，把 `command` 换成对应 Python 解释器绝对路径：

| 平台 | 路径示例 |
|---|---|
| macOS (miniforge/Anaconda) | `~/miniforge3/envs/micu-mcp/bin/python` |
| Linux (Anaconda) | `~/anaconda3/envs/micu-mcp/bin/python` |
| Windows (Anaconda) | `C:\\Users\\<you>\\anaconda3\\envs\\micu-mcp\\python.exe` |

### Codex CLI

`~/.codex/config.toml`（Windows 在 `%USERPROFILE%\.codex\config.toml`）：

```toml
[mcp_servers.micu-image]
command = "python"
args = ["<MCP_ROOT>/server.py"]
env = { MICU_API_KEY = "sk-...", MICU_SAVE_DIR = "<SAVE_DIR>" }
```

### 任何其他 MCP 客户端

server 走标准 stdio MCP 协议，把上述 `command/args/env` 翻译到对应客户端格式即可。Windows 上如果 `python` 不在 PATH，把 `command` 换成 `python.exe` 绝对路径。

---

## 用法

直接对 LLM 说人话，不用记 tool 名：

```
画一张 1024x1024 的赛博朋克猫咪          → image_generate
画 4K 海报：东方美人，水墨风             → image_generate（自动锁 pro，size 推断 3840×2160）
把 ~/Pictures/cat.png 的背景换成海边     → image_edit
给这 10 张产品图统一加 "米醋" 水印        → image_batch_edit
结合这 3 张参考图，画一张同风格的全新场景 → image_multi_reference
```

弱 LLM（Codex / DeepSeek 等）调用前先 `server_info` 看路由矩阵，再决定 size 和 model。

---

## Tool 详细参数

> 所有 tool 都返回 `{ ok, model, saved: {path, ...}, notes, ... }`；失败时 `ok=False` + `errors=[...]`。详见每个 tool 的 docstring。

### `image_generate(prompt, size=None, n=1, model=None, save_dir=None, basename=None)`

文生图。`size` 留空让 server 从 prompt 推（"4K/海报" → 3840×2160；"竖屏/9:16" → 1024×1536…）；强 LLM 已知偏好直接传 size 更准。`n` ∈ [1, 10]，≥2K 强制 `n=1`。

### `image_edit(prompt, image_path, mask_path=None, size="1024x1024", ...)`

单图编辑。`mask_path` 是 PNG，alpha=0 的像素会被改，alpha=255 保持原样；mask 必须与原图同尺寸、含 alpha 通道（color_type ∈ {4, 6}）。`size` ≥2K 时 mask 自动忽略（走 generations 路径）。

### `image_batch_edit(prompt, image_paths, size="1024x1024", ...)`

N 进 N 出。每张图独立调一次 `image_edit`，结果合并返回。`image_paths` 长度 2–20 张；non-pro 5 并发，pro 串行。

### `image_multi_reference(prompt, image_paths, size="1024x1024", ...)`

N 进 1 出。把 2–10 张参考图一次性嵌进 chat/completions 上下文里，模型综合后画一张新图。**注意**：chat 路径不接受 `size` 字段，输出尺寸不可严格控制（实测多为 1024² 或 1254²）；要精确高分辨率请改用 `image_generate`。

#### 真 4K 多图融合两步法

代理 chat 端不支持 4K。要真 4K 多图融合，按这个流程：

1. `image_multi_reference(...)` 跑出一张 1K 综合图。
2. `image_edit(prompt="upscale and refine", image_path=<上一步的图>, size="3840x2160")` 升到 4K。

### `server_info()`

返回当前 baseurl / 默认 model / size 规则矩阵 / 能力矩阵 / 重试策略。**强烈建议弱 LLM 第一次调本 server 前先调一次。**

---

## 安全与边界

server 端硬校验，违规直接拒：

- `size`：格式 / 范围 / 8 倍数对齐
- `n`：必须 ∈ [1, 10]
- `basename`：只允许 `[A-Za-z0-9_\-.]+`，防路径遍历
- `save_dir`：必须在 `MICU_SAVE_DIR_ROOT` 下，不能逃逸
- 输入图：≤ 4MB，magic bytes + 实际尺寸双重校验（防截断 / 假扩展名）
- 总输入：≤ 8MB（米醋请求体上限）
- 响应：≤ 25MB（防代理塞炸响应）
- mask：PNG + 与原图同尺寸 + 含 alpha 通道
- API key：只从 env 读，不接受 tool 参数（防参数注入泄 key）

---

## 已知限制

- **`/v1/images/edits` 在米醋上 ≥2K 全 503/524**：server 已自动绕到 generations + `reference_image`，但代价是 **alpha mask 不可用**（chat/generations 路径都不认 mask）。
- **`image_multi_reference` 的 `size` 只是 prompt 提示**：chat 路径不认 size 字段，输出尺寸由模型自由发挥。
- **CF 120s 超时**：≥2K 单张图本身耗时接近上限；4K 多图融合一步生成基本不可能（用上面两步法）。
- **base64 不直接返回**：所有图都强制落盘返回路径，避免污染 LLM 上下文。

---

## 验证

```bash
# 跑起来看看（stdio 不会有输出，正常）
python server.py

# 用 mcp inspector
npx @modelcontextprotocol/inspector python server.py

# 在 Claude Code 里
@micu-image server_info
```

如果 `server_info` 返回 baseurl / 路由规则，配置就 OK 了。

---

## 故障排查

| 症状 | 排查 |
|---|---|
| `MICU_API_KEY 未配置` | 检查客户端 mcpServers 配置里 env.MICU_API_KEY |
| 4K 图返回 1254×1254 | 米醋 ≤ 2.25MP 强压 1.57MP 福利档；要真 4K 必须 ≥ 4MP |
| 5xx 一直重试失败 | 米醋 origin 不稳，等 1–2 分钟重试；多图融合 2K+ 概率性 500 |
| 路径校验失败 | `save_dir` 必须在 `MICU_SAVE_DIR_ROOT` 下；`basename` 不能含 `/` `..` |
| mask 被忽略 | size ≥ 2K 路径不支持 mask，降到 ≤ 1536 边长再试 |

---

## 许可

私人项目，无开源许可声明。如需引用 / 修改请先联系。
