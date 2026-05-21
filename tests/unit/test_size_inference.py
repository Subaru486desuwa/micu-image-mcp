"""_infer_size_from_prompt 关键字推断。"""
from __future__ import annotations

import pytest

import server as S


class TestExplicitPixels:
    def test_basic(self):
        result = S._infer_size_from_prompt("画一张 1920x1080 的图")
        assert result is not None
        size, _ = result
        # 1920 / 1080 都是 8 倍数 → 不会改
        assert size == "1920x1080"

    def test_unicode_x(self):
        result = S._infer_size_from_prompt("3840×2160 高清")
        assert result is not None
        size, _ = result
        assert size == "3840x2160"

    def test_align_to_8(self):
        # 1500 → 1504
        result = S._infer_size_from_prompt("make a 1500x1500 logo")
        assert result is not None
        size, reason = result
        assert size == "1504x1504"
        assert "对齐 8" in reason


class TestKKeywords:
    def test_4k_horizontal_default(self):
        result = S._infer_size_from_prompt("a cyberpunk Tokyo 4K wallpaper")
        # wallpaper 含 horizontal_kw，4k 优先
        assert result is not None
        size, _ = result
        assert size == "3840x2160"

    def test_4k_vertical(self):
        result = S._infer_size_from_prompt("4K vertical 海报")
        # poster_kw 也匹配但 4K 优先（且 vertical 走竖屏分支）
        assert result is not None
        size, _ = result
        assert size == "2160x3840"

    def test_4k_uhd(self):
        result = S._infer_size_from_prompt("UHD landscape photograph")
        assert result is not None
        assert result[0] == "3840x2160"

    def test_2k_horizontal(self):
        result = S._infer_size_from_prompt("1080p movie still")
        assert result is not None
        assert result[0] == "2048x1152"

    def test_2k_vertical(self):
        result = S._infer_size_from_prompt("FullHD vertical phone wallpaper")
        assert result is not None
        assert result[0] == "1152x2048"

    def test_720p(self):
        result = S._infer_size_from_prompt("720p banner")
        assert result is not None
        assert result[0] == "1280x720"


class TestShapeKeywords:
    def test_square_logo(self):
        result = S._infer_size_from_prompt("a minimalist logo")
        assert result is not None
        assert result[0] == "1024x1024"

    def test_poster(self):
        result = S._infer_size_from_prompt("a movie poster")
        assert result is not None
        assert result[0] == "1024x1536"

    def test_photo32(self):
        result = S._infer_size_from_prompt("a photograph of a cat")
        assert result is not None
        assert result[0] == "1536x1024"

    def test_chinese_vertical(self):
        result = S._infer_size_from_prompt("一张竖屏图")
        assert result is not None
        assert result[0] == "1024x1536"

    def test_chinese_horizontal(self):
        result = S._infer_size_from_prompt("做一个横屏的壁纸")
        # 壁纸 matches horizontal_kw without K 关键字 → 1K 横屏
        assert result is not None
        assert result[0] == "1536x1024"


class TestNoMatch:
    def test_no_keywords(self):
        # "a red apple" 没有任何尺寸/形状关键字
        result = S._infer_size_from_prompt("a red apple on white")
        assert result is None

    def test_empty(self):
        assert S._infer_size_from_prompt("") is None


class TestPriority:
    def test_explicit_pixels_beat_kkw(self):
        # 同时含 1024x512 和 4K，优先用明确像素
        result = S._infer_size_from_prompt("4K landscape 1024x512 layout")
        assert result is not None
        assert result[0] == "1024x512"

    def test_2k_beats_horizontal_default(self):
        # 2K 关键字优先于"horizontal-only 默认 1536x1024"
        result = S._infer_size_from_prompt("2K landscape banner")
        assert result is not None
        assert result[0] == "2048x1152"
