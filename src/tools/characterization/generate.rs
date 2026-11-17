use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    domain::routing::RetryOptions,
    tools::test_support::{
        CapturedImage, Endpoint, Reply, ScriptedProvider, gray_1024x1024_png, rgb_32x24_png,
    },
};

use super::{
    Fixture, SIZE_NOTE_32X24_FOR_1024, SIZE_NOTE_32X24_FOR_2048X1152, models_and_formats, params,
    scripted, session_id,
};

const CONTINUED_NOTE: &str = "已有会话通过参考编辑延续上一版本图片与上下文";

async fn generate(fixture: &Fixture, arguments: Value) -> Value {
    fixture
        .engine
        .image_generate(params(arguments))
        .await
        .unwrap()
}

/// Opens a session whose revision 1 is `<TMP>/out/v1_1.png` (32x24).
async fn started_session(fixture: &Fixture) -> String {
    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let first = generate(
        fixture,
        json!({
            "prompt": "画一只猫", "size": "1024x1024", "basename": "v1",
            "session_id": "new", "context": {"style": "flat"},
        }),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    session_id(&first)
}

#[tokio::test]
async fn generate_success_reports_list_shape_with_size_and_format_notes() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "output_format": "webp", "basename": "cat",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/cat_1.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "model": "gpt-image-2.6-flare",
            }],
            "errors": [],
            "notes": [
                SIZE_NOTE_32X24_FOR_1024,
                "⚠ 请求 output_format=webp，实际返回 png；以 saved[].format 为准",
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].endpoint, Endpoint::Generate);
    assert_eq!(calls[0].model, "gpt-image-2.6-flare");
    assert_eq!(calls[0].prompt, "一只猫");
    assert_eq!(calls[0].size, "1024x1024");
    assert_eq!(calls[0].quality, None);
    assert_eq!(calls[0].response_format, None);
    assert_eq!(calls[0].output_format.as_deref(), Some("webp"));
    assert_eq!(
        calls[0].retry,
        RetryOptions {
            enabled: false,
            big_size: false
        }
    );
}

#[tokio::test]
async fn generate_infers_size_and_uses_default_basename_when_size_is_honored() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(
        &gray_1024x1024_png(),
    )));
    let result = generate(&fixture, json!({"prompt": "一只猫"})).await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": true,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/gen_<NANOS>_1.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
                "model": "gpt-image-2.6-flare",
            }],
            "errors": [],
            "notes": ["size=None → 无关键字命中，用默认 1024x1024"],
        }),
    );
}

#[tokio::test]
async fn generate_upstream_failure_reports_indexed_error_and_redacts_key() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::json(
        500,
        json!({"error": {"message": "upstream rejected key sk-test"}}),
    )));
    let result = generate(
        &fixture,
        json!({"prompt": "一只猫", "size": "1024x1024", "basename": "cat"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [],
            "errors": ["#1 HTTP 500: upstream rejected key [REDACTED]"],
            "notes": [],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].key, "sk-test");
}

#[tokio::test]
async fn generate_save_failure_on_modern_route_is_not_retried() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let result = generate(
        &fixture,
        json!({"prompt": "一只猫", "size": "1024x1024", "basename": "cat"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [],
            "errors": ["#1 保存失败: 响应中未识别到图片"],
            "notes": [],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [("gpt-image-2.6-flare", None)]
    );
}

#[tokio::test]
async fn generate_legacy_route_retries_next_format_with_indexed_error_in_note() {
    let fixture = Fixture::new(scripted());
    // #1: url cannot be saved, b64_json can. #2: upstream fails on the first format.
    fixture.provider.enqueue(Reply::no_image());
    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    fixture
        .provider
        .enqueue(Reply::json(500, json!({"error": {"message": "boom"}})));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "n": 2,
            "model": "gpt-image-2-openai", "basename": "pair",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2-openai",
            "size": "1024x1024",
            "requested_n": 2,
            "used_fallback": false,
            "size_honored": false,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/pair_1.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "model": "gpt-image-2-openai",
            }],
            "errors": ["#2 HTTP 500: boom"],
            "notes": [
                "URL 落盘失败（#1 保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json",
                "已通过 response_format=b64_json 成功落盘",
                SIZE_NOTE_32X24_FOR_1024,
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(
        models_and_formats(&calls),
        [
            ("gpt-image-2-openai", Some("url")),
            ("gpt-image-2-openai", Some("b64_json")),
            ("gpt-image-2-openai", Some("url")),
        ]
    );
    assert_eq!(
        calls[0].retry,
        RetryOptions {
            enabled: true,
            big_size: false
        }
    );
}

