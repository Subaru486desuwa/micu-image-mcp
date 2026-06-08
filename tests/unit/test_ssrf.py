"""SSRF / url 下载主路径 / 输入路径牢笼 的回归测试（BLIND-1）。

response_format 改为 url 后，_save_image_url 成为主下载路径且是 SSRF sink；
此前测试 fixture 永远返回 b64，这条路径零覆盖。这里补上：
  - _assert_download_url_safe 对内网/元数据/环回/非法 scheme 的拦截
  - 公网地址放行
  - 域名解析到内网时按解析结果拦截（DNS-rebinding 直指内网）
  - _save_image_url 端到端（MockTransport）能落盘
  - MICU_INPUT_ROOT 输入牢笼
  - PATH-2：错误信息不回显文件原始字节
  - PATH-5：截断 WebP 返回 None 而非 IndexError
"""
from __future__ import annotations

import asyncio
import io

import httpx
import pytest
from PIL import Image

from micu_image_mcp import config, http_client, io_safety, save
from micu_image_mcp.save import ImageSaveError, _assert_download_url_safe


def _png_bytes(w: int = 32, h: int = 32) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", (w, h), (200, 30, 30)).save(buf, format="PNG")
    return buf.getvalue()


# ---------- _assert_download_url_safe ----------

@pytest.mark.parametrize("url", [
    "file:///etc/passwd",
    "ftp://example.com/x.png",
    "gopher://example.com/x",
    "data:image/png;base64,AAAA",
])
def test_reject_bad_scheme(url):
    with pytest.raises(ImageSaveError):
        _assert_download_url_safe(url)


@pytest.mark.parametrize("url", [
    "http://127.0.0.1/x.png",
    "http://169.254.169.254/latest/meta-data/",   # 云元数据 IMDS
    "http://10.0.0.1/x.png",
    "http://192.168.1.5/x.png",
    "http://172.16.0.1/x.png",
    "http://[::1]/x.png",                          # IPv6 环回
    "https://[fc00::1]/x.png",                     # IPv6 ULA 私网
    "http://0.0.0.0/x.png",                        # unspecified
    "http://[::ffff:127.0.0.1]/x.png",             # IPv4-mapped 环回（防绕过）
])
def test_reject_internal_ip_literals(url):
    with pytest.raises(ImageSaveError):
        _assert_download_url_safe(url)


@pytest.mark.parametrize("url", [
    "https://1.1.1.1/x.png",
    "https://8.8.8.8/a/b.png",
])
def test_allow_public_ip_literals(url):
    # 不应抛出
    _assert_download_url_safe(url)


def test_block_hostname_resolving_to_private(monkeypatch):
    def fake_getaddrinfo(host, port, *a, **k):
        return [(2, 1, 6, "", ("10.1.2.3", port or 443))]
    monkeypatch.setattr(save.socket, "getaddrinfo", fake_getaddrinfo)
    with pytest.raises(ImageSaveError):
        _assert_download_url_safe("https://images.evil.example/x.png")


def test_allow_hostname_resolving_to_public(monkeypatch):
    def fake_getaddrinfo(host, port, *a, **k):
        return [(2, 1, 6, "", ("93.184.216.34", port or 443))]
    monkeypatch.setattr(save.socket, "getaddrinfo", fake_getaddrinfo)
    _assert_download_url_safe("https://oss.filenest.top/uploads/x.png")


# ---------- _save_image_url 端到端（now-primary 路径） ----------

def test_save_image_url_downloads_and_writes(monkeypatch, tmp_path):
    png = _png_bytes()

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=png, headers={"content-length": str(len(png))})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)

    # save_dir 必须在 _SAVE_ROOT 之下（落盘越界复查以 _SAVE_ROOT 为基准）
    save_dir = config._SAVE_ROOT / "ssrf_dl"
    save_dir.mkdir(parents=True, exist_ok=True)

    p, actual, size_bytes = asyncio.run(
        save._save_image_url("https://1.1.1.1/img.png", save_dir, "dl")
    )
    assert p.exists()
    assert p.suffix == ".png"
    assert actual == (32, 32)
    assert size_bytes == len(png)
    asyncio.run(client.aclose())


def test_save_image_url_rejects_ssrf_before_request(monkeypatch):
    # 即便 client 存在，内网 url 也应在发起请求前被拒。
    called = {"hit": False}

    def handler(request: httpx.Request) -> httpx.Response:
        called["hit"] = True
        return httpx.Response(200, content=_png_bytes())

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    monkeypatch.setattr(http_client, "_HTTP_CLIENT", client)
    save_dir = config._SAVE_ROOT / "ssrf_dl"
    save_dir.mkdir(parents=True, exist_ok=True)
    with pytest.raises(ImageSaveError):
        asyncio.run(save._save_image_url("http://169.254.169.254/x.png", save_dir, "x"))
    assert called["hit"] is False  # 请求未发出
    asyncio.run(client.aclose())


# ---------- MICU_INPUT_ROOT 输入牢笼 ----------

def test_input_root_jail(monkeypatch, tmp_path):
    root = tmp_path.resolve()
    monkeypatch.setattr(io_safety, "_INPUT_ROOT", root)

    inside = root / "ok.png"
    inside.write_bytes(_png_bytes())
    _, _, _, err = io_safety._validate_image_path(str(inside))
    assert err is None  # 根内的合法图片放行

    outside = tmp_path.parent / "outside.png"
    outside.write_bytes(_png_bytes())
    _, _, _, err2 = io_safety._validate_image_path(str(outside))
    assert err2 is not None and "MICU_INPUT_ROOT" in err2


def test_input_root_default_unrestricted():
    # 默认 _INPUT_ROOT=None：根外路径不因牢笼被拒（仍可能因不存在/非图片被拒，但不是牢笼错误）。
    assert io_safety._INPUT_ROOT is None
    _, _, _, err = io_safety._validate_image_path("/nonexistent/whatever.png")
    assert err is not None and "MICU_INPUT_ROOT" not in err


# ---------- PATH-2 / PATH-5 ----------

def test_validate_image_bytes_no_byte_echo():
    msg = io_safety._validate_image_bytes(b"TOPSECRET_TOKEN=abcdef0123456789")
    assert msg is not None
    assert "TOPSECRET" not in msg  # 不回显文件内容


def test_truncated_webp_returns_none_not_indexerror():
    # RIFF....WEBP + VP8X 但截断到 28 字节（<30），应返回 None 而非 IndexError
    raw = b"RIFF" + b"\x00\x00\x00\x00" + b"WEBP" + b"VP8X" + b"\x00" * 12
    assert len(raw) >= 24
    assert io_safety._detect_actual_size(raw) is None
