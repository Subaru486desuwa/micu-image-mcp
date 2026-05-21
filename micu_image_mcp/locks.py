"""跨进程文件锁 + 进程内 Semaphore（≥2K 串行队列）。"""
from __future__ import annotations

import asyncio
import os
import time
from contextlib import asynccontextmanager
from pathlib import Path

from .config import _LOCK_BACKEND, _FILE_LOCK_AVAILABLE

# Lazy import 实际锁后端：fcntl 在 POSIX；msvcrt 在 Windows。
if _LOCK_BACKEND == "posix":
    import fcntl  # type: ignore[import-untyped]
elif _LOCK_BACKEND == "windows":
    import msvcrt  # type: ignore[import-untyped]


# ≥2K 在米醋 origin 走 pro 模型串行队列，单张渲染 ~50-80s。
# 客户端并发 N 张时第 2 张就要排队等前一张，累积容易撞 CF 120s 硬上限 → 524 雪球。
#
# 双层锁：
#   - 进程内 Semaphore(1)：同 MCP 进程内的并发请求快速本地排队，零系统调用。
#   - 跨进程文件锁（POSIX flock）：多个 Claude Code / Codex 窗口各自 spawn 独立 MCP
#     子进程时，让所有进程串行打 origin。多窗口开发是常态（用户实测 5 进程并发就撞 524）。
# Lazy init：避免 module 导入期与 fastmcp event loop 不一致。
_BIG_SIZE_LOCK: asyncio.Semaphore | None = None
# 跨进程锁文件位置：固定 ~/.cache 下的 user-scoped 路径。
# 不用 tempfile.gettempdir() 是因为 Mac launchd 给 GUI 进程的 TMPDIR 与 terminal 进程不同
# (/var/folders/<hash>/T/ vs /tmp/...)，会让 GUI 启动的 Claude Code 与 terminal MCP 锁不同文件。
_BIG_SIZE_FILE_LOCK_PATH = Path.home() / ".cache" / "micu-image" / "bigsize.lock"


def _get_big_size_lock() -> asyncio.Semaphore:
    global _BIG_SIZE_LOCK
    if _BIG_SIZE_LOCK is None:
        _BIG_SIZE_LOCK = asyncio.Semaphore(1)
    return _BIG_SIZE_LOCK


def _acquire_big_size_file_lock_blocking() -> int:
    """阻塞获取系统级跨进程锁；返回 fd，关 fd 即释放。

    POSIX: fcntl.flock(LOCK_EX)，原生阻塞。
    Windows: msvcrt.locking(LK_LOCK, 1)，单次阻塞超时 10s，循环直到拿到。
    """
    _BIG_SIZE_FILE_LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    if _LOCK_BACKEND == "posix":
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_WRONLY | os.O_CREAT, 0o644)
        fcntl.flock(fd, fcntl.LOCK_EX)
        return fd
    if _LOCK_BACKEND == "windows":
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_RDWR | os.O_CREAT, 0o644)
        try:
            os.write(fd, b"\x00")
        except OSError:
            pass
        os.lseek(fd, 0, os.SEEK_SET)
        while True:
            try:
                msvcrt.locking(fd, msvcrt.LK_LOCK, 1)
                return fd
            except OSError:
                # 10s timeout 未拿到，再试
                continue
    raise RuntimeError("file lock backend unavailable")


def _release_big_size_file_lock(fd: int) -> None:
    try:
        if _LOCK_BACKEND == "posix":
            fcntl.flock(fd, fcntl.LOCK_UN)
        elif _LOCK_BACKEND == "windows":
            os.lseek(fd, 0, os.SEEK_SET)
            try:
                msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
            except OSError:
                pass
    finally:
        try:
            os.close(fd)
        except OSError:
            pass


@asynccontextmanager
async def _big_size_file_lock_async(notes_out: list[str] | None = None):
    """跨进程串行 ≥2K 请求。Windows 无 fcntl 时退化为 no-op（仅进程内 Semaphore 生效）。

    notes_out: 可选 list[str]，等锁 >2s 时附加排队 note 让多窗口排队对用户可见。
    """
    if not _FILE_LOCK_AVAILABLE:
        yield
        return
    t0 = time.monotonic()
    fd = await asyncio.to_thread(_acquire_big_size_file_lock_blocking)
    wait_s = time.monotonic() - t0
    if notes_out is not None and wait_s > 2.0:
        notes_out.append(
            f"等待跨进程 ≥2K 锁 {wait_s:.1f}s（其他 Claude Code / Codex 窗口同时在跑 ≥2K，已串行）"
        )
    try:
        yield
    finally:
        await asyncio.to_thread(_release_big_size_file_lock, fd)


__all__ = [
    "_BIG_SIZE_FILE_LOCK_PATH",
    "_get_big_size_lock",
    "_acquire_big_size_file_lock_blocking",
    "_release_big_size_file_lock",
    "_big_size_file_lock_async",
]
