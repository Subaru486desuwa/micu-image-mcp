import path from "node:path";
import { homedir } from "node:os";
import {
  MAX_INPUT_FILE_BYTES, MAX_N, MAX_SIZE_EDGE, MIN_SIZE_EDGE,
  SAVE_ROOT, DEFAULT_SAVE_DIR, SIZE_ALIGNMENT,
} from "./config.ts";

const SAFE_BASENAME_RE = /^[A-Za-z0-9_\-.]+$/;

export const validateSize = (
  size: string | null | undefined,
  opts: { allowNone?: boolean } = {},
): { cleaned: string | null; error: string | null } => {
  const allowNone = opts.allowNone ?? true;
  if (size == null) {
    return allowNone
      ? { cleaned: null, error: null }
      : { cleaned: null, error: "size 不能为 None（此 tool 必须传明确 size）" };
  }
  if (typeof size !== "string") {
    return { cleaned: null, error: `size 必须是字符串，收到 ${typeof size}` };
  }
  const s = size.trim().toLowerCase();
  const m = /^(\d+)x(\d+)$/.exec(s);
  if (!m) {
    return { cleaned: null, error: `size 格式错误：必须是 'WxH'（如 '1024x1024'），收到 ${JSON.stringify(size)}` };
  }
  const w = Number(m[1]); const h = Number(m[2]);
  if (w <= 0 || h <= 0) return { cleaned: null, error: `size W/H 必须为正数，收到 ${size}` };
  if (w < MIN_SIZE_EDGE || h < MIN_SIZE_EDGE) {
    return { cleaned: null, error: `size 边长太小（最小 ${MIN_SIZE_EDGE}），收到 ${size}` };
  }
  if (w > MAX_SIZE_EDGE || h > MAX_SIZE_EDGE) {
    return { cleaned: null, error: `size 边长太大（最大 ${MAX_SIZE_EDGE}），收到 ${size}` };
  }
  if (w % SIZE_ALIGNMENT !== 0 || h % SIZE_ALIGNMENT !== 0) {
    return { cleaned: null, error: `size W/H 必须是 ${SIZE_ALIGNMENT} 的倍数（米醋代理约束），收到 ${size}` };
  }
  return { cleaned: `${w}x${h}`, error: null };
};

export const validateN = (n: number): string | null => {
  if (typeof n !== "number" || !Number.isInteger(n)) {
    return `n 必须是整数，收到 ${typeof n}`;
  }
  if (n < 1) return `n 必须 ≥ 1，收到 ${n}`;
  if (n > MAX_N) return `n 必须 ≤ ${MAX_N}，收到 ${n}（防止意外 burn quota）`;
  return null;
};

export const safeBasename = (name: string | null | undefined): string | null => {
  if (name == null) return null;
  if (typeof name !== "string" || !name.trim()) return null;
  const only = path.basename(name);
  if (only !== name) return null;
  if (only.includes("..") || only.startsWith(".")) return null;
  if (!SAFE_BASENAME_RE.test(only)) return null;
  if (only.length > 100) return null;
  return only;
};

const expandHome = (p: string): string =>
  p.startsWith("~") ? path.join(homedir(), p.slice(1)) : p;

const isWithin = (parent: string, child: string): boolean => {
  const rel = path.relative(parent, child);
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
};

import { mkdir } from "node:fs/promises";

export const resolveSaveDir = async (
  saveDir: string | null | undefined,
): Promise<{ dir: string | null; error: string | null }> => {
  try {
    await mkdir(SAVE_ROOT, { recursive: true });
  } catch (e) {
    return { dir: null, error: `无法创建 save root ${SAVE_ROOT}: ${(e as Error).message}` };
  }
  if (saveDir == null) {
    const def = path.resolve(expandHome(DEFAULT_SAVE_DIR));
    return { dir: isWithin(SAVE_ROOT, def) ? def : SAVE_ROOT, error: null };
  }
  const resolved = path.resolve(expandHome(saveDir));
  if (!isWithin(SAVE_ROOT, resolved)) {
    return {
      dir: null,
      error: `save_dir 必须在安全根目录 ${SAVE_ROOT} 之下；收到 ${JSON.stringify(saveDir)}。` +
        `留空让 MCP 用默认目录，或先把 MICU_SAVE_DIR_ROOT 改到你想要的位置。`,
    };
  }
  return { dir: resolved, error: null };
};

