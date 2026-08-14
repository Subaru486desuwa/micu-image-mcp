"""GPT Image 2 当前模型重定向与参数约束的离线回归矩阵。"""
from __future__ import annotations

import asyncio
import base64
import io
import json

import pytest
from PIL import Image

import server


def _png_bytes(size: tuple[int, int] = (32, 32)) -> bytes:
    buf = io.BytesIO()
    Image.new("RGB", size, (12, 34, 56)).save(buf, format="PNG")
    return buf.getvalue()


def _response_png(size: tuple[int, int] = (32, 32)) -> str:
    payload = base64.b64encode(_png_bytes(size)).decode()
    return json.dumps({"data": [{"b64_json": payload}]})


@pytest.mark.parametrize("model", ["gpt-image-2", "gpt-image-2-openai"])
def test_current_models_are_accepted_exactly(model):
    assert server._model_error(model) is None


@pytest.mark.parametrize(
    "model",
    ["gpt-image-2-pro", "gpt-image-2-key", " gpt-image-2 ", "dall-e-3"],
)
def test_legacy_private_and_whitespace_models_are_rejected(model):
    error = server._model_error(model)
    assert error is not None
    assert "gpt-image-2 / gpt-image-2-openai" in error


def test_removed_model_in_default_environment_is_rejected_before_network(monkeypatch):
    async def unexpected_call(*args, **kwargs):  # noqa: ANN002, ANN003, ARG001
        raise AssertionError("removed default model must not reach the network")

    monkeypatch.setattr(server, "DEFAULT_MODEL", "gpt-image-2-pro")
    monkeypatch.setattr(server, "_call_with_retry", unexpected_call)
    result = asyncio.run(
        server.image_generate(
            prompt="default model migration guard",
            size="1024x1024",
            api_key="sk-test",
        )
    )

    assert result["ok"] is False
    assert "gpt-image-2-openai" in result["error"]


@pytest.mark.parametrize(
    ("requested", "size", "expected"),
    [
        (None, "1024x1024", "gpt-image-2"),
        ("gpt-image-2", "1536x1024", "gpt-image-2"),
        (None, "2048x1152", "gpt-image-2-openai"),
        ("gpt-image-2", "2048x2048", "gpt-image-2-openai"),
        ("gpt-image-2", "3840x2160", "gpt-image-2-openai"),
        ("gpt-image-2-openai", "1024x1024", "gpt-image-2-openai"),
    ],
)
def test_model_routing_matrix(requested, size, expected):
    model, _notes = server._resolve_model(requested, size)
    assert model == expected


@pytest.mark.parametrize(
    "size",
    ["1024x1024", "1280x720", "1536x1024", "2048x1152", "2048x2048", "3840x2160"],
)
def test_current_size_matrix_accepts_documented_resolutions(size):
    cleaned, error = server._validate_size(size, allow_none=False)
    assert error is None
    assert cleaned == size


@pytest.mark.parametrize(
    "size",
    [
        "1920x1080",  # both edges must be multiples of 16
        "256x256",  # below the 655,360 pixel minimum
        "4096x2160",  # edge above 3,840 and total pixels above the maximum
        "3840x1024",  # aspect ratio above 3:1
        "3840x3840",  # total pixels above 8,294,400
    ],
)
def test_current_size_matrix_rejects_out_of_contract_resolutions(size):
    cleaned, error = server._validate_size(size, allow_none=False)
    assert cleaned is None
    assert error is not None


@pytest.mark.parametrize("quality", [None, "auto", "low", "medium", "high"])
@pytest.mark.parametrize("model", ["gpt-image-2", "gpt-image-2-openai"])
def test_generation_model_quality_payload_matrix(monkeypatch, model, quality):
    calls = []

    async def fake_call(endpoint, _key, *args, **kwargs):  # noqa: ANN001, ARG001
        calls.append(endpoint)
        return 200, _response_png()

    monkeypatch.setattr(server, "_call_with_retry", fake_call)
    result = asyncio.run(
        server.image_generate(
            prompt="parameter matrix",
            size="1024x1024",
            model=model,
            quality=quality,
            api_key="sk-test",
        )
    )

    assert result["ok"] is True, result
    assert result["model"] == model
    assert result["size_honored"] is False
    assert calls[0].json_body["model"] == model
    assert calls[0].json_body["size"] == "1024x1024"
    if quality is None:
        assert "quality" not in calls[0].json_body
    else:
        assert calls[0].json_body["quality"] == quality


