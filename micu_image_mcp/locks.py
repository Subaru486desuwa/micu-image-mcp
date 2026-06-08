"""跨进程文件锁 + 进程内 Semaphore（≥2K 串行队列）。"""
from __future__ import annotations

import asyncio
import errno
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
        # flock 抛错（EINTR/EDEADLK/ENOLCK 等）时必须关 fd，否则每次失败泄漏一个 fd。
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
        except BaseException:
            os.close(fd)
            raise
        return fd
    if _LOCK_BACKEND == "windows":
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_RDWR | os.O_CREAT, 0o644)
        try:
            try:
                os.write(fd, b"\x00")
            except OSError:
                pass
            os.lseek(fd, 0, os.SEEK_SET)
            while True:
                try:
                    msvcrt.locking(fd, msvcrt.LK_LOCK, 1)
                    return fd
                except OSError as e:
                    # EDEADLK = msvcrt 10s 内未拿到锁的超时信号，继续等待重试；
                    # 其它 errno（EACCES/EBADF 等）是永久错误，不能无限 busy-loop，关 fd 后抛出。
                    if e.errno == errno.EDEADLK:
                        continue
                    raise
        except BaseException:
            os.close(fd)
            raise
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


# 轮询间隔：非阻塞获取失败后等多久再试。锁竞争是分钟级（≥2K 单张 50-80s），0.1s 轮询开销可忽略。
_LOCK_POLL_INTERVAL_SECONDS = 0.1


def _open_lock_fd() -> int:
    """打开（必要时创建）锁文件返回 fd。不获取锁，构造近乎瞬时。"""
    _BIG_SIZE_FILE_LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    if _LOCK_BACKEND == "posix":
        return os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_WRONLY | os.O_CREAT, 0o644)
    if _LOCK_BACKEND == "windows":
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_RDWR | os.O_CREAT, 0o644)
        try:
            os.write(fd, b"\x00")
        except OSError:
            pass
        os.lseek(fd, 0, os.SEEK_SET)
        return fd
    raise RuntimeError("file lock backend unavailable")


def _try_lock_nb(fd: int) -> bool:
    """非阻塞尝试获取锁：成功 True，被他人占用 False，其它错误抛出。

    非阻塞是关键：调用方在 async 侧轮询，使取消能在 await 点干净中断，
    不会像阻塞 flock 那样把 to_thread 线程永久卡住并孤儿持锁。
    """
    if _LOCK_BACKEND == "posix":
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except OSError as e:
            if e.errno in (errno.EAGAIN, errno.EWOULDBLOCK, errno.EACCES):
                return False
            raise
    if _LOCK_BACKEND == "windows":
        os.lseek(fd, 0, os.SEEK_SET)
        try:
            msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)
            return True
        except OSError as e:
            if e.errno in (errno.EDEADLK, errno.EACCES, errno.EAGAIN):
                return False
            raise
    return True


@asynccontextmanager
async def _big_size_file_lock_async(notes_out: list[str] | None = None):
    """跨进程串行 ≥2K 请求。Windows 无 fcntl 时退化为 no-op（仅进程内 Semaphore 生效）。

    notes_out: 可选 list[str]，等锁 >2s 时附加排队 note 让多窗口排队对用户可见。

    取消安全：async 侧非阻塞获取 + asyncio.sleep 轮询；无论在哪步被取消，finally 都会 close(fd)，
    而 close 本身即释放该 fd 上的 advisory 锁——不会留下孤儿持锁线程 / 泄漏 fd。
    """
    if not _FILE_LOCK_AVAILABLE:
        yield
        return
    t0 = time.monotonic()
    fd = await asyncio.to_thread(_open_lock_fd)
    acquired = False
    try:
        while True:
            acquired = await asyncio.to_thread(_try_lock_nb, fd)
            if acquired:
                break
            await asyncio.sleep(_LOCK_POLL_INTERVAL_SECONDS)
        wait_s = time.monotonic() - t0
        if notes_out is not None and wait_s > 2.0:
            notes_out.append(
                f"等待跨进程 ≥2K 锁 {wait_s:.1f}s（其他 Claude Code / Codex 窗口同时在跑 ≥2K，已串行）"
            )
        yield
    finally:
        if acquired:
            _release_big_size_file_lock(fd)  # 显式 LOCK_UN + close
        else:
            try:
                os.close(fd)  # close 即释放任何已持有的 advisory 锁
            except OSError:
                pass


__all__ = [
    "_BIG_SIZE_FILE_LOCK_PATH",
    "_get_big_size_lock",
    "_acquire_big_size_file_lock_blocking",
    "_release_big_size_file_lock",
    "_open_lock_fd", "_try_lock_nb",
    "_big_size_file_lock_async",
]
