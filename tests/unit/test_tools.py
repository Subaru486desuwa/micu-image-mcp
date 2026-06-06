"""工具级（@mcp.tool）测试：用 MockTransport / monkeypatch 把 HTTP 层换掉，离线跑通整条
入口校验 → 调用 → 落盘 路径。这是之前缺失的一层（单测只覆盖纯 helper），C1 的
NameError 正是因为没有这层测试才漏到线上。

不依赖 pytest-asyncio：async tool 用 asyncio.run() 驱动。
"""
from __future__ import annotations

import asyncio
import base64
import io
import json

import httpx
import pytest
from PIL import Image

import server
from micu_image_mcp import http_client, routing, save
from micu_image_mcp.save import ImageSaveError


def _png_bytes(w: int = 32, h: int = 32) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", (w, h), (123, 50, 200)).save(buf, format="PNG")
    return buf.getvalue()


def _canned_b64_response() -> str:
    b64 = base64.b64encode(_png_bytes()).decode()
    return json.dumps({"data": [{"b64_json": b64}]})


@pytest.fixture
def fake_http(monkeypatch):
    """把 server 命名空间里的 _call_with_retry 换成返回固定 b64 PNG 的假实现。"""
    resp = _canned_b64_response()

    async def fake_call(ep, key, *args, **kwargs):  # noqa: ANN001
        return 200, resp

    monkeypatch.setattr(server, "_call_with_retry", fake_call)
    return resp


@pytest.fixture
def two_input_pngs(tmp_path):
    paths = []
    for i in range(2):
        p = tmp_path / f"in_{i}.png"
        p.write_bytes(_png_bytes())
        paths.append(str(p))
    return paths


# ---------- C1 回归：image_batch_edit 不再因未定义 key 而 100% 失败 ----------

def test_image_batch_edit_succeeds(fake_http, two_input_pngs):
    r = asyncio.run(server.image_batch_edit(prompt="sketch", image_paths=two_input_pngs, api_key="sk-test"))
    assert r["ok"] is True, r
    assert r["total"] == 2
    assert r["succeeded"] == 2, r
    assert r["failed"] == 0
    for item in r["results"]:
        assert item.get("ok") is True, item
        # 关键：绝不能再出现 "name 'key' is not defined"
        assert "is not defined" not in str(item.get("error", ""))


def test_image_batch_edit_forwards_no_crash_single(fake_http, two_input_pngs):
    r = asyncio.run(server.image_batch_edit(prompt="x", image_paths=two_input_pngs[:1], api_key="sk-test"))
    assert r["succeeded"] == 1


# ---------- image_edit / image_generate happy path（之前零覆盖）----------

def test_image_edit_happy_path(fake_http, two_input_pngs):
    r = asyncio.run(server.image_edit(prompt="recolor", image_path=two_input_pngs[0], api_key="sk-test"))
    assert r["ok"] is True, r
    assert "saved" in r and r["saved"]["path"]


def test_image_generate_happy_path(fake_http):
    r = asyncio.run(server.image_generate(prompt="a red apple", size="1024x1024", api_key="sk-test"))
    assert r["ok"] is True, r
    assert r["used_fallback"] is False
    assert r["saved"], r


# ---------- M7 子项：推断尺寸越界不再硬错，回退默认 ----------

@pytest.mark.parametrize("prompt", ["a 128x128 icon", "make a 100x100 thumbnail"])
def test_infer_size_below_min_returns_none(prompt):
    # 对齐后 < MIN_SIZE_EDGE(256) 时返回 None，让调用方兜底 1024，而非产出会被校验硬拒的 size
    assert routing._infer_size_from_prompt(prompt) is None


def test_infer_size_in_range_still_works():
    out = routing._infer_size_from_prompt("render at 1920x1080 please")
    assert out is not None and out[0] == "1920x1080"


def test_image_generate_small_pixel_prompt_falls_back(fake_http):
    # size=None + prompt 含 "128x128" → 不报错，落到默认 1024
    r = asyncio.run(server.image_generate(prompt="a 128x128 pixel icon", api_key="sk-test"))
    assert r["ok"] is True, r
    assert r["size"] == "1024x1024"


# ---------- H1：b64 解码前就拒超大响应 ----------

def test_save_image_b64_rejects_oversized(monkeypatch, tmp_path):
    monkeypatch.setattr(save, "MAX_RESPONSE_BYTES", 100)  # 缩小上限便于触发
    b64 = base64.b64encode(_png_bytes()).decode()  # 远大于 100 字节
    with pytest.raises(ImageSaveError, match="超过单图上限"):
        asyncio.run(save._save_image_b64(b64, tmp_path, "x"))


# ---------- H1：_call_endpoint 流式读取 + cap ----------

def test_call_endpoint_normal(monkeypatch):
    def handler(request):
        return httpx.Response(200, json={"data": [{"b64_json": "abc"}]})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)
    ep = http_client.Endpoint(url="https://x.test/v1/images/generations", json_body={"a": 1})
    status, text, headers = asyncio.run(http_client._call_endpoint(ep, "k"))
    assert status == 200
    assert "b64_json" in text
    asyncio.run(client.aclose())


def test_call_endpoint_rejects_oversized_body(monkeypatch):
    monkeypatch.setattr(http_client, "MAX_RESPONSE_BYTES", 10)

    def handler(request):
        return httpx.Response(200, content=b"x" * 1000)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)
    ep = http_client.Endpoint(url="https://x.test/v1/images/generations", json_body={"a": 1})
    status, text, headers = asyncio.run(http_client._call_endpoint(ep, "k"))
    assert status == 413, (status, text)
    asyncio.run(client.aclose())
