import { mkdir, writeFile, access } from "node:fs/promises";
import path from "node:path";
import { request } from "undici";
import {
  detectActualSize, detectImageMime, validateImageBytes,
} from "./validation.ts";
import { MAX_RESPONSE_BYTES } from "./config.ts";

export class ImageSaveError extends Error {
  override readonly name = "ImageSaveError";
}

const extFromBytes = (raw: Uint8Array): string => {
  const m = detectImageMime(raw);
  switch (m) {
    case "image/png": return "png";
    case "image/jpeg": return "jpg";
    case "image/gif": return "gif";
    case "image/webp": return "webp";
    default: return "png";
  }
};

const exists = async (p: string): Promise<boolean> => {
  try { await access(p); return true; } catch { return false; }
};

export type SavedImage = {
  filePath: string;
  actual: [number, number] | null;
  sizeBytes: number;
};

export const saveValidatedBytes = async (
  raw: Uint8Array,
  saveDir: string,
  basename: string,
  sourceLabel: string,
): Promise<SavedImage> => {
  if (raw.length > MAX_RESPONSE_BYTES) {
    throw new ImageSaveError(
      `${sourceLabel} 响应 ${(raw.length / 1024 / 1024).toFixed(1)}MB 超过单图上限 ` +
      `${(MAX_RESPONSE_BYTES / 1024 / 1024).toFixed(0)}MB；可能是代理返回了错误内容`,
    );
  }
  const err = validateImageBytes(raw, sourceLabel);
  if (err) throw new ImageSaveError(err);
  const ext = extFromBytes(raw);
  await mkdir(saveDir, { recursive: true });

  let candidate = path.join(saveDir, `${basename}.${ext}`);
  let counter = 2;
  while (await exists(candidate)) {
    candidate = path.join(saveDir, `${basename}_${counter}.${ext}`);
    counter++;
    if (counter > 1000) throw new ImageSaveError(`basename 冲突过多：${basename}`);
  }
  // 路径越界二次确认
  const resolvedDir = path.resolve(saveDir);
  const resolvedPath = path.resolve(candidate);
  const rel = path.relative(resolvedDir, resolvedPath);
  if (rel.startsWith("..") || path.isAbsolute(rel)) {
    throw new ImageSaveError(`落盘路径越界: ${candidate}`);
  }
  await writeFile(candidate, raw);
  return { filePath: candidate, actual: detectActualSize(raw), sizeBytes: raw.length };
};

export const saveImageB64 = async (
  b64: string, saveDir: string, basename: string,
): Promise<SavedImage> => {
  let raw: Uint8Array;
  try {
    raw = Uint8Array.from(Buffer.from(b64, "base64"));
  } catch (e) {
    throw new ImageSaveError(`base64 解码失败: ${(e as Error).message}`);
  }
  return saveValidatedBytes(raw, saveDir, basename, "b64 响应");
};

export const saveImageUrl = async (
  url: string, saveDir: string, basename: string,
): Promise<SavedImage> => {
  const r = await request(url, {
    method: "GET",
    bodyTimeout: 120_000,
    headersTimeout: 120_000,
  });
  if (!(r.statusCode >= 200 && r.statusCode < 300)) {
    throw new ImageSaveError(`远端图 HTTP ${r.statusCode}: ${url.slice(0, 80)}`);
  }
  const clRaw = r.headers["content-length"];
  const cl = Array.isArray(clRaw) ? clRaw[0] : clRaw;
  if (cl && /^\d+$/.test(String(cl)) && Number(cl) > MAX_RESPONSE_BYTES) {
    throw new ImageSaveError(
      `远端图 Content-Length=${(Number(cl) / 1024 / 1024).toFixed(1)}MB 超过 ` +
      `${(MAX_RESPONSE_BYTES / 1024 / 1024).toFixed(0)}MB 上限`,
    );
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  for await (const chunk of r.body as AsyncIterable<Uint8Array>) {
    if (!chunk) continue;
    total += chunk.byteLength;
    if (total > MAX_RESPONSE_BYTES) {
      throw new ImageSaveError(
        `远端图实际下载 >${(MAX_RESPONSE_BYTES / 1024 / 1024).toFixed(0)}MB，已中断`,
      );
    }
    chunks.push(chunk);
  }
  const raw = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) { raw.set(c, off); off += c.byteLength; }
  return saveValidatedBytes(raw, saveDir, basename, `远端图 ${url.slice(0, 80)}`);
};