export type ImageMime = "image/png" | "image/jpeg" | "image/webp" | "image/gif";

export const detectImageMime = (raw: Uint8Array): ImageMime | null => {
  if (raw.length < 16) return null;
  // PNG
  if (raw[0] === 0x89 && raw[1] === 0x50 && raw[2] === 0x4e && raw[3] === 0x47 &&
      raw[4] === 0x0d && raw[5] === 0x0a && raw[6] === 0x1a && raw[7] === 0x0a) return "image/png";
  // JPEG
  if (raw[0] === 0xff && raw[1] === 0xd8 && raw[2] === 0xff) return "image/jpeg";
  // WebP
  if (raw[0] === 0x52 && raw[1] === 0x49 && raw[2] === 0x46 && raw[3] === 0x46 &&
      raw.length >= 12 &&
      raw[8] === 0x57 && raw[9] === 0x45 && raw[10] === 0x42 && raw[11] === 0x50) return "image/webp";
  // GIF
  if (raw[0] === 0x47 && raw[1] === 0x49 && raw[2] === 0x46 && raw[3] === 0x38 &&
      (raw[4] === 0x37 || raw[4] === 0x39) && raw[5] === 0x61) return "image/gif";
  return null;
};

export const validateImageBytes = (raw: Uint8Array, label = "image"): string | null => {
  if (!raw || raw.length < 16) {
    return `${label} 太小（${raw?.length ?? 0} 字节），不像合法图片`;
  }
  if (detectImageMime(raw) == null) {
    const head = Array.from(raw.slice(0, 16)).map((b) => b.toString(16).padStart(2, "0")).join("");
    return `${label} 不是 PNG/JPEG/WebP/GIF（前 16 字节 hex: ${head}）`;
  }
  return null;
};

/** PNG IHDR.color_type @ offset 25. 4=GA, 6=RGBA (含 alpha)。 */
export const pngColorType = (raw: Uint8Array): number | null => {
  if (raw.length < 26) return null;
  if (!(raw[0] === 0x89 && raw[1] === 0x50 && raw[2] === 0x4e && raw[3] === 0x47)) return null;
  return raw[25] ?? null;
};

/** 读 PNG / JPEG / WebP 实际像素尺寸，不依赖第三方库。 */
export const detectActualSize = (raw: Uint8Array): [number, number] | null => {
  if (raw.length < 24) return null;
  const dv = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  // PNG
  if (raw[0] === 0x89 && raw[1] === 0x50 && raw[2] === 0x4e && raw[3] === 0x47 &&
      raw[12] === 0x49 && raw[13] === 0x48 && raw[14] === 0x44 && raw[15] === 0x52) {
    return [dv.getUint32(16, false), dv.getUint32(20, false)];
  }
  // JPEG: scan SOFn
  if (raw[0] === 0xff && raw[1] === 0xd8 && raw[2] === 0xff) {
    let i = 2;
    while (i < raw.length - 9) {
      if (raw[i] !== 0xff) { i++; continue; }
      const marker = raw[i + 1] ?? 0;
      i += 2;
      if (marker === 0xd8 || marker === 0xd9) continue;
      if (marker >= 0xd0 && marker <= 0xd7) continue;
      const segLen = dv.getUint16(i, false);
      const sof = [0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf];
      if (sof.includes(marker)) {
        const h = dv.getUint16(i + 3, false);
        const w = dv.getUint16(i + 5, false);
        return [w, h];
      }
      i += segLen;
    }
    return null;
  }
  // WebP
  if (raw[0] === 0x52 && raw[1] === 0x49 && raw[2] === 0x46 && raw[3] === 0x46 &&
      raw[8] === 0x57 && raw[9] === 0x45 && raw[10] === 0x42 && raw[11] === 0x50) {
    const c0 = raw[12], c1 = raw[13], c2 = raw[14], c3 = raw[15];
    // VP8 (lossy)
    if (c0 === 0x56 && c1 === 0x50 && c2 === 0x38 && c3 === 0x20) {
      const w = (dv.getUint16(26, true) & 0x3fff);
      const h = (dv.getUint16(28, true) & 0x3fff);
      return [w, h];
    }
    // VP8L (lossless)
    if (c0 === 0x56 && c1 === 0x50 && c2 === 0x38 && c3 === 0x4c) {
      const b1 = raw[21] ?? 0, b2 = raw[22] ?? 0, b3 = raw[23] ?? 0, b4 = raw[24] ?? 0;
      const w = (((b2 & 0x3f) << 8) | b1) + 1;
      const h = (((b4 & 0x0f) << 10) | (b3 << 2) | ((b2 & 0xc0) >> 6)) + 1;
      return [w, h];
    }
    // VP8X (extended)
    if (c0 === 0x56 && c1 === 0x50 && c2 === 0x38 && c3 === 0x58) {
      const w = (raw[24]! | (raw[25]! << 8) | (raw[26]! << 16)) + 1;
      const h = (raw[27]! | (raw[28]! << 8) | (raw[29]! << 16)) + 1;
      return [w, h];
    }
  }
  return null;
};

