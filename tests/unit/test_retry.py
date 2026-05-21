"""retry 退避策略 + 响应解析 + Retry-After header 解析。"""
from __future__ import annotations

from email.utils import format_datetime
from datetime import datetime, timedelta, timezone

import pytest

import server as S


# ---------- _parse_retry_after ----------

class TestParseRetryAfter:
    def test_empty_headers(self):
        assert S._parse_retry_after({}) is None

    def test_missing_header(self):
        assert S._parse_retry_after({"x-other": "1"}) is None

    def test_integer_seconds(self):
        assert S._parse_retry_after({"retry-after": "5"}) == 5.0

    def test_float_seconds(self):
        assert S._parse_retry_after({"retry-after": "0.5"}) == 0.5

    def test_negative_zero(self):
        # 负数 → 0
        assert S._parse_retry_after({"retry-after": "-3"}) == 0.0

    def test_zero(self):
        assert S._parse_retry_after({"retry-after": "0"}) == 0.0

    def test_clamped_to_max(self):
        # 超大值会被夹到 MAX_RETRY_AFTER_SECONDS
        large = str(int(S.MAX_RETRY_AFTER_SECONDS * 10))
        assert S._parse_retry_after({"retry-after": large}) == S.MAX_RETRY_AFTER_SECONDS

    def test_http_date_future(self):
        # 未来 30s 的 HTTP-date
        future = datetime.now(timezone.utc) + timedelta(seconds=30)
        header = format_datetime(future, usegmt=True)
        result = S._parse_retry_after({"retry-after": header})
        assert result is not None
        # 30s ± 5s 容差
        assert 25 <= result <= 35

    def test_invalid_string(self):
        assert S._parse_retry_after({"retry-after": "not a number"}) is None


# ---------- _retry_delay ----------

class TestRetryDelay:
    def test_non_retryable_status(self):
        # 400 / 401 / 404 不在 RETRYABLE_STATUS
        for status in (400, 401, 403, 404):
            assert S._retry_delay(status, {}, attempt_index=0, big_size_lock=False) is None

    def test_524_fail_fast_with_big_size_lock(self):
        # 524 在 BIG_SIZE_FAIL_FAST_STATUS，big_size_lock=True 直接放弃
        assert S._retry_delay(524, {}, attempt_index=0, big_size_lock=True) is None

    def test_524_retries_for_small(self):
        # 524 对小图仍可重试
        delay = S._retry_delay(524, {}, attempt_index=0, big_size_lock=False)
        assert delay is not None
        assert delay > 0

    def test_big_size_only_one_retry(self):
        # big_size_lock 路径只允许一次重试，attempt_index >= 1 拒
        assert S._retry_delay(503, {}, attempt_index=0, big_size_lock=True) == S.BIG_RETRY_DELAY_SECONDS
        assert S._retry_delay(503, {}, attempt_index=1, big_size_lock=True) is None

    def test_small_path_exhausts_after_n_attempts(self):
        # 小图 SMALL_RETRY_DELAYS_SECONDS 长度 = 重试上限
        n = len(S.SMALL_RETRY_DELAYS_SECONDS)
        # 0..n-1 都能拿到 delay
        for i in range(n):
            assert S._retry_delay(500, {}, attempt_index=i, big_size_lock=False) is not None
        # 第 n 次拒绝
        assert S._retry_delay(500, {}, attempt_index=n, big_size_lock=False) is None

    def test_retry_after_header_overrides_default(self):
        # 429 在 RETRY_AFTER_STATUSES，header 中 retry-after 应被尊重
        delay = S._retry_delay(429, {"retry-after": "7"}, attempt_index=0, big_size_lock=False)
        assert delay == 7.0

    def test_retry_after_header_clamped(self):
        delay = S._retry_delay(429, {"retry-after": "9999"}, attempt_index=0, big_size_lock=False)
        assert delay == S.MAX_RETRY_AFTER_SECONDS

    def test_524_ignores_retry_after_header(self):
        # 524 不在 RETRY_AFTER_STATUSES，header 不生效，用默认退避
        delay = S._retry_delay(524, {"retry-after": "5"}, attempt_index=0, big_size_lock=False)
        # 应该是 SMALL_RETRY_DELAYS_SECONDS[0] + jitter，不是 5
        assert delay != 5.0
        assert delay is not None
        assert S.SMALL_RETRY_DELAYS_SECONDS[0] <= delay <= S.SMALL_RETRY_DELAYS_SECONDS[0] + S.RETRY_JITTER_SECONDS

    def test_network_error_status_zero(self):
        # status=0 是网络层异常，也在 RETRYABLE_STATUS
        delay = S._retry_delay(0, {}, attempt_index=0, big_size_lock=False)
        assert delay is not None