#[tokio::test]
async fn generate_legacy_route_reports_last_save_error_when_every_format_fails() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "model": "gpt-image-2", "basename": "cat",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [],
            "errors": ["#1 保存失败: 响应中未识别到图片"],
            "notes": [
                "URL 落盘失败（#1 保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json",
            ],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [
            ("gpt-image-2", Some("url")),
            ("gpt-image-2", Some("b64_json")),
        ]
    );
}

#[tokio::test]
async fn generate_legacy_route_stops_at_upstream_failure() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::raw(502, "Bad Gateway")));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "model": "gpt-image-2", "basename": "cat",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [],
            "errors": ["#1 HTTP 502: Bad Gateway"],
            "notes": [],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [("gpt-image-2", Some("url"))]
    );
}

// Existing behavior: with no image saved, the top-level model is the one that
// answered the last image, while `used_fallback` is true if any image used one.
#[tokio::test]
async fn generate_all_failed_takes_model_from_the_last_image() {
    let fixture = Fixture::new(scripted());
    fixture.provider.enqueue(
        Reply::json(500, json!({"error": {"message": "boom"}}))
            .answered_by_fallback("gpt-image-2-compat"),
    );
    fixture
        .provider
        .enqueue(Reply::json(500, json!({"error": {"message": "boom"}})));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "n": 2,
            "model": "gpt-image-2-openai", "basename": "pair",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2-openai",
            "size": "1024x1024",
            "requested_n": 2,
            "used_fallback": true,
            "size_honored": false,
            "saved": [],
            "errors": ["#1 HTTP 500: boom", "#2 HTTP 500: boom"],
            "notes": [],
        }),
    );
    // Each image starts from the routed model, not from the previous image's answer.
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [
            ("gpt-image-2-openai", Some("url")),
            ("gpt-image-2-openai", Some("url")),
        ]
    );
}

#[tokio::test]
async fn generate_parallel_route_keeps_request_order_and_merges_notes_once() {
    let fixture = Fixture::new(
        ScriptedProvider::answering(Reply::image(&rgb_32x24_png()))
            .with_delay(Duration::from_millis(30)),
    );
    let result = generate(
        &fixture,
        json!({"prompt": "一只猫", "size": "1024x1024", "n": 2, "basename": "set"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 2,
            "used_fallback": false,
            "size_honored": false,
            "saved": [
                {
                    "index": 1,
                    "path": "<TMP>/out/set_1.png",
                    "size_bytes": 93,
                    "format": "png",
                    "actual_size": "32x24",
                    "actual_megapixels": 0.0,
                    "model": "gpt-image-2.6-flare",
                },
                {
                    "index": 2,
                    "path": "<TMP>/out/set_2.png",
                    "size_bytes": 93,
                    "format": "png",
                    "actual_size": "32x24",
                    "actual_megapixels": 0.0,
                    "model": "gpt-image-2.6-flare",
                },
            ],
            "errors": [],
            "notes": ["1K + 标准线路 + N=2 已 5 并发", SIZE_NOTE_32X24_FOR_1024],
        }),
    );
    assert_eq!(fixture.provider.peak_in_flight(), 2);
}