import { readFile, stat } from "node:fs/promises";

export const validateImagePath = async (
  imagePath: string,
  label = "image_path",
): Promise<{ filePath: string; bytes: Uint8Array; mime: ImageMime | ""; error: string | null }> => {
  const filePath = path.resolve(expandHome(imagePath));
  let st;
  try {
    st = await stat(filePath);
  } catch (e) {
    return { filePath, bytes: new Uint8Array(0), mime: "", error: `${label} 不存在或无法 stat: ${filePath}` };
  }
  if (!st.isFile()) {
    return { filePath, bytes: new Uint8Array(0), mime: "", error: `${label} 不是文件: ${filePath}` };
  }
  if (st.size > MAX_INPUT_FILE_BYTES) {
    return {
      filePath, bytes: new Uint8Array(0), mime: "",
      error: `${label} 文件 ${(st.size / 1024 / 1024).toFixed(1)}MB 超过单文件上限 ` +
        `${(MAX_INPUT_FILE_BYTES / 1024 / 1024).toFixed(0)}MB；请先压缩`,
    };
  }
  let raw: Uint8Array;
  try {
    raw = await readFile(filePath);
  } catch (e) {
    return { filePath, bytes: new Uint8Array(0), mime: "", error: `${label} 读取失败: ${(e as Error).message}` };
  }
  const errBytes = validateImageBytes(raw, label);
  if (errBytes) return { filePath, bytes: raw, mime: "", error: errBytes };
  const actual = detectActualSize(raw);
  if (!actual) {
    return { filePath, bytes: raw, mime: "", error: `${label} 头部像图片，但解析不出宽高（可能截断、损坏或伪造）` };
  }
  if (actual[0] < 16 || actual[1] < 16) {
    return { filePath, bytes: raw, mime: "", error: `${label} 尺寸 ${actual[0]}x${actual[1]} 太小，不像正常图片` };
  }
  const mime = detectImageMime(raw);
  return { filePath, bytes: raw, mime: mime ?? "image/png", error: null };
};

export const validateMaskAgainstImage = (
  maskRaw: Uint8Array,
  imageSize: [number, number],
): string | null => {
  if (!(maskRaw[0] === 0x89 && maskRaw[1] === 0x50 && maskRaw[2] === 0x4e && maskRaw[3] === 0x47)) {
    return "mask_path 必须是 PNG（OpenAI 规范要求 alpha 通道）";
  }
  const ms = detectActualSize(maskRaw);
  if (!ms) return "mask PNG 头损坏，解析不出尺寸";
  if (ms[0] !== imageSize[0] || ms[1] !== imageSize[1]) {
    return `mask 尺寸 ${ms[0]}x${ms[1]} 必须与原图 ${imageSize[0]}x${imageSize[1]} 一致`;
  }
  const ct = pngColorType(maskRaw);
  if (ct !== 4 && ct !== 6) {
    const desc: Record<number, string> = { 0: "灰度", 2: "RGB", 3: "调色板" };
    const d = ct != null && ct in desc ? desc[ct] : `未知 (${ct})`;
    return `mask PNG color_type=${ct}（${d}），缺 alpha 通道；必须用 GA(4) 或 RGBA(6) 格式，alpha=0 标记编辑区`;
  }
  return null;
};

export const defaultBasename = (prefix: string): string =>
  `${prefix}_${process.hrtime.bigint()}`;
