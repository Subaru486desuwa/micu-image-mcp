# 米醋画图 MCP

把 [米醋](https://www.openclaudecode.cn) 的 `gpt-image-2` / `gpt-image-2-pro` 代理包装成 MCP server，让 Claude Code / Codex / Cursor 等任意 MCP 客户端都能直接调起来生图、改图、批处理、多图融合。

LLM 不用关心选模型 / 走哪个端点 / 重试限流——server 全自动。
---

## 功能

| Tool | 一句话 |
|---|---|
| `image_generate` | 文生图 |
| `image_edit` | 单图编辑（可选 alpha mask） |
| `image_batch_edit` | N 进 N 出，每张同指令独立处理 |
| `image_multi_reference` | N 进 1 出，综合多图风格画一张新图 |
| `server_info` | 路由规则 / size 矩阵诊断 |

每个 tool 的参数、约束、用法示例都在 `server.py` 的 docstring 里。LLM 第一次用本 server 之前，调一次 `server_info` 就能拿到当前路由策略和 size 约束矩阵。

---

## 一键安装（推荐）

```bash
git clone https://github.com/Subaru486desuwa/micu-image-mcp.git
cd micu-image-mcp
python install.py
```

脚本会：

1. 检查 Python ≥ 3.10
2. 自动 `pip install` 依赖
3. 交互问你 API key + 输出目录
4. 自动写入 `~/.claude.json` 和 `~/.codex/config.toml`（已有配置先备份再合并）
5. 启动 server 跑一次 initialize 握手验证

跑完重启 Claude Code / Codex 即可。让 LLM 说"调用 server_info"就能验证装好了。

### 选项

```bash
python install.py --no-codex      # 只写 Claude Code 配置
python install.py --no-claude     # 只写 Codex 配置
python install.py --yes           # 非交互（从 env 读）
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

## 卸载 / 重装

`install.py` 写之前会备份原 `~/.claude.json` 和 `~/.codex/config.toml`。要回滚：

```bash
ls ~/.claude.json.bak.*           # 看备份时间戳
cp ~/.claude.json.bak.YYYYMMDD_HHMMSS ~/.claude.json
```

要换 key / 输出目录，重跑 `python install.py` 即可（自动覆盖旧 micu-image 节，先备份）。

---

## 故障排查

| 症状 | 排查 |
|---|---|
| Claude Code 里看不到工具 | 重启客户端；`~/.claude.json` 里检查 `mcpServers.micu-image` 是否写入 |
| `MICU_API_KEY 未配置` | 重跑 `install.py`，或手动改配置文件里 env.MICU_API_KEY |
| 4K 图返回 1254×1254 | 米醋 ≤ 2.25MP 强压 1.57MP "福利档"；要真 4K 必须 ≥ 4MP（如 `2048×2048` / `3840×2160`） |
| 5xx 一直重试失败 | 米醋 origin 不稳，等 1–2 分钟重试；多图融合 2K+ 概率性 500 |
| Windows pip install 慢/失败 | 国内换源：`pip install -e . -i https://pypi.tuna.tsinghua.edu.cn/simple` |

更细的限制（CF 120s、mask 在 2K 不可用、image_multi_reference 输出尺寸不可控等）调一次 `server_info` 就有。

---

## 手动配置（不用 install.py）

只想自己改的话：

1. `pip install -e .`
2. 编辑 `~/.claude.json`（路径见 `.env.example`）：
   ```json
   {
     "mcpServers": {
       "micu-image": {
         "command": "python",
         "args": ["<你 clone 的绝对路径>/server.py"],
         "env": { "MICU_API_KEY": "sk-...", "MICU_SAVE_DIR": "..." }
       }
     }
   }
   ```
3. 重启 Claude Code

`server.py` 的 docstring 写了所有 tool 的输入输出契约，LLM 调一次就懂。

---

## 许可

如需引用 / 修改请先联系。
