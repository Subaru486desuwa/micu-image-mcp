"""_validate_image_bytes / _detect_actual_size / _png_color_type / _validate_mask / _size_note。"""
from __future__ import annotations

import struct
import zlib

import pytest

import server as S


# ---------- helpers: 构造最小合法 PNG / JPEG / WebP ----------

def _make_png(width: int, height: int, color_type: int = 6) -> bytes:
    """生成最小可解析 PNG (8B sig + IHDR + IDAT + IEND)。

    color_type:
      0 = 灰度, 2 = RGB, 3 = palette, 4 = GA, 6 = RGBA
    """
    sig = b"\x89PNG\r\n\x1a\n"
    # IHDR: width(4) height(4) bit_depth(1) color_type(1) compression(1) filter(1) interlace(1)
    ihdr_data = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    ihdr_crc = zlib.crc32(b"IHDR" + ihdr_data)
    ihdr = struct.pack(">I", 13) + b"IHDR" + ihdr_data + struct.pack(">I", ihdr_crc)
    # 最小 IDAT（空压缩流就够，server 只读 header）
    idat_data = zlib.compress(b"")
    idat_crc = zlib.crc32(b"IDAT" + idat_data)
    idat = struct.pack(">I", len(idat_data)) + b"IDAT" + idat_data + struct.pack(">I", idat_crc)
    iend_crc = zlib.crc32(b"IEND")
    iend = struct.pack(">I", 0) + b"IEND" + struct.pack(">I", iend_crc)
    return sig + ihdr + idat + iend


def _make_jpeg(width: int, height: int) -> bytes:
    """最小 JPEG header（SOI + APP0 + SOF0）；server 只读 SOFn 不需要完整码流。"""
    soi = b"\xff\xd8\xff"
    app0 = b"\xe0" + struct.pack(">H", 16) + b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
    # SOF0: marker FFC0 + len(2) + precision(1) + h(2) + w(2) + comps(1)
    sof = b"\xff\xc0" + struct.pack(">H", 11) + b"\x08" + struct.pack(">HH", height, width) + b"\x03\x01\x22\x00\x02\x11\x01\x03\x11\x01"
    # 加一些字节凑过 24 byte 下限
    tail = b"\xff\xd9"
    return soi + app0 + sof + tail


def _make_webp_vp8x(width: int, height: int) -> bytes:
    """生成 VP8X WebP（支持 alpha / animation 的扩展 chunk）。"""
    # WebP 格式: 'RIFF' + size(4LE) + 'WEBP' + chunk
    # VP8X chunk: 'VP8X' + size(4LE=10) + flags(1) + reserved(3) + width-1(3LE) + height-1(3LE)
    w_minus_1 = width - 1
    h_minus_1 = height - 1
    vp8x_payload = (
        b"\x00\x00\x00\x00"  # flags + reserved
        + bytes([w_minus_1 & 0xFF, (w_minus_1 >> 8) & 0xFF, (w_minus_1 >> 16) & 0xFF])
        + bytes([h_minus_1 & 0xFF, (h_minus_1 >> 8) & 0xFF, (h_minus_1 >> 16) & 0xFF])
    )
    vp8x = b"VP8X" + struct.pack("<I", 10) + vp8x_payload
    riff_payload = b"WEBP" + vp8x
    return b"RIFF" + struct.pack("<I", len(riff_payload)) + riff_payload


# ---------- _validate_image_bytes ----------

class TestValidateImageBytes:
    def test_png(self):
        assert S._validate_image_bytes(_make_png(64, 64)) is None

    def test_jpeg(self):
        assert S._validate_image_bytes(_make_jpeg(64, 64)) is None

    def test_webp(self):
        assert S._validate_image_bytes(_make_webp_vp8x(64, 64)) is None

    def test_gif87a(self):
        # GIF87a + 至少 16 字节
        assert S._validate_image_bytes(b"GIF87a" + b"\x00" * 20) is None

    def test_gif89a(self):
        assert S._validate_image_bytes(b"GIF89a" + b"\x00" * 20) is None

    def test_empty(self):
        msg = S._validate_image_bytes(b"")
        assert msg is not None and "太小" in msg

    def test_too_small(self):
        msg = S._validate_image_bytes(b"abc")
        assert msg is not None and "太小" in msg

    def test_garbage(self):
        msg = S._validate_image_bytes(b"\x00" * 32)
        assert msg is not None and "magic 不匹配" in msg
        # PATH-2 回归：错误信息不得回显文件原始字节
        assert "\\x00" not in msg and repr(b"\x00" * 16) not in msg

    def test_label_in_error(self):
        msg = S._validate_image_bytes(b"", label="mask_path")
        assert msg is not None and "mask_path" in msg


