use std::time::Duration;

use serde_json::{Value, json};

use crate::tools::test_support::{Reply, ScriptedProvider, rgb_32x24_png};

use super::{Fixture, SIZE_NOTE_32X24_FOR_1024, models_and_formats, params, scripted};

async fn batch_edit(fixture: &Fixture, arguments: Value) -> Value {
    fixture
        .engine
        .image_batch_edit(params(arguments))
        .await
        .unwrap()
}

#[tokio::test]
async fn batch_success_on_parallel_route_adds_index_and_input_to_edit_results() {
    let fixture = Fixture::new(
        ScriptedProvider::answering(Reply::image(&rgb_32x24_png()))
            .with_delay(Duration::from_millis(30)),
    );
    let first = fixture.input("a.png", &rgb_32x24_png());
    let second = fixture.input("b.png", &rgb_32x24_png());
    let result = batch_edit(
        &fixture,
        json!({
            "prompt": "素描风", "image_paths": [first, second], "model": "gpt-image-2.6-flare",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "used_fallback": false,
            "size": "1024x1024",
            "concurrency": 5,
            "total": 2,
            "succeeded": 2,
            "failed": 0,
            "results": [
                {
                    "ok": true,
                    "model": "gpt-image-2.6-flare",
                    "size": "1024x1024",
                    "used_fallback": false,
                    "saved": {
                        "path": "<TMP>/out/batch_<NANOS>_1.png",
                        "size_bytes": 93,
                        "format": "png",
                        "actual_size": "32x24",
                        "actual_megapixels": 0.0,
                    },
                    "notes": [SIZE_NOTE_32X24_FOR_1024],
                    "index": 1,
                    "input": "<TMP>/input/a.png",
                },
                {
                    "ok": true,
                    "model": "gpt-image-2.6-flare",
                    "size": "1024x1024",
                    "used_fallback": false,
                    "saved": {
                        "path": "<TMP>/out/batch_<NANOS>_2.png",
                        "size_bytes": 93,
                        "format": "png",
                        "actual_size": "32x24",
                        "actual_megapixels": 0.0,
                    },
                    "notes": [SIZE_NOTE_32X24_FOR_1024],
                    "index": 2,
                    "input": "<TMP>/input/b.png",
                },
            ],
            "notes": [],
        }),
    );
    assert_eq!(fixture.provider.peak_in_flight(), 2);
}

#[tokio::test]
async fn batch_upstream_failure_takes_model_from_the_first_result() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::json(
        500,
        json!({"error": {"message": "boom"}}),
    )));
    let input = fixture.input("a.png", &rgb_32x24_png());
    let result = batch_edit(
        &fixture,
        json!({"prompt": "素描风", "image_paths": [input]}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "concurrency": 1,
            "total": 1,
            "succeeded": 0,
            "failed": 1,
            "results": [{
                "ok": false,
                "model": "gpt-image-2.6-sunburst",
                "used_fallback": false,
                "size": "1024x1024",
                "error": "HTTP 500: boom",
                "notes": [],
                "index": 1,
                "input": "<TMP>/input/a.png",
            }],
            "notes": [],
        }),
    );
}

#[tokio::test]
async fn batch_save_failure_reports_the_edit_failure_shape_per_item() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let input = fixture.input("a.png", &rgb_32x24_png());
    let result = batch_edit(
        &fixture,
        json!({"prompt": "素描风", "image_paths": [input]}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "concurrency": 1,
            "total": 1,
            "succeeded": 0,
            "failed": 1,
            "results": [{
                "ok": false,
                "model": "gpt-image-2.6-sunburst",
                "used_fallback": false,
                "size": "1024x1024",
                "error": "保存失败: 响应中未识别到图片",
                "notes": [],
                "index": 1,
                "input": "<TMP>/input/a.png",
            }],
            "notes": [],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [("gpt-image-2.6-sunburst", None)]
    );
}

