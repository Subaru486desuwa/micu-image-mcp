"""_safe_basename / _resolve_save_dir 安全约束。"""
from __future__ import annotations

import os
from pathlib import Path

import pytest

import server as S


# ---------- _safe_basename ----------

class TestSafeBasename:
    def test_none(self):
        assert S._safe_basename(None) is None

    def test_empty(self):
        assert S._safe_basename("") is None

    def test_whitespace_only(self):
        assert S._safe_basename("   ") is None

    def test_simple_name(self):
        assert S._safe_basename("my_image") == "my_image"

    def test_with_dot(self):
        assert S._safe_basename("v1.0_draft") == "v1.0_draft"

    def test_with_dash_underscore(self):
        assert S._safe_basename("a-b_c.d") == "a-b_c.d"

    def test_path_separator_rejected(self):
        assert S._safe_basename("a/b") is None
        assert S._safe_basename("a\\b") is None
        assert S._safe_basename("/abs/path") is None
        assert S._safe_basename("../etc/passwd") is None

    def test_dotdot_rejected(self):
        assert S._safe_basename("..") is None
        assert S._safe_basename("..hidden") is None

    def test_leading_dot_rejected(self):
        # 隐藏文件名
        assert S._safe_basename(".hidden") is None

    def test_chinese_rejected(self):
        # _SAFE_BASENAME_RE 只允许 ASCII 字母数字 _-.
        assert S._safe_basename("中文") is None

    def test_space_rejected(self):
        assert S._safe_basename("file name") is None

    def test_too_long(self):
        assert S._safe_basename("a" * 101) is None
        assert S._safe_basename("a" * 100) == "a" * 100  # 边界 100 OK

    def test_non_string(self):
        assert S._safe_basename(123) is None  # type: ignore[arg-type]


# ---------- _resolve_save_dir ----------

class TestResolveSaveDir:
    def test_none_returns_default(self):
        path, err = S._resolve_save_dir(None)
        assert err is None
        assert path is not None
        # 默认应在 _SAVE_ROOT 之内或就是 _SAVE_ROOT
        try:
            path.relative_to(S._SAVE_ROOT)
        except ValueError:
            assert path == S._SAVE_ROOT

    def test_within_root(self):
        # 在 _SAVE_ROOT 下创建子目录 → 接受
        sub = S._SAVE_ROOT / "subdir-test"
        path, err = S._resolve_save_dir(str(sub))
        assert err is None
        assert path is not None
        assert path == sub.resolve()

    def test_outside_root_rejected(self):
        # 试着指到 /tmp 之外完全无关的路径
        path, err = S._resolve_save_dir("/usr")
        assert path is None
        assert err is not None
        assert "安全根目录" in err

    def test_relative_outside_root_rejected(self, tmp_path):
        # 用 tmp_path（pytest fixture）作为完全独立的目录测试拒绝
        path, err = S._resolve_save_dir(str(tmp_path))
        # tmp_path 在 /tmp 下，可能与 _SAVE_ROOT 同根。检查是否实际在 _SAVE_ROOT 内决定
        try:
            tmp_path.resolve().relative_to(S._SAVE_ROOT)
            # 若 tmp_path 恰好在 _SAVE_ROOT 内（很罕见），不应被拒
            assert err is None
        except ValueError:
            assert err is not None
            assert path is None

    def test_path_traversal_via_dotdot_rejected(self):
        attack = str(S._SAVE_ROOT / ".." / ".." / "etc")
        path, err = S._resolve_save_dir(attack)
        assert path is None
        assert err is not None
