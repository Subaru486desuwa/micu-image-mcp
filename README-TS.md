# 米醋画图 MCP — TypeScript Port (`ts-port` branch)

把 `main` 分支上 1831 行的 Python 实现迁移到 TypeScript。当前是**首版骨架 + 一个完整 tool 端到端验证**的状态，不是替代品，是另一条候选路线。

> 想要直接能用、所有 tool 都通的版本，请回 `main` 分支用 Python。

---

## 状态

| Tool | 移植状态 | 备注 |
|------|---------|------|
| `image_generate` | ✅ 完整端到端 | 1K 实测稳定；2K/4K 见下「已知限制」 |
| `server_info` | ✅ | 标了 `port_status` 说明哪些 tool 还没移 |
| `image_edit` | ❌ 未移 | 看 `server.py` line 1134 |
| `image_batch_edit` | ❌ 未移 | 看 `server.py` line 1375 |
| `image_multi_reference` | ❌ 未移 | 看 `server.py` line 1507 |

架构（路由、重试、双层锁、validation、save、actual_size 解析）全部对齐 Python 版，加新 tool 复用现有模块。

---

## 必须用 Node + tsx，不要用 Bun

**Bun 会静默 shim 大部分 undici / fetch API，但 shim 不完整或行为不一致**：

| 用法 | 在 Bun 下的表现 |
|------|----------------|
| `globalThis.fetch` | HTTP/2 默认开。聚合站慢响应被对端 RST_STREAM → ECONNRESET。|
| `import { fetch } from "undici"` | 重定向到 Bun 的 native fetch（同上）。|
| `import { Client } from "undici"` | 实例化 OK，但 `.request()` 返回 `undefined`，`.close()` 缺失。|
| `import { request } from "undici"` | JS 函数是真 undici，但底层 socket 仍走 Bun，长响应不稳。|
| `import { Agent } from "undici"` | 部分方法签名不一致。|

实测：相同代码 `bun run` 报"socket closed unexpectedly"，`tsx`（Node）报真实的 undici `UND_ERR_SOCKET`，Node 路径才能用真 undici keepalive 池。

`package.json` 已把 `start` 设为 `tsx`。Bun 路径保留为 `start:bun` 仅供调试参考。

---

## 安装与运行

```bash
# 已有 Node 20+ 即可。tsx 是本地 devDep。
bun install        # 或 npm install / pnpm install — 只装依赖，不影响运行
npm run start      # = tsx src/index.ts，stdio MCP server
npm run typecheck  # = tsc --noEmit
```

要让 Claude Code / Codex / Cursor 用上：把 MCP 客户端配置里的 command 指向 `tsx <repo>/src/index.ts`，env 变量参考下面。

---

## 环境变量（与 Python 版完全一致）

| 变量 | 默认值 | 说明 |
|------|-------|------|
| `MICU_API_KEY` | （空，会拒） | 米醋 API key |
| `MICU_BASEURL` | `https://www.micuapi.ai` | 上游 baseurl |
| `MICU_MODEL` | `gpt-image-2` | 默认模型，可在 tool 调用时覆盖 |
| `MICU_SAVE_DIR_ROOT` | `~/Pictures/micu-out` | 输出安全根目录，所有写入路径必须在它之下 |
| `MICU_SAVE_DIR` | = `MICU_SAVE_DIR_ROOT` | 默认输出目录 |
| `MICU_USE_SHELL_PROXY` | `0` | 是否让 fetch 拾取 `HTTPS_PROXY` 等 shell 代理 env |
| `MICU_DEBUG` | `0` | `=1` 时把 fetch 异常打到 stderr，方便排查 |
| `MICU_SKIP_FILE_LOCK` | `0` | `=1` 跳过跨进程 `proper-lockfile`，仅用进程内 Semaphore（调试用） |

---

## 项目结构

```
src/
  index.ts                MCP stdio server 入口
  config.ts               所有 env / 常量
  routing.ts              size 解析、tier 分档、model 选择、prompt 关键字推断
  validation.ts           size/n/basename/save_dir/image-bytes/mask 校验 + magic-bytes 尺寸读取
  http.ts                 undici.request 封装、SSE stream、双层锁包裹的重试
  lock.ts                 进程内 Semaphore + proper-lockfile 跨进程锁
  save.ts                 b64/url 落盘 + 路径越界二次确认
  tools/
    image_generate.ts     完整移植
    server_info.ts        诊断 / 能力查询
```

---

## 已知限制：60 秒上游 timeout 物理墙

`micuapi.ai`、`openclaudecode.cn`、`e-flowcode.cc` 这一系聚合站（疑似共用 Cloudflare 或同一国内云前置）都有约 **60 秒上游 timeout**：

| 档位 | 渲染耗时 | 命中 60s？ | 实测结果 |
|------|---------|------------|----------|
| 1K | ~30s | 通常不会 | ✅ 稳定 |
| 2K | 30-115s | 边缘 | ~50% 抖动 |
| 4K | 60-100s | 几乎必中 | ❌ 客户端看到 "other side closed" / "socket closed abruptly" |

服务器侧对 4K 经常超过 60s，代理就把连接切了，**任何客户端**（curl / undici / Bun fetch）都没法绕。

MCP 在 ≥2K 失败时**自动 fallback 到 chat stream**（流式让代理看到首字节就放行），代价是 chat 路径下 size 不生效，输出固定 ~1.57MP。`notes` 字段会透明告知发生了 fallback。

如果你的 baseurl 没有这个 60s 限制（自部署、企业代理、跑深夜错峰），4K 一般 50-80s 能完成，那就没这个问题。

---

## 加新 tool 怎么做

参考 `src/tools/image_generate.ts`：

1. 用 `zod` 写 inputSchema，复用 `validateSize` / `validateN` / `safeBasename` / `resolveSaveDir`
2. 路由用 `resolveModel(model, size)` 自动选 pro / non-pro
3. 网络用 `callWithRetry({ ep, key, retryPro, stream, bigSizeLock, notesOut })` —— `bigSizeLock` 在 ≥2K 自动开，跨进程串行
4. 落盘用 `saveImageB64` / `saveImageUrl`
5. 把 saved 列表回填 + 用 `sizeNote(requested, actual)` 检测代理自动放大/压缩
6. 在 `index.ts` 注册 tool

`src/tools/server_info.ts` 的 `port_status` 字段也记得同步更新。

---

## 为什么不直接基于 main 的 Python？

1. 有人想要单二进制分发（Bun `--compile` / Node `pkg`）
2. TS 的 MCP SDK 比 Python 更新最快、tool calling 协议特性最先到位
3. 部分团队想统一前后端到 TS

这一版到 1K 稳定可用、其他 tool 待补，等真有分发需求再继续推。
