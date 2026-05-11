import {
  DEFAULT_MODEL, HIGH_RES_EDGE, PRO_MODEL, SIZE_ALIGNMENT,
} from "./config.ts";

export type SizeTier = "unknown" | "small" | "1k" | "2k" | "4k";

export const parseSize = (size: string): [number, number] | null => {
  const m = /^(\d+)x(\d+)$/.exec(size.trim().toLowerCase());
  return m ? [Number(m[1]), Number(m[2])] : null;
};

export const maxEdge = (size: string): number => {
  const p = parseSize(size);
  return p ? Math.max(p[0], p[1]) : 0;
};

export const sizeTier = (size: string): SizeTier => {
  const e = maxEdge(size);
  if (e === 0) return "unknown";
  if (e < 1024) return "small";
  if (e < 1600) return "1k";
  if (e < 3000) return "2k";
  return "4k";
};

export const reject4kWithReference = (size: string, tool: string): string | null => {
  if (sizeTier(size) !== "4k") return null;
  return (
    `size=${size} (4K) 在 ${tool} 已禁用：origin 处理 4K + 参考图稳定 > 120s，` +
    `撞 Cloudflare Proxy Read Timeout 物理上限。请改用 2K：` +
    `横屏 "2048x1152" / 竖屏 "1152x2048" / 方形 "2048x2048"。` +
    `若必须 4K，可两步法：先 1K/2K 出综合图 → 再用 image_generate(size="3840x2160") ` +
    `描述同场景升 4K（人物 ID 不保证一致）。`
  );
};

export const resolveModel = (
  requested: string | null, size: string,
): { model: string; notes: string[] } => {
  const notes: string[] = [];
  const tier = sizeTier(size);
  let model = requested || DEFAULT_MODEL;
  if ((tier === "2k" || tier === "4k") && !model.toLowerCase().includes("pro")) {
    notes.push(`size=${size} (${tier}) 仅 pro 支持，已自动切到 ${PRO_MODEL}`);
    model = PRO_MODEL;
  }
  return { model, notes };
};

export const bypassEdits = (model: string, size: string): boolean =>
  model.toLowerCase().includes("pro") && maxEdge(size) >= HIGH_RES_EDGE;

const roundToAlignment = (n: number): number =>
  Math.max(16, Math.round(n / 8) * 8);

/** 从 prompt 关键字推断 size。返回 [size, reason] 或 null。 */
export const inferSizeFromPrompt = (prompt: string): [string, string] | null => {
  const p = prompt.toLowerCase();

  // 1) 明确像素 "1920x1080" / "1920×1080"
  const m = /(\d{3,4})\s*[x×]\s*(\d{3,4})/.exec(p);
  if (m) {
    const w = Number(m[1]), h = Number(m[2]);
    const w16 = roundToAlignment(w), h16 = roundToAlignment(h);
    if (w16 !== w || h16 !== h) {
      return [`${w16}x${h16}`, `prompt 含像素 ${w}x${h}，对齐 ${SIZE_ALIGNMENT} 倍数为 ${w16}x${h16}`];
    }
    return [`${w16}x${h16}`, `prompt 含明确像素 ${w}x${h}`];
  }

  const verticalKw = ["9:16", "竖屏", "竖版", "vertical", "portrait", "phone wallpaper",
    "tiktok", "reels", "stories", "手机壁纸"];
  const horizontalKw = ["16:9", "横屏", "横版", "landscape", "widescreen", "desktop wallpaper",
    "wallpaper", "壁纸", "banner", "封面", "cover"];
  const squareKw = ["正方形", "square", "avatar", "头像", "icon", "logo", "profile pic",
    "头像图", "图标"];
  const posterKw = ["poster", "海报", "2:3", "movie poster"];
  const photo32Kw = ["3:2", "photograph", "照片"];

  const has = (arr: string[]) => arr.some((k) => p.includes(k));
  const isVert = has(verticalKw);
  const isHoriz = has(horizontalKw);
  const isSquare = has(squareKw);
  const isPoster = has(posterKw);
  const isPhoto32 = has(photo32Kw);

  if (/\b4k\b|uhd|ultra[\s-]?hd|超高清/.test(p)) {
    return isVert
      ? ["2160x3840", "prompt 含 4K 关键字 + 竖屏"]
      : ["3840x2160", "prompt 含 4K 关键字（默认横屏）"];
  }
  if (/\b2k\b|1080p|full[\s-]?hd|\bfhd\b/.test(p)) {
    // 不选 1920×1080 / 1080×1920：≤2.25MP 会被 origin 压到 ~1.57MP；2048×1152 跨 2.25MP 阈值拿到真分辨率
    return isVert
      ? ["1152x2048", "prompt 含 2K/1080p 关键字 + 竖屏（用 1152×2048 跨 2.25MP 阈值，避开福利档降级）"]
      : ["2048x1152", "prompt 含 2K/1080p 关键字（默认横屏；用 2048×1152 跨 2.25MP 阈值，避开福利档降级）"];
  }
  if (/720p|\bhd\b/.test(p)) {
    return isVert
      ? ["720x1280", "prompt 含 720p 关键字 + 竖屏"]
      : ["1280x720", "prompt 含 720p 关键字"];
  }

  if (isSquare) return ["1024x1024", "prompt 含正方形/logo/头像关键字"];
  if (isPoster) return ["1024x1536", "prompt 含海报/2:3 关键字"];
  if (isPhoto32) return ["1536x1024", "prompt 含照片/3:2 关键字"];
  if (isVert) return ["1024x1536", "prompt 含竖屏关键字（1K 默认）"];
  if (isHoriz) return ["1536x1024", "prompt 含横屏关键字（1K 默认）"];
  return null;
};

export const sizeNote = (
  requested: string,
  actual: [number, number] | null,
): string | null => {
  if (!actual) return null;
  const p = parseSize(requested);
  if (!p) return null;
  const [rw, rh] = p;
  const [aw, ah] = actual;
  if (aw === rw && ah === rh) return null;
  const rmp = (rw * rh) / 1_000_000;
  const amp = (aw * ah) / 1_000_000;
  // origin 把所有 ≤2.25MP 的请求统一处理到 ~1.57MP（看请求大小是放大还是压缩）
  if (rmp <= 2.25 && amp >= 1.3 && amp <= 1.8) {
    if (rmp <= 1.57) {
      return `ℹ 实际 ${aw}×${ah} (${amp.toFixed(2)}MP) > 请求 ${rw}×${rh} (${rmp.toFixed(2)}MP)：米醋对 ≤2.25MP 的请求等比放大到 ~1.57MP（福利档）。`;
    }
    return `⚠ 实际 ${aw}×${ah} (${amp.toFixed(2)}MP) < 请求 ${rw}×${rh} (${rmp.toFixed(2)}MP)：米醋对 ≤2.25MP 的请求统一压到 ~1.57MP（福利档降级）。想拿到真分辨率请改用 ≥4MP 的 size（如 2048×1152、1152×2048、2048×2048、3840×2160）。`;
  }
  return `⚠ 实际 ${aw}×${ah} (${amp.toFixed(2)}MP) ≠ 请求 ${rw}×${rh} (${rmp.toFixed(2)}MP)；如非 chat 路径请检查模型与 size 是否匹配。`;
};
