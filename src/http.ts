import { request, Agent, FormData as UndiciFormData } from "undici";
import { RETRYABLE_STATUS, TRUST_ENV_PROXY } from "./config.ts";
import { Semaphore, getBigSizeLock, withBigSizeFileLock } from "./lock.ts";

export type Endpoint = {
  url: string;
  jsonBody?: Record<string, unknown> | null;
  /** field -> File-like {name, bytes, mime} 或 string（普通 form 字段）。 */
  multipart?: Record<string, MultipartField | string> | null;
};

export type MultipartField = { filename: string; bytes: Uint8Array; mime: string };

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const jitter = (base: number) => base + Math.random() * 2_000;

// HTTP 客户端：每次 attempt 用一个独立的 undici.Agent，请求完关闭。
//   - 共享全局 Agent 在重试场景下会复用上次失败留在 keepalive 池里的"脏 socket" → 连环 ECONNRESET / other side closed。
//   - Per-attempt Agent 强制每次新 TCP/TLS，代价 ~200ms 握手，对分钟级生图忽略不计。
//   - 推荐 Node 运行时跑（Bun 对 undici.Agent 的 shim 不完整 —— close()/destroy() 缺失或方法签名不一致）。
//   - 行为对齐 Python httpx 与 curl。
const buildHeaders = (key: string, contentType: string | null): Record<string, string> => {
  const h: Record<string, string> = {
    Authorization: `Bearer ${key}`,
    Accept: "application/json",
  };
  if (contentType) h["Content-Type"] = contentType;
  return h;
};

const newAgent = (): Agent => new Agent({
  keepAliveTimeout: 1_000,
  keepAliveMaxTimeout: 1_000,
  pipelining: 1,
  connections: 1,
});

const closeAgent = async (a: Agent): Promise<void> => {
  try { await a.close(); } catch { /* idempotent */ }
};

const callJson = async (
  ep: Endpoint, key: string, timeoutMs: number,
): Promise<[number, string]> => {
  if (ep.multipart) {
    const fd = new UndiciFormData();
    for (const [k, v] of Object.entries(ep.multipart)) {
      if (typeof v === "string") {
        fd.append(k, v);
      } else {
        const arr = new Uint8Array(v.bytes);
        const blob = new Blob([arr], { type: v.mime });
        fd.append(k, blob, v.filename);
      }
    }
    const a1 = newAgent();
    try {
      const r = await request(ep.url, {
        method: "POST",
        headers: buildHeaders(key, null),
        body: fd,
        bodyTimeout: timeoutMs,
        headersTimeout: timeoutMs,
        dispatcher: a1,
      });
      return [r.statusCode, await r.body.text()];
    } finally {
      await closeAgent(a1);
    }
  }
  const agent = newAgent();
  try {
    const r = await request(ep.url, {
      method: "POST",
      headers: buildHeaders(key, "application/json"),
      body: JSON.stringify(ep.jsonBody ?? {}),
      bodyTimeout: timeoutMs,
      headersTimeout: timeoutMs,
      dispatcher: agent,
    });
    return [r.statusCode, await r.body.text()];
  } finally {
    await closeAgent(agent);
  }
};

/**
 * SSE stream 调用（chat/completions 专用）。把 delta.content 累加成完整 content，
 * 包装成与非 stream 等价的 chat completion JSON 返回。stream 让 CF 看到首字节就放行，
 * 不再撞 120s upstream timeout。
 */