# ---------- _parse_response ----------

class TestParseResponse:
    def test_json_dict(self):
        result = S._parse_response('{"key": "value"}')
        assert result == {"key": "value"}

    def test_json_array(self):
        # _parse_response 把 list 也能解析（json.loads 返回 list）；它的返回类型 dict|str 实际更宽容
        result = S._parse_response('[1, 2, 3]')
        assert result == [1, 2, 3] or isinstance(result, str)

    def test_plain_text(self):
        result = S._parse_response("plain not json")
        assert result == "plain not json"

    def test_empty_string(self):
        # json.loads("") 抛 → fallback 到原字符串
        assert S._parse_response("") == ""


# ---------- _error_detail ----------

class TestErrorDetail:
    def test_openai_style_error(self):
        text = '{"error": {"message": "invalid api key", "type": "auth"}}'
        assert S._error_detail(text) == "invalid api key"

    def test_root_message(self):
        text = '{"message": "rate limit"}'
        assert S._error_detail(text) == "rate limit"

    def test_plain_text_truncated(self):
        text = "x" * 600
        result = S._error_detail(text)
        assert len(result) <= 400

    def test_empty(self):
        assert S._error_detail("") == ""

    def test_unparseable_json(self):
        text = "Internal Server Error (HTML page)"
        assert S._error_detail(text) == text

    def test_truncates_long_openai_error(self):
        text = '{"error": {"message": "' + "x" * 600 + '"}}'
        result = S._error_detail(text)
        assert len(result) <= 400


# ---------- _extract_image_payload ----------

class TestExtractImagePayload:
    def test_b64_in_data(self):
        b64, url = S._extract_image_payload({"data": [{"b64_json": "AAAA"}]})
        assert b64 == "AAAA"
        assert url is None

    def test_url_in_data(self):
        b64, url = S._extract_image_payload({"data": [{"url": "https://x.com/y.png"}]})
        assert b64 is None
        assert url == "https://x.com/y.png"

    def test_chat_markdown_url(self):
        resp = {
            "choices": [{"message": {"content": "Here you go ![img](https://x.com/y.png)"}}]
        }
        b64, url = S._extract_image_payload(resp)
        assert url == "https://x.com/y.png"

    def test_chat_markdown_b64(self):
        resp = {
            "choices": [{"message": {"content": "![](data:image/png;base64,AAAABBBB)"}}]
        }
        b64, url = S._extract_image_payload(resp)
        assert b64 == "AAAABBBB"

    def test_chat_bare_url(self):
        resp = {
            "choices": [{"message": {"content": "see https://x.com/foo.png for details"}}]
        }
        b64, url = S._extract_image_payload(resp)
        assert url == "https://x.com/foo.png"

    def test_empty_response(self):
        b64, url = S._extract_image_payload({})
        assert b64 is None and url is None

    def test_string_response(self):
        # 米醋偶尔返回纯字符串（错误页等）→ 不抛
        b64, url = S._extract_image_payload("garbage")
        assert b64 is None and url is None


# ---------- _extract_image_payloads (复数) ----------

class TestExtractImagePayloads:
    def test_multi_b64(self):
        resp = {"data": [{"b64_json": "A"}, {"b64_json": "B"}, {"url": "u.png"}]}
        payloads = S._extract_image_payloads(resp)
        assert len(payloads) == 3
        assert payloads[0] == ("A", None)
        assert payloads[1] == ("B", None)
        assert payloads[2] == (None, "u.png")

    def test_single_chat_fallback(self):
        # 没 data，走 chat fallback → 返回单元素 list
        resp = {"choices": [{"message": {"content": "![](https://x/y.png)"}}]}
        payloads = S._extract_image_payloads(resp)
        assert payloads == [(None, "https://x/y.png")]

    def test_empty(self):
        assert S._extract_image_payloads({}) == []

    def test_string(self):
        assert S._extract_image_payloads("garbage") == []
