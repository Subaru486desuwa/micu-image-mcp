"""model 路由 / 端点选择 / grok aspect-ratio 映射。"""
from __future__ import annotations

import pytest

import server as S


# ---------- _is_grok_model ----------

class TestIsGrokModel:
    def test_none(self):
        assert S._is_grok_model(None) is False

    def test_empty(self):
        assert S._is_grok_model("") is False

    def test_image2_models(self):
        assert S._is_grok_model("gpt-image-2") is False
        assert S._is_grok_model("gpt-image-2-pro") is False
        assert S._is_grok_model("GPT-Image-2") is False

    def test_grok_aliases(self):
        for m in S.GROK_AVAILABLE_MODELS:
            assert S._is_grok_model(m) is True

    def test_grok_prefix_unlisted(self):
        # 凡是 grok- 前缀都视为 grok（前向兼容新模型）
        assert S._is_grok_model("grok-imagine-image-future-v9") is True

    def test_case_insensitive(self):
        assert S._is_grok_model("GROK-IMAGINE-IMAGE-LITE") is True
        assert S._is_grok_model("Grok-Imagine-Image") is True

    def test_whitespace_handled(self):
        assert S._is_grok_model("  grok-imagine-image  ") is True


# ---------- _resolve_model ----------

class TestResolveModel:
    def test_default_1k_stays_nonpro(self):
        # 1K 走默认 model
        model, notes = S._resolve_model(None, "1024x1024")
        assert "pro" not in model.lower()
        assert notes == []

    def test_2k_auto_pro(self):
        model, notes = S._resolve_model("gpt-image-2", "2048x2048")
        assert model == S.PRO_MODEL
        assert any("pro" in n.lower() for n in notes)

    def test_4k_auto_pro(self):
        model, notes = S._resolve_model("gpt-image-2", "3840x2160")
        assert model == S.PRO_MODEL
        assert any("4k" in n.lower() for n in notes)

    def test_2k_explicit_pro_no_note(self):
        model, notes = S._resolve_model("gpt-image-2-pro", "2048x2048")
        assert model == "gpt-image-2-pro"
        assert notes == []  # 没切换就没提示

    def test_grok_bypasses_pro_gate(self):
        # grok 路径不套用 pro 锁
        model, notes = S._resolve_model("grok-imagine-image-lite", "2048x2048")
        assert model == "grok-imagine-image-lite"
        assert notes == []

    def test_small_size_stays_default(self):
        model, notes = S._resolve_model(None, "256x256")
        assert model == S.DEFAULT_MODEL


# ---------- _bypass_edits ----------

class TestBypassEdits:
    def test_nonpro_1k_uses_edits(self):
        # 非 pro + 小图 → 用 /v1/images/edits
        assert S._bypass_edits("gpt-image-2", "1024x1024") is False

    def test_pro_1k_uses_edits(self):
        # pro + 1K（< HIGH_RES_EDGE 1600）→ 仍可走 edits
        assert S._bypass_edits("gpt-image-2-pro", "1024x1024") is False

    def test_pro_2k_bypasses(self):
        # pro + 2K → 必须绕开 edits
        assert S._bypass_edits("gpt-image-2-pro", "2048x2048") is True

    def test_pro_4k_bypasses(self):
        assert S._bypass_edits("gpt-image-2-pro", "3840x2160") is True

    def test_boundary_at_HIGH_RES_EDGE(self):
        # max edge == 1600 已视为 ≥1600
        assert S._bypass_edits("gpt-image-2-pro", "1600x1600") is True
        assert S._bypass_edits("gpt-image-2-pro", "1599x1599") is False


# ---------- _reject_4k_with_reference ----------

class TestReject4kWithReference:
    def test_1k_ok(self):
        assert S._reject_4k_with_reference("1024x1024", "image_edit") is None

    def test_2k_ok(self):
        assert S._reject_4k_with_reference("2048x2048", "image_edit") is None

    def test_4k_rejected(self):
        msg = S._reject_4k_with_reference("3840x2160", "image_edit")
        assert msg is not None
        assert "image_edit" in msg
        assert "4K" in msg

    def test_includes_alternate_suggestion(self):
        msg = S._reject_4k_with_reference("3840x2160", "image_multi_reference")
        assert msg is not None
        assert "2048x1152" in msg or "1152x2048" in msg or "2048x2048" in msg


# ---------- _grok_aspect_ratio ----------

class TestGrokAspectRatio:
    @pytest.mark.parametrize("size,expected", [
        ("1024x1024", "1:1"),
        ("2048x2048", "1:1"),
        ("1536x1024", "3:2"),
        ("1024x1536", "2:3"),
        ("3840x2160", "16:9"),
        ("2160x3840", "9:16"),
    ])
    def test_common_ratios(self, size, expected):
        assert S._grok_aspect_ratio(size) == expected

    def test_invalid_falls_back_1_1(self):
        assert S._grok_aspect_ratio("invalid") == "1:1"

    def test_extreme_ratio_picks_closest(self):
        # 20:9 ≈ 2.22；19.5:9 ≈ 2.17；2:1 = 2.0
        r = S._grok_aspect_ratio("2000x1000")  # 真比 2.0
        assert r in {"2:1", "19.5:9", "20:9"}  # 最近的几个候选都接受


# ---------- _grok_resolution ----------

class TestGrokResolution:
    @pytest.mark.parametrize("size,res", [
        ("1024x1024", "1k"),
        ("1599x900", "1k"),
        ("1600x1600", "2k"),
        ("2048x2048", "2k"),
        ("3840x2160", "2k"),  # 4K 也映射到 2k（米醋后端没有 4K 档）
    ])
    def test_resolution_mapping(self, size, res):
        assert S._grok_resolution(size) == res