#[tokio::test]
async fn batch_serial_route_mixes_results_and_reports_differing_models() {
    let fixture = Fixture::new(scripted());
    let first = fixture.input("a.png", &rgb_32x24_png());
    let second = fixture.input("b.png", &rgb_32x24_png());
    fixture
        .provider
        .enqueue(Reply::json(500, json!({"error": {"message": "boom"}})));
    fixture.provider.enqueue(
        Reply::image(&rgb_32x24_png())
            .answered_by_fallback("gpt-image-2.5-sunburst-openai")
            .with_note(
                "上游明确拒绝模型 gpt-image-2.5-sunburst → 使用兼容 ID gpt-image-2.5-sunburst-openai 补试一次",
            ),
    );
    let result = batch_edit(
        &fixture,
        json!({
            "prompt": "素描风", "image_paths": [first, second],
            "model": "gpt-image-2.5-sunburst-openai",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.5-sunburst-openai",
            "used_fallback": true,
            "size": "1024x1024",
            "concurrency": 1,
            "total": 2,
            "succeeded": 1,
            "failed": 1,
            "results": [
                {
                    "ok": false,
                    "model": "gpt-image-2.5-sunburst",
                    "used_fallback": false,
                    "size": "1024x1024",
                    "error": "HTTP 500: boom",
                    "notes": [],
                    "index": 1,
                    "input": "<TMP>/input/a.png",
                },
                {
                    "ok": true,
                    "model": "gpt-image-2.5-sunburst-openai",
                    "size": "1024x1024",
                    "used_fallback": true,
                    "saved": {
                        "path": "<TMP>/out/batch_<NANOS>_2.png",
                        "size_bytes": 93,
                        "format": "png",
                        "actual_size": "32x24",
                        "actual_megapixels": 0.0,
                    },
                    "notes": [
                        "上游明确拒绝模型 gpt-image-2.5-sunburst → 使用兼容 ID gpt-image-2.5-sunburst-openai 补试一次",
                        SIZE_NOTE_32X24_FOR_1024,
                    ],
                    "index": 2,
                    "input": "<TMP>/input/b.png",
                },
            ],
            "notes": [
                "模型 ID gpt-image-2.5-sunburst-openai 已更新为 gpt-image-2.5-sunburst",
                "批量结果使用了不同兼容 ID；各图片的实际模型见 results[].model",
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(
        models_and_formats(&calls),
        [
            ("gpt-image-2.5-sunburst", None),
            ("gpt-image-2.5-sunburst", None),
        ]
    );
    // Serial routes keep a 1.5s gap between images.
    assert!(
        calls[1].started_at.duration_since(calls[0].started_at) >= Duration::from_secs_f64(1.45)
    );
}

#[tokio::test]
async fn batch_items_report_validation_error_and_missing_key() {
    let fixture = Fixture::with_api_key(scripted(), "");
    let missing = fixture
        .root
        .join("input")
        .join("missing.png")
        .to_string_lossy()
        .into_owned();
    let present = fixture.input("a.png", &rgb_32x24_png());
    let result = batch_edit(
        &fixture,
        json!({
            "prompt": "素描风", "image_paths": [missing, present], "model": "gpt-image-2.6-flare",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-flare",
            "used_fallback": false,
            "size": "1024x1024",
            "concurrency": 5,
            "total": 2,
            "succeeded": 0,
            "failed": 2,
            "results": [
                {
                    "ok": false,
                    "error": "image_path 不存在: <TMP>/input/missing.png",
                    "errors": ["image_path 不存在: <TMP>/input/missing.png"],
                    "index": 1,
                    "input": "<TMP>/input/missing.png",
                },
                {
                    "ok": false,
                    "index": 2,
                    "input": "<TMP>/input/a.png",
                    "error": "API key 不可用（Missing）。在终端运行客户端配置所指向的同一个 micu-image-mcp 可执行文件的 credentials authorize 命令，完成后重连 MCP；首次配置或换 Key 请运行 credentials set。自定义凭据项须沿用 MICU_KEYCHAIN_SERVICE / MICU_KEYCHAIN_ACCOUNT。 也可设置 MICU_API_KEY 或在调用时传 api_key。",
                },
            ],
            "notes": [],
        }),
    );
    assert!(fixture.provider.calls().is_empty());
}
