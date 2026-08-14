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
import subprocess
import sys
from pathlib import Path

import httpx
import pytest
from PIL import Image

import server
from micu_image_mcp import http_client, routing, save
from micu_image_mcp.save import ImageSaveError


REPO_ROOT = Path(__file__).resolve().parents[2]


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


@pytest.mark.parametrize(
    ("tool", "kwargs"),
    [
        (server.image_generate, {"prompt": "test"}),
        (server.image_edit, {"prompt": "test", "image_path": "/does/not/exist.png"}),
        (server.image_batch_edit, {"prompt": "test", "image_paths": ["/does/not/exist.png"]}),
        (
            server.image_multi_reference,
            {"prompt": "test", "image_paths": ["/does/not/exist-a.png", "/does/not/exist-b.png"]},
        ),
    ],
)
def test_image_tools_reject_grok_before_io_or_network(tool, kwargs):
    r = asyncio.run(tool(model="grok-imagine-image", api_key="sk-test", **kwargs))

    assert r["ok"] is False
    assert "Grok" in r["error"]
    assert "暂时关闭" in r["error"]


@pytest.mark.parametrize("model", ["dall-e-3", " gpt-image-2 "])
def test_image_generate_rejects_every_other_model(model):
    r = asyncio.run(server.image_generate(prompt="test", model=model, api_key="sk-test"))

    assert r["ok"] is False
    assert "gpt-image-2 / gpt-image-2-openai" in r["error"]
    assert "Grok 生图渠道暂时关闭" in r["error"]


def test_server_info_reports_only_image2_models():
    r = server.server_info()

    assert r["available_models"] == ["gpt-image-2", "gpt-image-2-openai"]
    assert r["grok_channel_enabled"] is False
    assert r["grok_available_models"] == []
    assert "grok_api_key_configured" in r
    assert "暂时关闭" in r["recommended_sizes"]["grok_tip"]
    assert "暂时关闭" in r["capability_matrix"]["grok_image_generate"]["1k"]
    assert "暂时关闭" in r["response_handling"]["grok_extract_paths"]


@pytest.mark.parametrize("script", ["perf_bench.py", "stress_concurrent.py"])
def test_benchmark_cli_does_not_offer_grok(script):
    result = subprocess.run(
        [sys.executable, str(REPO_ROOT / "tests" / script), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert "grok" not in result.stdout.lower()
    assert "gpt-image-2-openai" in result.stdout


def test_image_generate_uses_url_first_response_format(monkeypatch):
    """auto 模式默认先请求 response_format=url。"""
    captured: dict = {}

    async def fake_call(ep, key, *args, **kwargs):  # noqa: ANN001
        if ep.json_body:
            captured.update(ep.json_body)
        return 200, _canned_b64_response()

    monkeypatch.setattr(server, "_call_with_retry", fake_call)
    monkeypatch.setattr(server, "RESPONSE_FORMATS_TO_TRY", ("url", "b64_json"))
    r = asyncio.run(server.image_generate(prompt="test", size="1024x1024", api_key="sk-test"))
    assert r["ok"] is True, r
    assert captured.get("response_format") == "url"


def test_image_generate_forwards_quality(monkeypatch):
    resp = _canned_b64_response()
    calls = []

    async def fake_call(ep, key, *args, **kwargs):  # noqa: ANN001
        calls.append(ep)
        return 200, resp

    monkeypatch.setattr(server, "_call_with_retry", fake_call)

    r = asyncio.run(server.image_generate(
        prompt="一只红苹果",
        size="1024x1024",
        quality="high",
        api_key="sk-test",
    ))

    assert r["ok"] is True, r
    assert calls, "expected image_generate to call the backend"
    assert calls[0].json_body["quality"] == "high"


def test_save_extracted_payload_url_then_b64(monkeypatch, tmp_path):
    """同响应含 url+b64 时：url 失败应 fallback 到 b64。"""
    from micu_image_mcp import config

    png = _png_bytes()
    b64 = base64.b64encode(png).decode()
    save_dir = config._SAVE_ROOT / "url_b64_fb"
    save_dir.mkdir(parents=True, exist_ok=True)

    async def failing_url(*_a, **_k):
        raise save.ImageSaveError("url fail")

    monkeypatch.setattr(save, "_save_image_url", failing_url)
    notes: list[str] = []
    p, actual, size_bytes = asyncio.run(
        save._save_extracted_payload(b64, "https://example.com/x.png", save_dir, "t", notes)
    )
    assert p.exists()
    assert size_bytes == len(png)
    assert any("b64_json" in n for n in notes)


# ---------- M7 子项：推断尺寸越界不再硬错，回退默认 ----------

@pytest.mark.parametrize("prompt", ["a 128x128 icon", "make a 100x100 thumbnail"])
def test_infer_size_below_min_returns_none(prompt):
    # 对齐后 < MIN_SIZE_EDGE(256) 时返回 None，让调用方兜底 1024，而非产出会被校验硬拒的 size
    assert routing._infer_size_from_prompt(prompt) is None


@pytest.mark.parametrize("prompt", ["render at 512x512", "render at 3840x1024"])
def test_infer_size_outside_pixel_or_ratio_contract_returns_none(prompt):
    assert routing._infer_size_from_prompt(prompt) is None


def test_infer_size_in_range_still_works():
    out = routing._infer_size_from_prompt("render at 1920x1080 please")
    assert out is not None and out[0] == "1920x1088"


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


def test_call_endpoint_sends_literal_utf8_json(monkeypatch):
    prompt = "一只可爱的小猫正在跳伞"

    def handler(request):
        assert request.headers["content-type"] == "application/json; charset=utf-8"
        assert prompt.encode("utf-8") in request.content
        assert b"\\u4e00" not in request.content
        assert json.loads(request.content.decode("utf-8"))["prompt"] == prompt
        return httpx.Response(200, json={"data": [{"b64_json": "abc"}]})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)
    ep = http_client.Endpoint(
        url="https://x.test/v1/images/generations",
        json_body={"prompt": prompt},
    )
    status, _, _ = asyncio.run(http_client._call_endpoint(ep, "k"))
    assert status == 200
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
