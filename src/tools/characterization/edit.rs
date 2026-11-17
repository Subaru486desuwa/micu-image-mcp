use serde_json::{Value, json};

use crate::{
    domain::routing::RetryOptions,
    tools::test_support::{
        CapturedImage, Endpoint, Reply, ScriptedProvider, gray_1024x1024_png, rgb_32x24_png,
        rgba_32x24_png,
    },
};

use super::{
    Fixture, SIZE_NOTE_32X24_FOR_1024, SIZE_NOTE_32X24_FOR_2048X1152, models_and_formats, params,
    scripted, session_id,
};

async fn edit(fixture: &Fixture, arguments: Value) -> Value {
    fixture.engine.image_edit(params(arguments)).await.unwrap()
}

/// Opens a session whose revision 1 is `<TMP>/out/s1.png` (32x24).
async fn started_session(fixture: &Fixture) -> String {
    let source = fixture.input("source.png", &rgb_32x24_png());
    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let first = edit(
        fixture,
        json!({
            "prompt": "第一轮", "image_path": source, "basename": "s1",
            "session_id": "new", "context": {"identity": "same face"},
        }),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    session_id(&first)
}

#[tokio::test]
async fn edit_success_reports_single_image_shape_with_size_and_format_notes() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let source = fixture.input("source.png", &rgb_32x24_png());
    let mask = fixture.input("mask.png", &rgba_32x24_png());
    let result = edit(
        &fixture,
        json!({
            "prompt": "换背景", "image_path": source, "mask_path": mask, "size": "1024x1024",
            "quality": "high", "output_format": "webp", "basename": "edited",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-sunburst",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": {
                "path": "<TMP>/out/edited.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "notes": [
                SIZE_NOTE_32X24_FOR_1024,
                "⚠ 请求 output_format=webp，实际返回 png；以 saved[].format 为准",
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].endpoint, Endpoint::Edit);
    assert_eq!(calls[0].model, "gpt-image-2.6-sunburst");
    assert_eq!(calls[0].prompt, "换背景");
    assert_eq!(calls[0].size, "1024x1024");
    assert_eq!(calls[0].quality.as_deref(), Some("high"));
    assert_eq!(calls[0].response_format, None);
    assert_eq!(calls[0].output_format.as_deref(), Some("webp"));
    assert_eq!(
        calls[0].images,
        [CapturedImage {
            field: "image".into(),
            filename: "source.png".into(),
            mime: "image/png".into(),
            bytes: rgb_32x24_png(),
        }]
    );
    assert!(calls[0].has_mask);
    assert_eq!(
        calls[0].retry,
        RetryOptions {
            enabled: true,
            big_size: false
        }
    );
}

#[tokio::test]
async fn edit_default_basename_and_honored_size_have_no_notes() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(
        &gray_1024x1024_png(),
    )));
    let source = fixture.input("source.png", &rgb_32x24_png());
    let result = edit(&fixture, json!({"prompt": "换背景", "image_path": source})).await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2.6-sunburst",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": {
                "path": "<TMP>/out/edit_<NANOS>.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
            },
            "notes": [],
        }),
    );
    assert!(!fixture.provider.calls()[0].has_mask);
}

#[tokio::test]
async fn edit_upstream_failure_redacts_the_override_key() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::json(
        401,
        json!({"error": {"message": "invalid key sk-override-key"}}),
    )));
    let source = fixture.input("source.png", &rgb_32x24_png());
    let result = edit(
        &fixture,
        json!({
            "prompt": "换背景", "image_path": source, "basename": "edited",
            "api_key": "sk-override-key",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "error": "HTTP 401: invalid key [REDACTED]",
            "notes": [],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].key, "sk-override-key");
}

#[tokio::test]
async fn edit_save_failure_on_modern_route_is_not_retried() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let source = fixture.input("source.png", &rgb_32x24_png());
    let result = edit(
        &fixture,
        json!({"prompt": "换背景", "image_path": source, "basename": "edited"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "error": "保存失败: 响应中未识别到图片",
            "notes": [],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [("gpt-image-2.6-sunburst", None)]
    );
}

#[tokio::test]
async fn edit_legacy_route_retries_next_format_and_carries_model_and_fallback() {
    let fixture = Fixture::new(scripted());
    let source = fixture.input("source.png", &rgb_32x24_png());
    fixture.provider.enqueue(
        Reply::no_image()
            .answered_by_fallback("gpt-image-2-compat")
            .with_note("上游第 1 次应答附带的提示"),
    );
    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let result = edit(
        &fixture,
        json!({
            "prompt": "增强细节", "image_path": source, "size": "2048x1152",
            "model": "gpt-image-2", "basename": "big",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2-compat",
            "size": "2048x1152",
            "used_fallback": true,
            "saved": {
                "path": "<TMP>/out/big.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "notes": [
                "size=2048x1152 (2k) 已自动切到高质量线路 gpt-image-2-openai",
                "上游第 1 次应答附带的提示",
                "URL 落盘失败（保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json",
                "已通过 response_format=b64_json 成功落盘",
                SIZE_NOTE_32X24_FOR_2048X1152,
            ],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(
        models_and_formats(&calls),
        [
            ("gpt-image-2-openai", Some("url")),
            ("gpt-image-2-compat", Some("b64_json")),
        ]
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
async fn edit_validation_error_has_error_and_errors() {
    let fixture = Fixture::new(scripted());
    let result = edit(&fixture, json!({"prompt": "换背景"})).await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "error": "单次编辑或新会话需要 image_path；已有会话可自动使用上一版本",
            "errors": ["单次编辑或新会话需要 image_path；已有会话可自动使用上一版本"],
        }),
    );
    assert!(fixture.provider.calls().is_empty());
}

// Existing behavior: a new session is created before the request is validated,
// so a validation error reports no session yet leaves its empty directory behind.
#[tokio::test]
async fn edit_new_session_validation_error_reports_no_session_but_leaves_its_directory() {
    let fixture = Fixture::new(scripted());
    let result = edit(&fixture, json!({"prompt": "换背景", "session_id": "new"})).await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "error": "单次编辑或新会话需要 image_path；已有会话可自动使用上一版本",
            "errors": ["单次编辑或新会话需要 image_path；已有会话可自动使用上一版本"],
        }),
    );
    let session_directories = std::fs::read_dir(fixture.root.join("out").join(".micu-sessions"))
        .unwrap()
        .count();
    assert_eq!(session_directories, 1);
}

