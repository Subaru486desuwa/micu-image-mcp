"""响应 JSON 解析 + 图片 payload 提取（b64 / url）。"""
from __future__ import annotations

import json
import re


def _parse_response(text: str) -> dict | str:
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        return text


def _error_detail(text: str) -> str:
    try:
        j = json.loads(text)
        if isinstance(j, dict):
            err = j.get("error")
            if isinstance(err, dict) and err.get("message"):
                return str(err["message"])[:400]
            if j.get("message"):
                return str(j["message"])[:400]
        return text[:400]
    except Exception:  # noqa: BLE001
        return (text or "")[:400]


def _extract_image_payload(resp: dict | str) -> tuple[str | None, str | None]:
    """从米醋响应里提取 (b64, url)；二者至少有一个。"""
    if isinstance(resp, str):
        return None, None
    # /v1/images/generations & /v1/images/edits 标准格式
    data = resp.get("data") if isinstance(resp, dict) else None
    if isinstance(data, list) and data:
        item = data[0]
        if isinstance(item, dict):
            if item.get("b64_json"):
                return item["b64_json"], None
            if item.get("url"):
                return None, item["url"]
    # /v1/chat/completions fallback：图嵌在 markdown ![](url) 或 base64
    choices = resp.get("choices") if isinstance(resp, dict) else None
    if isinstance(choices, list) and choices:
        msg = (choices[0] or {}).get("message", {})
        content = msg.get("content")
        if isinstance(content, str):
            m = re.search(r"!\[[^\]]*\]\((data:image/[^;]+;base64,([A-Za-z0-9+/=\s]+))\)", content)
            if m:
                return m.group(2).strip(), None
            m = re.search(r"!\[[^\]]*\]\((https?://[^)]+)\)", content)
            if m:
                return None, m.group(1)
            m = re.search(r"\b(https?://\S+\.(?:png|jpe?g|webp|gif))\b", content, re.I)
            if m:
                return None, m.group(1)
    return None, None


def _extract_image_payloads(resp: dict | str) -> list[tuple[str | None, str | None]]:
    """提取一组图像 payload。Grok 批量生成时会返回多张。"""
    if isinstance(resp, str):
        return []
    data = resp.get("data") if isinstance(resp, dict) else None
    if isinstance(data, list) and data:
        payloads: list[tuple[str | None, str | None]] = []
        for item in data:
            if not isinstance(item, dict):
                continue
            if item.get("b64_json"):
                payloads.append((str(item["b64_json"]), None))
            elif item.get("url"):
                payloads.append((None, str(item["url"])))
        if payloads:
            return payloads
    b64, url = _extract_image_payload(resp)
    if b64 or url:
        return [(b64, url)]
    return []


__all__ = [
    "_parse_response", "_error_detail",
    "_extract_image_payload", "_extract_image_payloads",
]