@pytest.mark.parametrize("quality", ["ultra", "draft", 1, True])
def test_invalid_quality_is_rejected_before_network(monkeypatch, quality):
    async def unexpected_call(*args, **kwargs):  # noqa: ANN002, ANN003, ARG001
        raise AssertionError("invalid quality must not reach the network")

    monkeypatch.setattr(server, "_call_with_retry", unexpected_call)
    result = asyncio.run(
        server.image_generate(
            prompt="parameter matrix",
            size="1024x1024",
            quality=quality,  # type: ignore[arg-type]
            api_key="sk-test",
        )
    )

    assert result["ok"] is False
    assert "quality" in result["error"]


def test_generation_reports_exact_size_and_never_falls_back_to_chat(monkeypatch):
    calls = []

    async def successful_call(endpoint, _key, *args, **kwargs):  # noqa: ANN001, ARG001
        calls.append(endpoint)
        return 200, _response_png((1024, 1024))

    monkeypatch.setattr(server, "_call_with_retry", successful_call)
    result = asyncio.run(
        server.image_generate(
            prompt="exact size regression",
            size="1024x1024",
            model="gpt-image-2-openai",
            api_key="sk-test",
        )
    )

    assert result["ok"] is True, result
    assert result["size_honored"] is True
    assert result["used_fallback"] is False
    assert calls
    assert all(endpoint.url.endswith("/v1/images/generations") for endpoint in calls)


def test_generation_failure_never_sends_image_model_to_chat(monkeypatch):
    calls = []

    async def failing_call(endpoint, _key, *args, **kwargs):  # noqa: ANN001, ARG001
        calls.append(endpoint)
        return 503, '{"error":{"message":"upstream unavailable"}}'

    monkeypatch.setattr(server, "_call_with_retry", failing_call)
    result = asyncio.run(
        server.image_generate(
            prompt="do not reroute this image model",
            size="1024x1024",
            model="gpt-image-2-openai",
            api_key="sk-test",
        )
    )

    assert result["ok"] is False
    assert result["used_fallback"] is False
    assert calls
    assert all(endpoint.url.endswith("/v1/images/generations") for endpoint in calls)


def test_edit_failure_never_sends_image_model_to_chat(monkeypatch, tmp_path):
    source = tmp_path / "source.png"
    source.write_bytes(_png_bytes())
    calls = []

    async def failing_call(endpoint, _key, *args, **kwargs):  # noqa: ANN001, ARG001
        calls.append(endpoint)
        return 503, '{"error":{"message":"upstream unavailable"}}'

    monkeypatch.setattr(server, "_call_with_retry", failing_call)
    result = asyncio.run(
        server.image_edit(
            prompt="do not reroute this edit",
            image_path=str(source),
            size="1024x1024",
            model="gpt-image-2-openai",
            api_key="sk-test",
        )
    )

    assert result["ok"] is False
    assert calls
    assert all(endpoint.url.endswith("/v1/images/edits") for endpoint in calls)


def test_multi_reference_failure_never_sends_image_model_to_chat(monkeypatch, tmp_path):
    sources = []
    for index in range(2):
        source = tmp_path / f"source-{index}.png"
        source.write_bytes(_png_bytes())
        sources.append(str(source))
    calls = []

    async def failing_call(endpoint, _key, *args, **kwargs):  # noqa: ANN001, ARG001
        calls.append(endpoint)
        return 503, '{"error":{"message":"upstream unavailable"}}'

    monkeypatch.setattr(server, "_call_with_retry", failing_call)
    result = asyncio.run(
        server.image_multi_reference(
            prompt="do not reroute these references",
            image_paths=sources,
            size="1024x1024",
            model="gpt-image-2-openai",
            api_key="sk-test",
        )
    )

    assert result["ok"] is False
    assert result["used_fallback"] is False
    assert calls
    assert all(endpoint.url.endswith("/v1/images/edits") for endpoint in calls)