#[tokio::test]
async fn edit_session_success_puts_session_first_and_continues_from_previous_output() {
    let fixture = Fixture::new(scripted());
    let source = fixture.input("source.png", &rgb_32x24_png());

    fixture
        .provider
        .enqueue(Reply::image(&gray_1024x1024_png()));
    let first = edit(
        &fixture,
        json!({
            "prompt": "把衣服改成红色", "image_path": source, "basename": "s1",
            "session_id": "new", "context": {"identity": "same face"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &first,
        json!({
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "parent_revision": null,
                "input_path": "<TMP>/input/source.png",
                "context": {"identity": "same face"},
                "history_length": 1,
            },
            "ok": true,
            "model": "gpt-image-2.6-sunburst",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": {
                "path": "<TMP>/out/s1.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
            },
            "notes": [],
        }),
    );

    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let second = edit(
        &fixture,
        json!({
            "prompt": "背景换成海边", "session_id": session_id(&first), "basename": "s2",
            "context": {"background": "seaside"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &second,
        json!({
            "session": {
                "id": "edit_<SESSION>",
                "revision": 2,
                "parent_revision": 1,
                "input_path": "<TMP>/out/s1.png",
                "context": {"background": "seaside", "identity": "same face"},
                "history_length": 2,
            },
            "ok": true,
            "model": "gpt-image-2.6-sunburst",
            "size": "1024x1024",
            "used_fallback": false,
            "saved": {
                "path": "<TMP>/out/s2.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "notes": [SIZE_NOTE_32X24_FOR_1024],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(
        calls[1].images,
        [CapturedImage {
            field: "image".into(),
            filename: "s1.png".into(),
            mime: "image/png".into(),
            bytes: gray_1024x1024_png(),
        }]
    );
    assert!(
        calls[1].prompt.ends_with(
            "\n\nSUCCESSFUL EDIT HISTORY (chronological, selected branch only):\n[{\"revision\":1,\"request\":\"把衣服改成红色\"}]\n\nCURRENT EDIT INSTRUCTION:\n背景换成海边"
        ),
        "{}",
        calls[1].prompt
    );
}

#[tokio::test]
async fn edit_session_upstream_failure_reports_unchanged_session() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    fixture.provider.enqueue(Reply::json(
        400,
        json!({"error": {"message": "content policy"}}),
    ));
    let result = edit(
        &fixture,
        json!({
            "prompt": "失败的一轮", "session_id": id, "context": {"identity": "other"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "context": {"identity": "same face"},
            },
            "error": "HTTP 400: content policy",
            "notes": [],
        }),
    );
    assert!(!fixture.revision_record(&id, 2).exists());
}

#[tokio::test]
async fn edit_new_session_upstream_failure_reports_null_revision() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::json(
        400,
        json!({"error": {"message": "content policy"}}),
    )));
    let source = fixture.input("source.png", &rgb_32x24_png());
    let result = edit(
        &fixture,
        json!({
            "prompt": "第一轮", "image_path": source, "session_id": "new",
            "context": {"identity": "same face"},
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "used_fallback": false,
            "size": "1024x1024",
            "session": {
                "id": "edit_<SESSION>",
                "revision": null,
                "context": null,
            },
            "error": "HTTP 400: content policy",
            "notes": [],
        }),
    );
}

#[tokio::test]
async fn edit_session_commit_failure_keeps_saved_image() {
    let fixture = Fixture::new(scripted());
    let id = started_session(&fixture).await;

    // Another writer publishes revision 2 while the upstream call is in flight.
    let competing_record = fixture.revision_record(&id, 2);
    fixture
        .provider
        .enqueue(Reply::image(&rgb_32x24_png()).with_effect(move || {
            std::fs::write(&competing_record, b"{}").unwrap();
        }));
    let result = edit(
        &fixture,
        json!({"prompt": "第二轮", "session_id": id, "basename": "s2"}),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "error": "图片已保存，但会话提交失败：会话版本提交失败",
            "errors": ["图片已保存，但会话提交失败：会话版本提交失败"],
            "saved": {
                "path": "<TMP>/out/s2.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "session": {
                "id": "edit_<SESSION>",
                "revision": 1,
                "context": {"identity": "same face"},
            },
        }),
    );
    assert!(fixture.root.join("out").join("s2.png").is_file());
}
