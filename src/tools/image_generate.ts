import { z } from "zod";
import {
  API_KEY, DEFAULT_BASEURL, RETRYABLE_STATUS,
} from "../config.ts";
import {
  callWithRetry, errorDetail, extractImagePayload, parseResponse,
  type Endpoint,
} from "../http.ts";
import {
  inferSizeFromPrompt, parseSize, resolveModel, sizeNote, sizeTier,
} from "../routing.ts";
import { Semaphore } from "../lock.ts";
import {
  defaultBasename, resolveSaveDir, safeBasename, validateN, validateSize,
} from "../validation.ts";
import { saveImageB64, saveImageUrl, type SavedImage } from "../save.ts";

const getKey = (override: string | null | undefined): string => {
  const k = (override ?? "").trim() || API_KEY;
  if (!k) {
    throw new Error("未配置 API key。请设置 MICU_API_KEY 环境变量，或在调用时传 api_key 参数。");
  }
  return k;
};

export const imageGenerateInputSchema = {
  prompt: z.string().describe("图像描述。1-2000 字符。"),
  size: z.string().nullable().optional().describe(
    "'WxH' 字符串或 null。留空让 MCP 从 prompt 推断；强 LLM 已知偏好时显式传更准。" +
    "W/H 都必须是 8 的倍数。",
  ),
  n: z.number().int().optional().default(1).describe("张数 1-10。≥2K 强制 N=1。"),
  model: z.string().nullable().optional().describe(
    "显式指定模型，留空按 size 自动选。可选 'gpt-image-2' / 'gpt-image-2-pro'。",
  ),
  save_dir: z.string().nullable().optional().describe(
    "输出目录，必须在 MICU_SAVE_DIR_ROOT 之下。",
  ),
  basename: z.string().nullable().optional().describe(
    "文件名前缀（不带扩展名），仅允许 [A-Za-z0-9_-.]。",
  ),
  quality: z.enum(["low", "medium", "high", "auto"]).nullable().optional().describe(
    "渲染 quality（仅 gpt-image-2-pro 生效；non-pro 模型代理会忽略）。" +
    "low → 4K 也能稳过 60s 上游墙（实测 86s 完成）；high/auto → 大概率撞 524 触发 fallback。" +
    "留空走 origin default。",
  ),
  api_key: z.string().nullable().optional().describe(
    "覆盖 MICU_API_KEY 环境变量。一般留空。",
  ),
};

export type ImageGenerateArgs = {
  prompt: string;
  size?: string | null;
  n?: number;
  model?: string | null;
  save_dir?: string | null;
  basename?: string | null;
  quality?: "low" | "medium" | "high" | "auto" | null;
  api_key?: string | null;
};

export type ImageGenerateResult = {
  ok: boolean;
  error?: string;
  model?: string;
  size?: string;
  requested_n?: number;
  saved?: Array<{
    index: number;
    path: string;
    size_bytes: number;
    actual_size?: string;
    actual_megapixels?: number;
  }>;
  errors: string[];
  notes?: string[];
};