#[tokio::test]
async fn generate_reports_model_and_fallback_answered_by_provider() {
    let fixture = Fixture::new(scripted());
    fixture.provider.enqueue(
        Reply::image(&rgb_32x24_png())
            .answered_by_fallback("gpt-image-2.5-flare-openai")
            .with_note(
                "上游明确拒绝模型 gpt-image-2.5-flare → 使用兼容 ID gpt-image-2.5-flare-openai 补试一次",
            ),
    );
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024",
            "model": "gpt-image-2.5-flare-openai", "basename": "alias",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.5-flare-openai",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": true,
            "size_honored": false,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/alias_1.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "model": "gpt-image-2.5-flare-openai",
            }],
            "errors": [],
            "notes": [
                "模型 ID gpt-image-2.5-flare-openai 已更新为 gpt-image-2.5-flare",
                "上游明确拒绝模型 gpt-image-2.5-flare → 使用兼容 ID gpt-image-2.5-flare-openai 补试一次",
                SIZE_NOTE_32X24_FOR_1024,
            ],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [("gpt-image-2.5-flare", None)]
    );
}

#[tokio::test]
async fn generate_caller_notes_precede_image_notes() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一张 2K 风景图", "n": 3, "model": "gpt-image-2", "basename": "wide",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2-openai",
            "size": "2048x1152",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/wide_1.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "model": "gpt-image-2-openai",
            }],
            "errors": [],
            "notes": [
                "size=None → 推断 2048x1152（prompt 含 2K/1080p 关键字（默认横屏））",
                "size=2048x1152 (2k) 已自动切到高质量线路 gpt-image-2-openai",
                "2K 强制 N=1，已忽略请求的 n=3",
                SIZE_NOTE_32X24_FOR_2048X1152,
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(
        models_and_formats(&calls),
        [("gpt-image-2-openai", Some("url"))]
    );
    assert_eq!(
        calls[0].retry,
        RetryOptions {
            enabled: true,
            big_size: true
        }
    );
}

#[tokio::test]
async fn generate_new_session_puts_session_first() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({
            "prompt": "画一只猫", "size": "1024x1024", "basename": "v1",
            "session_id": "new", "context": {"style": "flat"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "parent_revision": null,
                "input_path": "",
                "context": {"style": "flat"},
                "history_length": 1,
            },
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "requested_n": 1,
            "used_fallback": false,
            "size_honored": false,
            "saved": [{
                "index": 1,
                "path": "<TMP>/out/v1_1.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "model": "gpt-image-2.6-flare",
            }],
            "errors": [],
            "notes": [SIZE_NOTE_32X24_FOR_1024],
        }),
    );
    assert_eq!(
        fixture.provider.calls()[0].prompt,
        "Create the first image in this editing session. CURRENT GENERATION INSTRUCTION takes priority over EFFECTIVE CONSTRAINTS.\n\nEFFECTIVE CONSTRAINTS (keyed, current state):\n{\"style\":\"flat\"}\n\nCURRENT GENERATION INSTRUCTION:\n画一只猫"
    );
}

// Existing quirk: a continued generation is an edit result rewritten into the
// list shape, so `index` is the last key of `saved[0]` and the list fields
// follow `notes`.
#[tokio::test]
async fn continued_generation_success_keeps_list_shape_with_index_last() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    fixture
        .provider
        .enqueue(Reply::image(&gray_1024x1024_png()));
    let second = generate(
        &fixture,
        json!({"prompt": "换成夜晚", "session_id": id, "basename": "v2"}),
    )
    .await;
    fixture.assert_public_json(
        &second,
        json!({
            "session": {
                "id": "edit_<SESSION>",
                "revision": 2,
                "parent_revision": 1,
                "input_path": "<TMP>/out/v1_1.png",
                "context": {"style": "flat"},
                "history_length": 2,
            },
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": [{
                "path": "<TMP>/out/v2.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
                "index": 1,
            }],
            "notes": [CONTINUED_NOTE],
            "requested_n": 1,
            "size_honored": true,
            "errors": [],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls[1].endpoint, Endpoint::Edit);
    assert_eq!(
        models_and_formats(&calls[1..]),
        [("gpt-image-2.6-flare", None)]
    );
    assert_eq!(
        calls[1].images,
        [CapturedImage {
            field: "image".into(),
            filename: "v1_1.png".into(),
            mime: "image/png".into(),
            bytes: rgb_32x24_png(),
        }]
    );
    assert!(!calls[1].has_mask);
    assert!(
        calls[1].prompt.ends_with(
            "\n\nSUCCESSFUL EDIT HISTORY (chronological, selected branch only):\n[{\"revision\":1,\"request\":\"画一只猫\"}]\n\nCURRENT EDIT INSTRUCTION:\n换成夜晚"
        ),
        "{}",
        calls[1].prompt
    );

    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let third = generate(
        &fixture,
        json!({
            "prompt": "加上月亮", "session_id": id, "basename": "v3",
            "context": {"mood": "quiet"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &third,
        json!({
            "session": {
                "id": "edit_<SESSION>",
                "revision": 3,
                "parent_revision": 2,
                "input_path": "<TMP>/out/v2.png",
                "context": {"mood": "quiet", "style": "flat"},
                "history_length": 3,
            },
            "ok": true,
            "model": "gpt-image-2.6-flare",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": [{
                "path": "<TMP>/out/v3.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
                "index": 1,
            }],
            "notes": [SIZE_NOTE_32X24_FOR_1024, CONTINUED_NOTE],
            "requested_n": 1,
            "size_honored": false,
            "errors": [],
        }),
    );
}

