import { homedir } from "node:os";
import path from "node:path";

const env = (k: string, d = ""): string => process.env[k] ?? d;
const flag = (v: string): boolean => ["1", "true", "yes"].includes(v.trim().toLowerCase());

export const DEFAULT_BASEURL: string = env("MICU_BASEURL", "https://www.micuapi.ai");
export const API_KEY: string = env("MICU_API_KEY", "");
export const DEFAULT_MODEL: string = env("MICU_MODEL", "gpt-image-2");
export const TRUST_ENV_PROXY: boolean = flag(env("MICU_USE_SHELL_PROXY"));

export const SAVE_ROOT: string = path.resolve(
  env("MICU_SAVE_DIR_ROOT", path.join(homedir(), "Pictures", "micu-out")),
);
export const DEFAULT_SAVE_DIR: string = env("MICU_SAVE_DIR", SAVE_ROOT);

export const PRO_MODEL = "gpt-image-2-pro";
export const NONPRO_MODEL = "gpt-image-2";

export const HIGH_RES_EDGE = 1600;
export const EDITS_MAX_EDGE = 1536;

export const VALID_SIZES_1K: readonly string[] = [
  "1024x1024", "1280x720", "720x1280", "1024x1536", "1536x1024",
];
export const VALID_SIZES_2K: readonly string[] = [
  "1920x1080", "1080x1920", "2048x2048", "2048x1152", "1152x2048",
];
export const VALID_SIZES_4K: readonly string[] = ["3840x2160", "2160x3840"];

export const MAX_N = 10;
export const MIN_SIZE_EDGE = 256;
export const MAX_SIZE_EDGE = 4096;
export const SIZE_ALIGNMENT = 8;

export const MAX_INPUT_FILE_BYTES = 4 * 1024 * 1024;
export const MAX_TOTAL_INPUT_BYTES = 8 * 1024 * 1024;
export const MAX_RESPONSE_BYTES = 25 * 1024 * 1024;

export const BIG_SIZE_FILE_LOCK_PATH: string = path.join(
  homedir(), ".cache", "micu-image", "bigsize.lock",
);

export const RETRYABLE_STATUS: readonly number[] = [
  0, 408, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527,
];
