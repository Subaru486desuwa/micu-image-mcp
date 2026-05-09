import { mkdir, writeFile, access } from "node:fs/promises";
import path from "node:path";
import lockfile from "proper-lockfile";
import { BIG_SIZE_FILE_LOCK_PATH } from "./config.ts";

/** 进程内简易信号量。Bun/Node 没 stdlib 等价物，自己实现一个。 */
export class Semaphore {
  private permits: number;
  private waiters: Array<() => void> = [];
  constructor(permits: number) { this.permits = permits; }
  async acquire(): Promise<void> {
    if (this.permits > 0) { this.permits--; return; }
    await new Promise<void>((res) => this.waiters.push(res));
    this.permits--;
  }
  release(): void {
    this.permits++;
    const w = this.waiters.shift();
    if (w) w();
  }
  async run<T>(fn: () => Promise<T>): Promise<T> {
    await this.acquire();
    try { return await fn(); } finally { this.release(); }
  }
}

let _bigSizeLock: Semaphore | null = null;
export const getBigSizeLock = (): Semaphore => {
  if (!_bigSizeLock) _bigSizeLock = new Semaphore(1);
  return _bigSizeLock;
};

const ensureLockTarget = async (): Promise<void> => {
  await mkdir(path.dirname(BIG_SIZE_FILE_LOCK_PATH), { recursive: true });
  try {
    await access(BIG_SIZE_FILE_LOCK_PATH);
  } catch {
    await writeFile(BIG_SIZE_FILE_LOCK_PATH, "");
  }
};

/**
 * 跨进程串行 ≥2K 请求。多 Claude Code / Codex 窗口各自 spawn 独立 MCP 子进程时，
 * 让所有进程串行打 origin（avoid 524 雪球）。
 *
 * proper-lockfile 在 Windows / POSIX 都能跑（用同目录 .lock 子目录原子重命名实现）。
 * 用 retries=Infinity 阻塞等锁，与 Python 版 fcntl.flock(LOCK_EX) 行为对齐。
 */
export const withBigSizeFileLock = async <T>(
  notesOut: string[] | null,
  fn: () => Promise<T>,
): Promise<T> => {
  await ensureLockTarget();
  const t0 = Date.now();
  const release = await lockfile.lock(BIG_SIZE_FILE_LOCK_PATH, {
    retries: { retries: 600, factor: 1, minTimeout: 1000, maxTimeout: 1000 },
    stale: 180_000,
  });
  const waitS = (Date.now() - t0) / 1000;
  if (notesOut && waitS > 2) {
    notesOut.push(
      `等待跨进程 ≥2K 锁 ${waitS.toFixed(1)}s（其他 Claude Code / Codex 窗口同时在跑 ≥2K，已串行）`,
    );
  }
  try {
    return await fn();
  } finally {
    try { await release(); } catch { /* lockfile 已被外部释放/失效 */ }
  }
};