const callStream = async (
  ep: Endpoint, key: string, timeoutMs: number,
): Promise<[number, string]> => {
  if (!ep.jsonBody || ep.multipart) return callJson(ep, key, timeoutMs);
  const reqBody = { ...ep.jsonBody, stream: true };
  const agent = newAgent();
  let r;
  try {
    r = await request(ep.url, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${key}`,
        Accept: "text/event-stream",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(reqBody),
      bodyTimeout: timeoutMs,
      headersTimeout: timeoutMs,
      dispatcher: agent,
    });
  } catch (e) {
    await closeAgent(agent);
    throw e;
  }
  if (!(r.statusCode >= 200 && r.statusCode < 300)) {
    const txt = await r.body.text();
    await closeAgent(agent);
    return [r.statusCode, txt];
  }

  const decoder = new TextDecoder("utf-8");
  let buf = "";
  let fullContent = "";
  let lastFinish: string | null = null;
  let lineCount = 0;

  outer: for await (const chunk of r.body as AsyncIterable<Uint8Array>) {
    buf += decoder.decode(chunk, { stream: true });
    let idx;
    while ((idx = buf.indexOf("\n")) >= 0) {
      const rawLine = buf.slice(0, idx).replace(/\r$/, "");
      buf = buf.slice(idx + 1);
      if (!rawLine) continue;
      lineCount++;
      const line = rawLine.trim();
      if (!line.startsWith("data:")) continue;
      const payload = line.slice(5).trim();
      if (payload === "[DONE]") break outer;
      let evt: any;
      try { evt = JSON.parse(payload); } catch { continue; }
      const choices = evt?.choices;
      if (Array.isArray(choices) && choices.length > 0) {
        const c0 = choices[0] ?? {};
        const delta = c0.delta ?? {};
        if (typeof delta.content === "string") fullContent += delta.content;
        if (c0.finish_reason) lastFinish = c0.finish_reason;
      }
      if (typeof evt?.delta === "string" && typeof evt?.type === "string" && evt.type.endsWith(".delta")) {
        fullContent += evt.delta;
      }
    }
  }
  const fake = {
    choices: [{
      message: { role: "assistant", content: fullContent },
      finish_reason: lastFinish ?? "stop",
    }],
    _stream_lines: lineCount,
  };
  await closeAgent(agent);
  return [r.statusCode || 200, JSON.stringify(fake)];
};

const DEBUG = process.env.MICU_DEBUG === "1";

const attempt = async (
  ep: Endpoint, key: string, stream: boolean, timeoutMs: number,
): Promise<[number, string]> => {
  try {
    return stream ? await callStream(ep, key, timeoutMs) : await callJson(ep, key, timeoutMs);
  } catch (e) {
    const err = e as Error & { code?: string; cause?: unknown };
    const detail = `${err.name}: ${err.message}` +
      (err.code ? ` [code=${err.code}]` : "") +
      (err.cause ? ` [cause=${String(err.cause).slice(0, 200)}]` : "");
    if (DEBUG) {
      process.stderr.write(`[micu http] ${ep.url} threw: ${detail}\n`);
    }
    return [0, detail];
  }
};

export const callWithRetry = async (opts: {
  ep: Endpoint;
  key: string;
  retryPro: boolean;
  stream?: boolean;
  bigSizeLock?: boolean;
  notesOut?: string[] | null;
  timeoutMs?: number;
}): Promise<[number, string]> => {
  const stream = opts.stream ?? false;
  const bigSizeLock = opts.bigSizeLock ?? false;
  const timeoutMs = opts.timeoutMs ?? 600_000;
  const notes = opts.notesOut ?? null;

  const run = async (): Promise<[number, string]> => {
    let [status, text] = await attempt(opts.ep, opts.key, stream, timeoutMs);
    // 网络层瞬抖：无条件 1 次免费重试
    if (status === 0) {
      await sleep(2_000);
      [status, text] = await attempt(opts.ep, opts.key, stream, timeoutMs);
    }
    const retryable = (s: number) => RETRYABLE_STATUS.includes(s);
    if (bigSizeLock) {
      // ≥2K：CF 524 fail fast（origin 持续慢，等也无用）；其他 5xx 抖动按指数退避多次重试
      // 实测 openclaudecode.cn 这类聚合站的 500「系统繁忙」常间歇连发 4-5 次才放过去，
      // 单次 60s 重试不够。4s/16s/60s 三次（含 jitter）总等待 ~80s，覆盖大多数瞬时拥塞。
      const delays = [4_000, 16_000, 60_000];
      for (const d of delays) {
        if (status >= 200 && status < 300) break;
        if (!opts.retryPro || !retryable(status) || status === 524) break;
        await sleep(jitter(d));
        [status, text] = await attempt(opts.ep, opts.key, stream, timeoutMs);
      }
    } else {
      if (!(status >= 200 && status < 300) && opts.retryPro && retryable(status)) {
        await sleep(jitter(4_000));
        [status, text] = await attempt(opts.ep, opts.key, stream, timeoutMs);
      }
      if (!(status >= 200 && status < 300) && opts.retryPro && retryable(status)) {
        await sleep(jitter(8_000));
        [status, text] = await attempt(opts.ep, opts.key, stream, timeoutMs);
      }
    }
    return [status, text];
  };

  if (bigSizeLock) {
    if (process.env.MICU_SKIP_FILE_LOCK === "1") {
      // 调试用：只走进程内 Semaphore，跳过 proper-lockfile 跨进程锁
      return getBigSizeLock().run(run);
    }
    return getBigSizeLock().run(() =>
      withBigSizeFileLock(notes, run),
    );
  }
  return run();
};

export const parseResponse = (text: string): unknown => {
  try { return JSON.parse(text); } catch { return text; }
};

export const errorDetail = (text: string): string => {
  try {
    const j = JSON.parse(text);
    if (j && typeof j === "object") {
      const err = (j as any).error;
      if (err && typeof err === "object" && err.message) return String(err.message).slice(0, 400);
      if ((j as any).message) return String((j as any).message).slice(0, 400);
    }
  } catch { /* ignore */ }
  return (text || "").slice(0, 400);
};

/** 从米醋响应里提取 (b64, url)；至少有一个，否则返回 [null, null]。 */
export const extractImagePayload = (
  resp: unknown,
): [string | null, string | null] => {
  if (typeof resp !== "object" || resp == null) return [null, null];
  const r = resp as Record<string, any>;
  // /v1/images/generations & /v1/images/edits 标准格式
  if (Array.isArray(r.data) && r.data.length > 0) {
    const item = r.data[0];
    if (item && typeof item === "object") {
      if (typeof item.b64_json === "string" && item.b64_json) return [item.b64_json, null];
      if (typeof item.url === "string" && item.url) return [null, item.url];
    }
  }
  // /v1/chat/completions fallback
  if (Array.isArray(r.choices) && r.choices.length > 0) {
    const msg = (r.choices[0]?.message ?? {}) as Record<string, any>;
    const content = msg.content;
    if (typeof content === "string") {
      const m1 = /!\[[^\]]*\]\((data:image\/[^;]+;base64,([A-Za-z0-9+/=\s]+))\)/.exec(content);
      if (m1) return [m1[2]!.trim(), null];
      const m2 = /!\[[^\]]*\]\((https?:\/\/[^)]+)\)/.exec(content);
      if (m2) return [null, m2[1]!];
      const m3 = /\b(https?:\/\/\S+\.(?:png|jpe?g|webp|gif))\b/i.exec(content);
      if (m3) return [null, m3[1]!];
    }
  }
  return [null, null];
};

export { Semaphore };