# ---------- _detect_actual_size ----------

class TestDetectActualSize:
    def test_png(self):
        assert S._detect_actual_size(_make_png(800, 600)) == (800, 600)

    def test_png_square(self):
        assert S._detect_actual_size(_make_png(1024, 1024)) == (1024, 1024)

    def test_jpeg(self):
        assert S._detect_actual_size(_make_jpeg(1920, 1080)) == (1920, 1080)

    def test_webp_vp8x(self):
        assert S._detect_actual_size(_make_webp_vp8x(2048, 1152)) == (2048, 1152)

    def test_too_short(self):
        assert S._detect_actual_size(b"\x89PNG\r\n\x1a\n") is None

    def test_unknown_format(self):
        assert S._detect_actual_size(b"\x00" * 100) is None


# ---------- _png_color_type ----------

class TestPngColorType:
    @pytest.mark.parametrize("ct", [0, 2, 3, 4, 6])
    def test_each_color_type(self, ct):
        assert S._png_color_type(_make_png(32, 32, color_type=ct)) == ct

    def test_non_png(self):
        assert S._png_color_type(_make_jpeg(32, 32)) is None

    def test_too_short(self):
        assert S._png_color_type(b"\x89PNG") is None


# ---------- _validate_mask_against_image ----------

class TestValidateMaskAgainstImage:
    def test_valid_rgba(self):
        # RGBA mask (color_type=6) 与原图同尺寸
        mask = _make_png(1024, 1024, color_type=6)
        assert S._validate_mask_against_image(mask, (1024, 1024)) is None

    def test_valid_ga(self):
        # 灰度+alpha (color_type=4) 也接受
        mask = _make_png(1024, 1024, color_type=4)
        assert S._validate_mask_against_image(mask, (1024, 1024)) is None

    def test_wrong_size(self):
        mask = _make_png(512, 512, color_type=6)
        msg = S._validate_mask_against_image(mask, (1024, 1024))
        assert msg is not None and "尺寸" in msg

    def test_no_alpha_rgb_rejected(self):
        # color_type=2 (RGB) 没 alpha
        mask = _make_png(1024, 1024, color_type=2)
        msg = S._validate_mask_against_image(mask, (1024, 1024))
        assert msg is not None and "alpha" in msg

    def test_grayscale_rejected(self):
        mask = _make_png(1024, 1024, color_type=0)
        msg = S._validate_mask_against_image(mask, (1024, 1024))
        assert msg is not None

    def test_non_png_rejected(self):
        mask = _make_jpeg(1024, 1024)
        msg = S._validate_mask_against_image(mask, (1024, 1024))
        assert msg is not None and "PNG" in msg


# ---------- _size_note ----------

class TestSizeNote:
    def test_exact_match_no_note(self):
        assert S._size_note("1024x1024", (1024, 1024)) is None

    def test_no_actual_no_note(self):
        assert S._size_note("1024x1024", None) is None

    def test_compressed_to_157mp(self):
        # 1920x1080 (2.07MP) 被压到 1671x939 (1.57MP)
        # 1920x1080 是 2.07MP ≤ 2.25MP，actual 1.57MP → 走"压到 1.57MP"分支
        msg = S._size_note("1920x1080", (1671, 939))
        assert msg is not None
        assert "1.57" in msg or "福利档" in msg

    def test_upscaled_to_157mp(self):
        # 1024x1024 (1.05MP) 被放大到 ~1.57MP
        msg = S._size_note("1024x1024", (1254, 1254))
        assert msg is not None
        # 1.05MP < 1.57MP 触发"放大"提示
        assert "放大" in msg or "ℹ" in msg

    def test_generic_mismatch(self):
        # 高 MP 请求 + 实际不匹配 → 一般提示
        msg = S._size_note("3840x2160", (2048, 1152))
        assert msg is not None
        assert "≠" in msg or "不" in msg

    def test_invalid_requested(self):
        # 解析不出 requested → 不提示
        assert S._size_note("invalid", (1024, 1024)) is None