export const imageGenerate = async (
  raw: ImageGenerateArgs,
): Promise<ImageGenerateResult> => {
  let key: string;
  try { key = getKey(raw.api_key); }
  catch (e) { return { ok: false, error: (e as Error).message, errors: [(e as Error).message] }; }

  const baseurl = DEFAULT_BASEURL;

  if (typeof raw.prompt !== "string" || !raw.prompt.trim()) {
    return { ok: false, error: "prompt 不能为空", errors: ["prompt 不能为空"] };
  }
  let n = raw.n ?? 1;
  const errN = validateN(n);
  if (errN) return { ok: false, error: errN, errors: [errN] };

  const safeStem = raw.basename != null ? safeBasename(raw.basename) : null;
  if (raw.basename != null && !safeStem) {
    const msg = `basename ${JSON.stringify(raw.basename)} 含非法字符或路径分量；仅允许 [A-Za-z0-9_-.]，禁含 / 与 ..`;
    return { ok: false, error: msg, errors: [msg] };
  }
  const { dir: outDir, error: dirErr } = await resolveSaveDir(raw.save_dir);
  if (dirErr || !outDir) return { ok: false, error: dirErr!, errors: [dirErr!] };

  let size: string | null = raw.size ?? null;
  let inferredNote: string | null = null;
  if (size == null) {
    const guess = inferSizeFromPrompt(raw.prompt);
    if (guess) {
      size = guess[0];
      inferredNote = `size=None → 推断 ${guess[0]}（${guess[1]}）`;
    } else {
      size = "1024x1024";
      inferredNote = "size=None → 无关键字命中，用默认 1024x1024";
    }
  }
  const { cleaned, error: sizeErr } = validateSize(size, { allowNone: false });
  if (sizeErr || !cleaned) return { ok: false, error: sizeErr!, errors: [sizeErr!] };
  size = cleaned;

  const { model: effModel, notes } = resolveModel(raw.model ?? null, size);
  if (inferredNote) notes.unshift(inferredNote);
  const tier = sizeTier(size);
  if ((tier === "2k" || tier === "4k") && n > 1) {
    notes.push(`${tier.toUpperCase()} 强制 N=1，已忽略请求的 n=${n}`);
    n = 1;
  }
  const isPro = effModel.toLowerCase().includes("pro");
  const stem = safeStem ?? defaultBasename("gen");

  const epBody: Record<string, unknown> = {
    model: effModel,
    prompt: raw.prompt,
    n: 1,
    size,
    response_format: "b64_json",
  };
  if (raw.quality) {
    epBody.quality = raw.quality;
    if (!isPro) {
      notes.push(`quality=${raw.quality} 仅 gpt-image-2-pro 生效；当前模型 ${effModel} 代理会忽略`);
    } else {
      notes.push(`quality=${raw.quality}`);
    }
  }
  const ep: Endpoint = {
    url: `${baseurl}/v1/images/generations`,
    jsonBody: epBody,
  };

  const aggressiveRetry = isPro || tier === "2k" || tier === "4k";
  const canConcurrent = n > 1 && (tier === "small" || tier === "1k") && !isPro;
  const concurrency = canConcurrent ? 5 : 1;
  const bigSizeLock = tier === "2k" || tier === "4k";

  type AttemptResult = [number, SavedImage | null, string | null];
  const requestedSize = size;

  const doOne = async (idx: number): Promise<AttemptResult> => {
    let [status, text] = await callWithRetry({
      ep, key, retryPro: aggressiveRetry, stream: false,
      bigSizeLock, notesOut: notes,
    });

    // ≥2K 失败 → chat stream fallback
    if (!(status >= 200 && status < 300) && bigSizeLock && RETRYABLE_STATUS.includes(status)) {
      const origStatus = status;
      const origDetail = errorDetail(text);
      const chatEp: Endpoint = {
        url: `${baseurl}/v1/chat/completions`,
        jsonBody: {
          model: effModel,
          messages: [{ role: "user", content: raw.prompt }],
          size: requestedSize,
        },
      };
      const [cs, ct] = await callWithRetry({
        ep: chatEp, key, retryPro: isPro, stream: true,
      });
      if (cs >= 200 && cs < 300) {
        const fb = `generations 主路径 HTTP ${origStatus}（${origDetail}）→ fallback chat stream（size 不生效，实际输出 ~1.57MP）`;
        if (!notes.includes(fb)) notes.push(fb);
        status = cs; text = ct;
      }
    }

    if (!(status >= 200 && status < 300)) {
      return [idx, null, `#${idx + 1} HTTP ${status}: ${errorDetail(text)}`];
    }
    const resp = parseResponse(text);
    const [b64, url] = extractImagePayload(resp);
    try {
      let saved: SavedImage;
      if (b64) saved = await saveImageB64(b64, outDir, `${stem}_${idx + 1}`);
      else if (url) saved = await saveImageUrl(url, outDir, `${stem}_${idx + 1}`);
      else return [idx, null, `#${idx + 1} 响应里未找到图片`];
      return [idx, saved, null];
    } catch (e) {
      return [idx, null, `#${idx + 1} 保存失败: ${(e as Error).message}`];
    }
  };

  let results: AttemptResult[];
  if (concurrency > 1) {
    const sem = new Semaphore(concurrency);
    results = await Promise.all(
      Array.from({ length: n }, (_, i) => sem.run(() => doOne(i))),
    );
    notes.push(`1K + non-pro + N=${n} 已 ${concurrency} 并发`);
  } else {
    results = [];
    for (let i = 0; i < n; i++) results.push(await doOne(i));
  }

  results.sort((a, b) => a[0] - b[0]);
  const saved: NonNullable<ImageGenerateResult["saved"]> = [];
  const errors: string[] = [];
  for (const [idx, entry, err] of results) {
    if (entry) {
      const out: NonNullable<ImageGenerateResult["saved"]>[number] = {
        index: idx + 1,
        path: entry.filePath,
        size_bytes: entry.sizeBytes,
      };
      if (entry.actual) {
        out.actual_size = `${entry.actual[0]}x${entry.actual[1]}`;
        out.actual_megapixels = Math.round((entry.actual[0] * entry.actual[1]) / 1_000_000 * 100) / 100;
      }
      saved.push(out);
      const sn = sizeNote(size, entry.actual);
      if (sn && !notes.includes(sn)) notes.push(sn);
    }
    if (err) errors.push(err);
  }

  return {
    ok: saved.length > 0,
    model: effModel,
    size,
    requested_n: n,
    saved,
    errors,
    notes,
  };
};
