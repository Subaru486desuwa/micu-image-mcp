import {
  API_KEY, BIG_SIZE_FILE_LOCK_PATH, DEFAULT_BASEURL, DEFAULT_MODEL,
  DEFAULT_SAVE_DIR, HIGH_RES_EDGE, MAX_INPUT_FILE_BYTES, MAX_N,
  MAX_RESPONSE_BYTES, MAX_SIZE_EDGE, MAX_TOTAL_INPUT_BYTES,
  MIN_SIZE_EDGE, NONPRO_MODEL, PRO_MODEL, RETRYABLE_STATUS,
  SAVE_ROOT, SIZE_ALIGNMENT, VALID_SIZES_1K, VALID_SIZES_2K, VALID_SIZES_4K,
} from "../config.ts";

export const serverInfo = (): Record<string, unknown> => ({
  base_url: DEFAULT_BASEURL,
  default_model: DEFAULT_MODEL,
  available_models: [NONPRO_MODEL, PRO_MODEL],
  default_save_dir: DEFAULT_SAVE_DIR,
  api_key_configured: !!API_KEY,
  size_rules: {
    format: "WxH 字符串（如 '1024x1024'）",
    alignment: `W 与 H 都必须是 ${SIZE_ALIGNMENT} 的整数倍（米醋实测约束，OpenAI 官方要 16）`,
    edge_range: `W/H 必须在 [${MIN_SIZE_EDGE}, ${MAX_SIZE_EDGE}] 范围内`,
    compress_below_2_25mp:
      "请求总像素 ≤ 2.25MP（如 1024² / 1280×720 / 1500² / 1920×1080）会被代理" +
      "等比放大或压缩到 ~1.57MP（福利档），实际输出 ≠ 请求 size。",
    exact_above_4mp:
      "请求总像素 ≥ 4MP（如 2048² / 3840×2160）严格按 size 1:1 输出。",
    auto_pro_threshold:
      `max edge ≥ ${HIGH_RES_EDGE} → 自动锁 ${PRO_MODEL}（${NONPRO_MODEL} 在该档代理会拒）。`,
  },
  safety_constraints: {
    n_range: `image_generate 的 n ∈ [1, ${MAX_N}]，超出立即拒（防 burn quota）`,
    save_dir_root:
      `所有输出强制落在 MICU_SAVE_DIR_ROOT=${SAVE_ROOT} 之下；传 root 之外路径会被拒`,
    basename_charset: "basename 仅允许 [A-Za-z0-9_-.]，禁含 / .. 和路径分量",
    input_size_limits:
      `单输入图 ≤${Math.floor(MAX_INPUT_FILE_BYTES / 1024 / 1024)}MB；` +
      `image_multi_reference 总和 ≤${Math.floor(MAX_TOTAL_INPUT_BYTES / 1024 / 1024)}MB`,
    input_image_validation:
      "所有输入图按 magic bytes 校验为 PNG/JPEG/WebP/GIF；非图片立即拒（防本地任意文件外传）",
    response_size_limit:
      `远端响应 ≤${Math.floor(MAX_RESPONSE_BYTES / 1024 / 1024)}MB；超过中断不落盘`,
    base_url_locked:
      "base_url 锁在启动期 MICU_BASEURL env，运行期 tool 不接受参数（防 key 外泄到攻击者 host）",
  },
  recommended_sizes: {
    "1k_福利档_约1.57MP": [...VALID_SIZES_1K].sort(),
    "2k_仅pro_严格1_1": [...VALID_SIZES_2K].sort(),
    "4k_仅pro_严格1_1": [...VALID_SIZES_4K].sort(),
    tip: "想拿到精确分辨率请选 2K/4K 档；选 1K 档会被代理统一拉到 1.57MP。",
  },
  capability_matrix: {
    image_generate: {
      "1k": "可用，single 30s，N>1 自动 5 并发",
      "2k_pro": "可用，single 40-60s，N=1 强制；origin 拥塞撞 524 时自动 fallback 到 chat stream（输出 ~1.57MP，notes 里有标记）",
      "4k_pro": "可用，single 50-80s，N=1 强制；偶尔 524 自动重试",
    },
    image_edit: { note: "TS 端口尚未实现（只移植了 image_generate）" },
    image_batch_edit: { note: "TS 端口尚未实现" },
    image_multi_reference: { note: "TS 端口尚未实现" },
  },
  retry_policy: {
    retryable_status: [...RETRYABLE_STATUS],
    schedule_1k: "失败 → 4s + jitter → 重试 → 8s + jitter → 重试（共 3 次尝试）；网络层异常额外免费重试 1 次",
    schedule_2k_4k:
      "双层锁内：可恢复 5xx → 60s → 重试 1 次（共 2 次尝试）；CF 524 fail fast 不重试。" +
      "锁让整机任意时刻只有一个 ≥2K 请求打到 origin，避免多客户端并发 + origin pro 队列堆叠 → CF 524 雪球。" +
      "锁等待 >2s 时 notes 会提示在排队",
    trigger: "model 含 'pro' 或 size tier ∈ {2k, 4k}",
    concurrency_2k_4k:
      "双层锁: (1) 进程内 Semaphore(1) 同 MCP 进程内并发本地排队; " +
      `(2) 跨进程 lockfile @ ${BIG_SIZE_FILE_LOCK_PATH}（proper-lockfile 跨平台原子重命名实现） —— ` +
      "多 Claude Code/Codex 窗口各自独立 MCP 子进程时跨进程串行打 origin。",
  },
  response_handling: {
    saved_to_disk: "所有生成的图片落盘到 save_dir（默认 MICU_SAVE_DIR_ROOT）",
    actual_size_field: "返回的 saved[].actual_size 是从 PNG/JPEG header 读出的真实像素，可与请求 size 对比验证",
    extract_paths: "支持 data[].b64_json / data[].url / chat content markdown 三种响应格式",
  },
  port_status: {
    runtime: "TypeScript / Bun",
    implemented_tools: ["image_generate", "server_info"],
    pending_tools: ["image_edit", "image_batch_edit", "image_multi_reference"],
    notes: "首版只跑通了 image_generate end-to-end；其他 tool 还在 main 分支的 Python 版本里。",
  },
});
