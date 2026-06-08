"""跨进程大图锁的取消安全回归（LOCK-1 / BLIND-6）。

旧实现用 `to_thread(阻塞 flock)`，等锁期间被取消会留下孤儿线程持锁 + fd 泄漏 → 进程级死锁。
新实现非阻塞轮询 + finally close(fd)，取消应干净释放，后续获取不被阻塞。
"""
from __future__ import annotations

import asyncio

import pytest

from micu_image_mcp import locks


@pytest.mark.skipif(not locks._FILE_LOCK_AVAILABLE, reason="无文件锁后端，锁退化为 no-op")
def test_lock_cancel_during_wait_does_not_deadlock():
    async def scenario() -> bool:
        async with locks._big_size_file_lock_async():
            # 第二个任务会卡在等锁（外层持有），让它进入轮询后取消它。
            async def waiter():
                async with locks._big_size_file_lock_async():
                    await asyncio.sleep(5)

            t = asyncio.create_task(waiter())
            await asyncio.sleep(0.3)
            t.cancel()
            with pytest.raises(asyncio.CancelledError):
                await t
        # 外层退出释放锁后，再次获取必须能在合理时间内拿到（无死锁/无孤儿持锁）。
        async with asyncio.timeout(5):
            async with locks._big_size_file_lock_async():
                return True
        return False

    assert asyncio.run(scenario()) is True
