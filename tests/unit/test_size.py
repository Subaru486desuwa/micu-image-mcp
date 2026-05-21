"""size 校验 / 解析 / 档位划分相关纯函数。"""
from __future__ import annotations

import pytest

import server as S


# ---------- _parse_size ----------

class TestParseSize:
    def test_basic_lowercase(self):
        assert S._parse_size("1024x1024") == (1024, 1024)

    def test_basic_uppercase_x(self):
        # _parse_size 会先 strip().lower()，X 也通过
        assert S._parse_size("1024X1024") == (1024, 1024)

    def test_whitespace(self):
        assert S._parse_size("  2048x1152  ") == (2048, 1152)

    def test_rectangular(self):
        assert S._parse_size("1280x720") == (1280, 720)

    def test_invalid_format(self):
        assert S._parse_size("1024*1024") is None
        assert S._parse_size("1024") is None
        assert S._parse_size("") is None
        assert S._parse_size("abc") is None

    def test_extra_suffix(self):
        # 严格 ^...$ 匹配
        assert S._parse_size("1024x1024px") is None


# ---------- _max_edge ----------

class TestMaxEdge:
    @pytest.mark.parametrize("size,expected", [
        ("1024x1024", 1024),
        ("3840x2160", 3840),
        ("2160x3840", 3840),
        ("1280x720", 1280),
        ("720x1280", 1280),
    ])
    def test_basic(self, size, expected):
        assert S._max_edge(size) == expected

    def test_invalid_returns_zero(self):
        assert S._max_edge("invalid") == 0
        assert S._max_edge("") == 0


# ---------- _size_tier ----------

class TestSizeTier:
    @pytest.mark.parametrize("size,tier", [
        ("invalid", "unknown"),
        ("256x256", "small"),     # < 1024
        ("512x512", "small"),
        ("1023x1023", "small"),
        ("1024x1024", "1k"),      # [1024, 1600)
        ("1280x720", "1k"),
        ("1536x1024", "1k"),
        ("1599x1599", "1k"),
        ("1600x1600", "2k"),      # [1600, 3000)
        ("2048x2048", "2k"),
        ("2048x1152", "2k"),
        ("2999x2999", "2k"),
        ("3000x3000", "4k"),      # >= 3000
        ("3840x2160", "4k"),
        ("2160x3840", "4k"),
    ])
    def test_tiers(self, size, tier):
        assert S._size_tier(size) == tier


# ---------- _validate_size ----------

class TestValidateSize:
    def test_none_allowed(self):
        cleaned, err = S._validate_size(None, allow_none=True)
        assert cleaned is None
        assert err is None

    def test_none_rejected(self):
        cleaned, err = S._validate_size(None, allow_none=False)
        assert cleaned is None
        assert err is not None and "不能为 None" in err

    def test_non_string(self):
        cleaned, err = S._validate_size(1024)  # type: ignore[arg-type]
        assert cleaned is None
        assert err is not None and "必须是字符串" in err

    def test_bad_format(self):
        cleaned, err = S._validate_size("1024-1024")
        assert cleaned is None
        assert err is not None and "格式错误" in err

    def test_too_small(self):
        cleaned, err = S._validate_size("128x128")
        assert cleaned is None
        assert err is not None and "太小" in err

    def test_too_large(self):
        cleaned, err = S._validate_size("4104x4104")
        assert cleaned is None
        assert err is not None and "太大" in err

    def test_not_aligned_to_8(self):
        # 1500 不是 8 的倍数
        cleaned, err = S._validate_size("1500x1500")
        assert cleaned is None
        assert err is not None and "8" in err

    def test_valid_normalized(self):
        cleaned, err = S._validate_size("  1024X1024  ")
        assert cleaned == "1024x1024"
        assert err is None

    def test_boundary_valid(self):
        # 256 / 4096 都在范围内
        for size in ("256x256", "4096x4096", "256x4096"):
            cleaned, err = S._validate_size(size)
            assert err is None, f"{size}: {err}"
            assert cleaned == size

    def test_common_pro_sizes(self):
        for size in ("2048x2048", "2048x1152", "3840x2160", "2160x3840"):
            cleaned, err = S._validate_size(size)
            assert err is None
            assert cleaned == size


# ---------- _validate_grok_size ----------

class TestValidateGrokSize:
    def test_none_allowed(self):
        cleaned, err = S._validate_grok_size(None, allow_none=True)
        assert cleaned is None
        assert err is None

    def test_no_8_multiple_restriction(self):
        # grok 不要求 8 倍数
        cleaned, err = S._validate_grok_size("1501x1001")
        assert err is None
        assert cleaned == "1501x1001"

    def test_no_4k_limit(self):
        # grok 不强制 <=4096
        cleaned, err = S._validate_grok_size("8000x4500")
        assert err is None
        assert cleaned == "8000x4500"

    def test_bad_format(self):
        cleaned, err = S._validate_grok_size("not_a_size")
        assert cleaned is None
        assert err is not None and "格式错误" in err

    def test_negative_rejected(self):
        # 正则只匹配 \d+，"-100" 在 \d+ 模式下被拒
        cleaned, err = S._validate_grok_size("-100x100")
        assert cleaned is None
        assert err is not None


# ---------- _validate_n ----------

class TestValidateN:
    def test_normal_values(self):
        for n in range(1, S.MAX_N + 1):
            assert S._validate_n(n) is None

    def test_zero_rejected(self):
        err = S._validate_n(0)
        assert err is not None and "≥ 1" in err

    def test_negative_rejected(self):
        assert S._validate_n(-1) is not None

    def test_over_max(self):
        err = S._validate_n(S.MAX_N + 1)
        assert err is not None and "burn quota" in err

    def test_float_rejected(self):
        assert S._validate_n(1.5) is not None  # type: ignore[arg-type]

    def test_string_rejected(self):
        assert S._validate_n("1") is not None  # type: ignore[arg-type]

    def test_bool_rejected(self):
        # isinstance(True, int) 是 True，但 bool 应该单独拒掉
        assert S._validate_n(True) is not None  # type: ignore[arg-type]


# ---------- _round_to_alignment ----------

class TestRoundToAlignment:
    @pytest.mark.parametrize("inp,out", [
        (1080, 1080),   # 已对齐 8
        (1500, 1504),   # round(1500/8)=187.5 → 188 * 8 = 1504
        (720, 720),
        (1, 16),        # 下限 16
        (0, 16),
        (15, 16),
        (1024, 1024),
        (3840, 3840),
    ])
    def test_alignment(self, inp, out):
        assert S._round_to_alignment(inp) == out


# ---------- _parse_actual ----------

class TestParseActual:
    def test_basic(self):
        assert S._parse_actual("1024x1024") == (1024, 1024)

    def test_none(self):
        assert S._parse_actual(None) is None

    def test_empty(self):
        assert S._parse_actual("") is None

    def test_invalid(self):
        assert S._parse_actual("WxH") is None