// Existing defect, fixed later as a separate change: an upstream failure of a
// continued generation returns the edit shape, without `saved`, `errors`,
// `requested_n` or `size_honored`, and its error has no `#1 ` prefix.
#[tokio::test]
async fn continued_generation_upstream_failure_returns_edit_shape() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    fixture.provider.enqueue(Reply::json(
        500,
        json!({"error": {"message": "upstream exploded"}}),
    ));
    let result = generate(
        &fixture,
        json!({"prompt": "换成夜晚", "session_id": id, "basename": "v2"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-flare",
            "used_fallback": false,
            "size": "1024x1024",
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "context": {"style": "flat"},
            },
            "error": "HTTP 500: upstream exploded",
            "notes": [],
        }),
    );
    assert!(!fixture.revision_record(&id, 2).exists());
}

#[tokio::test]
async fn continued_generation_save_failure_returns_edit_shape() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    fixture.provider.enqueue(Reply::no_image());
    let result = generate(
        &fixture,
        json!({"prompt": "换成夜晚", "session_id": id, "basename": "v2"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-flare",
            "used_fallback": false,
            "size": "1024x1024",
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "context": {"style": "flat"},
            },
            "error": "保存失败: 响应中未识别到图片",
            "notes": [],
        }),
    );
    assert_eq!(fixture.provider.calls().len(), 2);
    assert!(!fixture.revision_record(&id, 2).exists());
}

// Existing quirk: when the image is saved but the session commit fails, the
// result has no `size`, so `size_honored` is false even though the saved image
// has exactly the requested size.
#[tokio::test]
async fn continued_generation_commit_failure_has_no_size_and_never_honors_size() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    // Another writer publishes revision 2 while the upstream call is in flight.
    let competing_record = fixture.revision_record(&id, 2);
    fixture
        .provider
        .enqueue(Reply::image(&gray_1024x1024_png()).with_effect(move || {
            std::fs::write(&competing_record, b"{}").unwrap();
        }));
    let result = generate(
        &fixture,
        json!({"prompt": "换成夜晚", "session_id": id, "basename": "v2"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "error": "图片已保存，但会话提交失败：会话版本提交失败",
            "errors": ["图片已保存，但会话提交失败：会话版本提交失败"],
            "saved": [{
                "path": "<TMP>/out/v2.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
                "index": 1,
            }],
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "context": {"style": "flat"},
            },
            "requested_n": 1,
            "size_honored": false,
        }),
    );
    assert!(fixture.root.join("out").join("v2.png").is_file());
}

#[tokio::test]
async fn continued_generation_validation_error_passes_through() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    let result = generate(
        &fixture,
        json!({"prompt": "换成夜晚", "session_id": id, "parent_revision": 9}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "error": "编辑会话不存在或无法读取",
            "errors": ["编辑会话不存在或无法读取"],
        }),
    );
    assert_eq!(fixture.provider.calls().len(), 1);
}
