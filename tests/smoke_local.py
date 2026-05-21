"""本机 smoke 测试（无需 API key）：

验证三件事：
  1. spawn server.py 子进程 + MCP stdio initialize + tools/list（协议层）
  2. in-process call 5 个 tool 入口校验链路（错误信息无回归）
  3. 边界拒绝：4K reference / N>10 / prompt 空 / image_path 不存在 / size 不合法

跑法:
  python tests/smoke_local.py            # 全跑
  python tests/smoke_local.py --proto    # 仅协议层
  python tests/smoke_local.py --entry    # 仅 tool 入口
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


# 在 import server 之前设沙箱
_SANDBOX = Path(tempfile.gettempdir()) / "micu-smoke"
_SANDBOX.mkdir(parents=True, exist_ok=True)
os.environ["MICU_SAVE_DIR_ROOT"] = str(_SANDBOX)
os.environ["MICU_SAVE_DIR"] = str(_SANDBOX)

# 故意不设 MICU_API_KEY，触发 "未配置 API key" 错误（验证错误链路）

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO))


def ok(msg: str) -> None:
    print(f"  \033[32m✓\033[0m {msg}")


def fail(msg: str) -> None:
    print(f"  \033[31m✗\033[0m {msg}")
    raise SystemExit(1)


def info(msg: str) -> None:
    print(f"  \033[36mℹ\033[0m {msg}")


# ---------- 1. MCP 协议层握手 ----------

async def test_proto() -> None:
    print("\n[1] MCP stdio 协议层握手（spawn server.py）")
    server_path = REPO / "server.py"
    if not server_path.exists():
        fail(f"server.py 不存在: {server_path}")

    proc = await asyncio.create_subprocess_exec(
        sys.executable, str(server_path),
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        env={**os.environ, "PYTHONUNBUFFERED": "1"},
    )

    async def send(msg: dict) -> None:
        line = json.dumps(msg, ensure_ascii=False) + "\n"
        proc.stdin.write(line.encode())  # type: ignore[union-attr]
        await proc.stdin.drain()  # type: ignore[union-attr]

    async def recv() -> dict:
        line = await asyncio.wait_for(proc.stdout.readline(), timeout=8.0)  # type: ignore[union-attr]
        if not line:
            stderr = await proc.stderr.read()  # type: ignore[union-attr]
            fail(f"server 提前退出。stderr:\n{stderr.decode(errors='replace')[:600]}")
        return json.loads(line.decode())

    # initialize
    await send({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0.0"},
        },
    })
    r = await recv()
    if r.get("result", {}).get("serverInfo", {}).get("name") == "micu-image":
        ok(f"initialize: serverInfo.name = {r['result']['serverInfo']['name']}")
    else:
        fail(f"initialize 响应异常: {r}")

    # initialized notification
    await send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    # tools/list
    await send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    r = await recv()
    tools = r.get("result", {}).get("tools", [])
    names = sorted(t["name"] for t in tools)
    expected = sorted([
        "image_generate", "image_edit", "image_batch_edit",
        "image_multi_reference", "server_info",
    ])
    if names == expected:
        ok(f"tools/list: 收到 {len(names)} 个 tool")
    else:
        fail(f"tools/list 不匹配。期望 {expected}，实际 {names}")

    # tools/call server_info
    await send({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "server_info", "arguments": {}},
    })
    r = await recv()
    content = r.get("result", {}).get("content", [])
    if content and content[0].get("type") == "text":
        info_data = json.loads(content[0]["text"])
        if info_data.get("base_url") == "https://www.micuapi.ai":
            ok(f"tools/call server_info: base_url={info_data['base_url']}")
        else:
            fail(f"server_info 响应内容异常: {info_data}")
    else:
        fail(f"tools/call 响应无内容: {r}")

    proc.terminate()
    try:
        await asyncio.wait_for(proc.wait(), timeout=2.0)
    except asyncio.TimeoutError:
        proc.kill()


# ---------- 2. tool 入口校验链路 (in-process) ----------

async def test_entries() -> None:
    print("\n[2] in-process 入口校验链路（no API key → '未配置 API key' 兜底）")
    import server as S  # type: ignore[import-not-found]

    # 2.1 prompt 空 → image_generate 立刻拒
    r = await S.image_generate(prompt="")
    if not r["ok"] and "prompt 不能为空" in r.get("error", ""):
        ok("image_generate prompt='' → '不能为空'")
    else:
        fail(f"prompt='' 没被拒: {r}")

    # 2.2 N 超界
    r = await S.image_generate(prompt="hi", n=99)
    if not r["ok"] and "≤" in r.get("error", "") and "burn quota" in r.get("error", ""):
        ok("image_generate n=99 → burn quota 拒")
    else:
        fail(f"n=99 没被拒: {r}")

    # 2.3 size 非 8 倍数
    r = await S.image_generate(prompt="hi", size="1500x1500")
    if not r["ok"] and "8" in r.get("error", ""):
        ok(f"image_generate size='1500x1500' → 8 倍数拒")
    else:
        fail(f"size 1500 没被拒: {r}")

    # 2.4 basename 非法
    r = await S.image_generate(prompt="hi", basename="../etc/passwd")
    if not r["ok"] and "非法字符" in r.get("error", ""):
        ok("image_generate basename='../etc/passwd' → 非法字符拒")
    else:
        fail(f"basename 攻击没被拒: {r}")

    # 2.5 save_dir 越界（root 外）
    r = await S.image_generate(prompt="hi", save_dir="/usr")
    if not r["ok"] and "安全根目录" in r.get("error", ""):
        ok("image_generate save_dir='/usr' → 越界拒")
    else:
        fail(f"save_dir 越界没拒: {r}")

    # 2.6 image_edit 4K rejected
    # 构造一个本地 1024x1024 PNG 给 image_path（让 4K 拒在 size 校验前先 OK 走到 4K 拒）
    img_path = _SANDBOX / "test.png"
    img_path.write_bytes(_minimal_png(1024, 1024))
    r = await S.image_edit(prompt="hi", image_path=str(img_path), size="3840x2160")
    if not r["ok"] and "4K" in r.get("error", ""):
        ok("image_edit size=4K → 已禁用拒")
    else:
        fail(f"image_edit 4K 没被拒: {r}")

    # 2.7 image_edit image_path 不存在
    r = await S.image_edit(prompt="hi", image_path="/nonexistent/xyz.png", size="1024x1024")
    if not r["ok"] and "不存在" in r.get("error", ""):
        ok("image_edit image_path 不存在 → 拒")
    else:
        fail(f"image_path 不存在没拒: {r}")

    # 2.8 image_multi_reference 4K rejected
    r = await S.image_multi_reference(
        prompt="hi",
        image_paths=[str(img_path), str(img_path)],
        size="3840x2160",
    )
    if not r["ok"] and "4K" in r.get("error", ""):
        ok("image_multi_reference size=4K → 已禁用拒")
    else:
        fail(f"image_multi_reference 4K 没被拒: {r}")

    # 2.9 走到 API key 缺失 → 原设计抛 RuntimeError 让 FastMCP 转 MCP error response
    try:
        r = await S.image_generate(prompt="a red apple on white", size="1024x1024")
        fail(f"应该 raise RuntimeError，但返回了 {r}")
    except RuntimeError as e:
        if "未配置 API key" in str(e):
            ok(f"image_generate no key → RuntimeError（{str(e)[:60]}...）")
        else:
            fail(f"RuntimeError 内容不匹配: {e}")

    # 2.10 服务端入口校验链路完整（注意 client 侧拒在 raise 之前）
    # prompt 空校验早于 _get_key，所以 prompt='' 返回 dict（已在 2.1 验证）
    # 反向：先校验 N 再到 _get_key
    try:
        r = await S.image_generate(prompt="hi", size="1024x1024", n=2)
        # 1K + n=2 不在 _get_key 之前拒（n 校验在最早）→ 应该走到 _get_key raise
        fail(f"n=2 + no key 应 raise，但返回 {r}")
    except RuntimeError as e:
        if "API key" in str(e):
            ok("image_generate n=2 + no key → 走到 _get_key raise（早期校验全部 OK）")
        else:
            fail(f"RuntimeError 不匹配: {e}")


def _minimal_png(width: int, height: int) -> bytes:
    """复制 tests/unit/test_image_inspection.py 里的最小 PNG 构造，独立可用。"""
    import struct
    import zlib
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr_data = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data)
    ihdr = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
    idat_data = zlib.compress(b"")
    idat_crc = zlib.crc32(b"IDAT" + idat_data)
    idat = struct.pack(">I", len(idat_data)) + b"IDAT" + idat_data + struct.pack(">I", idat_crc)
    iend_crc = zlib.crc32(b"IEND")
    iend = struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)
    return sig + ihdr + idat + iend


# ---------- main ----------

async def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--proto", action="store_true")
    ap.add_argument("--entry", action="store_true")
    args = ap.parse_args()
    do_all = not (args.proto or args.entry)

    if args.proto or do_all:
        await test_proto()
    if args.entry or do_all:
        await test_entries()

    print("\n\033[32m=== smoke 全部通过 ===\033[0m")


if __name__ == "__main__":
    asyncio.run(main())
