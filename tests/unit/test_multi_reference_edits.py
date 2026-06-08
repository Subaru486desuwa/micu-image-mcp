"""image_multi_reference 改走 /v1/images/edits + image[] 的回归。

旧主路径 generations + image_urls 被米醋静默忽略（image_tokens=0，参考图不起作用）。
新主路径必须：① _call_endpoint 把同名多文件列表展开成重复 image[] part；
② image_multi_reference 端到端 POST 落在 /v1/images/edits、含 N 个 image[]、不含 image_urls。
"""
from __future__ import annotations

import asyncio
import io

import httpx
import pytest
from PIL import Image

from micu_image_mcp import config, http_client
from micu_image_mcp.http_client import Endpoint, _call_endpoint
import server


def _png(c=(200, 30, 30)) -> bytes:
    b = io.BytesIO()
    Image.new("RGB", (48, 48), c).save(b, format="PNG")
    return b.getvalue()


def test_call_endpoint_expands_image_array(monkeypatch):
    cap = {}

    def handler(request: httpx.Request) -> httpx.Response:
        body = request.content  # AsyncClient+MockTransport 在调 handler 前已 aread()
        cap["url"] = str(request.url)
        cap["count"] = body.count(b'name="image[]"')
        cap["has_image_urls"] = b"image_urls" in body
        return httpx.Response(200, json={"data": [{"url": "https://1.1.1.1/x.png"}]})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)
    ep = Endpoint(
        url="https://www.micuapi.ai/v1/images/edits",
        multipart={
            "model": "gpt-image-2",
            "prompt": "x",
            "size": "1024x1024",
            "response_format": "url",
            "image[]": [
                ("a.png", _png(), "image/png"),
                ("b.png", _png(), "image/png"),
                ("c.png", _png(), "image/png"),
            ],
        },
    )
    status, _text, _h = asyncio.run(_call_endpoint(ep, "k"))
    asyncio.run(client.aclose())
    assert status == 200
    assert cap["count"] == 3          # 3 个同名 image[] part
    assert cap["has_image_urls"] is False
    assert cap["url"].endswith("/v1/images/edits")


def test_multi_reference_routes_to_edits_with_image_array(monkeypatch, tmp_path):
    posted = {}
    png = _png()

    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "POST":
            body = request.content
            posted["url"] = str(request.url)
            posted["count"] = body.count(b'name="image[]"')
            posted["has_image_urls"] = b"image_urls" in body
            return httpx.Response(200, json={"data": [{"url": "https://1.1.1.1/out.png"}]})
        return httpx.Response(200, content=png, headers={"content-length": str(len(png))})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)

    p1 = tmp_path / "r1.png"; p1.write_bytes(png)
    p2 = tmp_path / "r2.png"; p2.write_bytes(png)
    save_dir = config._SAVE_ROOT / "mref_test"
    save_dir.mkdir(parents=True, exist_ok=True)

    r = asyncio.run(server.image_multi_reference(
        prompt="把这两张合成一张新图",
        image_paths=[str(p1), str(p2)],
        size="1024x1024",
        save_dir=str(save_dir),
        api_key="testkey",
    ))
    asyncio.run(client.aclose())

    assert r["ok"] is True, r
    assert posted["url"].endswith("/v1/images/edits")   # 不再是 /v1/images/generations
    assert posted["count"] == 2                          # 两张参考图都作为 image[] 发出
    assert posted["has_image_urls"] is False             # 旧的被忽略字段已弃用
    assert r["n_references"] == 2


def test_image_edit_high_res_routes_to_edits(monkeypatch, tmp_path):
    """≥2K image_edit 改走 /v1/images/edits（旧坏路径 generations + reference_image 已废弃）。"""
    posted = {}
    png = _png()

    def handler(request: httpx.Request) -> httpx.Response:
        if request.method == "POST":
            body = request.content
            posted["url"] = str(request.url)
            posted["has_image"] = b'name="image"' in body
            return httpx.Response(200, json={"data": [{"url": "https://1.1.1.1/out.png"}]})
        return httpx.Response(200, content=png, headers={"content-length": str(len(png))})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)

    src = tmp_path / "src.png"; src.write_bytes(png)
    save_dir = config._SAVE_ROOT / "edit_test"
    save_dir.mkdir(parents=True, exist_ok=True)

    r = asyncio.run(server.image_edit(
        prompt="把背景改成星空",
        image_path=str(src),
        size="2048x2048",
        save_dir=str(save_dir),
        api_key="testkey",
    ))
    asyncio.run(client.aclose())

    assert r["ok"] is True, r
    assert posted["url"].endswith("/v1/images/edits")   # 不再是 /v1/images/generations
    assert posted["has_image"] is True                  # 输入图作为 image part 发出
