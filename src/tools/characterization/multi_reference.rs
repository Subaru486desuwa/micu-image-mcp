use serde_json::{Value, json};

use crate::tools::test_support::{
    CapturedImage, Endpoint, Reply, ScriptedProvider, gray_1024x1024_png, rgb_32x24_png,
    rgba_32x24_png,
};

use super::{Fixture, SIZE_NOTE_32X24_FOR_1024, models_and_formats, params, scripted};

async fn multi_reference(fixture: &Fixture, arguments: Value) -> Value {
    fixture
        .engine
        .image_multi_reference(params(arguments))
        .await
        .unwrap()
}

fn references(fixture: &Fixture) -> [String; 2] {
    [
        fixture.input("ref-0.png", &rgb_32x24_png()),
        fixture.input("ref-1.png", &rgba_32x24_png()),
    ]
}

#[tokio::test]
async fn multi_reference_success_reports_reference_count_and_plain_size_note() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture), "quality": "max",
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
            "size_honored": false,
            "n_references": 2,
            "saved": {
                "path": "<TMP>/out/multiref_<NANOS>.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "notes": [SIZE_NOTE_32X24_FOR_1024],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].endpoint, Endpoint::Edit);
    assert_eq!(calls[0].model, "gpt-image-2.6-sunburst");
    assert_eq!(
        calls[0].prompt,
        "Reference images are provided. Synthesize their visual elements (style, palette, composition, subjects) into ONE single new image per the instruction below. Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\nInstruction:\n融合成海报"
    );
    assert_eq!(calls[0].size, "1024x1024");
    assert_eq!(calls[0].quality.as_deref(), Some("max"));
    assert_eq!(calls[0].response_format, None);
    assert_eq!(calls[0].output_format, None);
    assert_eq!(
        calls[0].images,
        [
            CapturedImage {
                field: "image[]".into(),
                filename: "ref-0.png".into(),
                mime: "image/png".into(),
                bytes: rgb_32x24_png(),
            },
            CapturedImage {
                field: "image[]".into(),
                filename: "ref-1.png".into(),
                mime: "image/png".into(),
                bytes: rgba_32x24_png(),
            },
        ]
    );
    assert!(!calls[0].has_mask);
}

#[tokio::test]
async fn multi_reference_honored_size_has_no_notes() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(
        &gray_1024x1024_png(),
    )));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture), "basename": "poster",
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
            "size_honored": true,
            "n_references": 2,
            "saved": {
                "path": "<TMP>/out/poster.png",
                "size_bytes": 207,
                "format": "png",
                "actual_size": "1024x1024",
                "actual_megapixels": 1.05,
            },
            "notes": [],
        }),
    );
}

#[tokio::test]
async fn multi_reference_upstream_failure_reports_error_without_size() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::json(
        429,
        json!({"error": {"message": "rate limited"}}),
    )));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture), "basename": "poster",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "n_references": 2,
            "used_fallback": false,
            "error": "HTTP 429: rate limited",
            "notes": [],
        }),
    );
    assert_eq!(fixture.provider.calls().len(), 1);
}

#[tokio::test]
async fn multi_reference_save_failure_on_modern_route_is_not_retried() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture), "basename": "poster",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2.6-sunburst",
            "n_references": 2,
            "used_fallback": false,
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
async fn multi_reference_legacy_route_retries_next_format() {
    let fixture = Fixture::new(scripted());
    fixture.provider.enqueue(Reply::no_image());
    fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture),
            "model": "gpt-image-2-openai", "basename": "blend",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": "gpt-image-2-openai",
            "size": "1024x1024",
            "used_fallback": false,
            "size_honored": false,
            "n_references": 2,
            "saved": {
                "path": "<TMP>/out/blend.png",
                "size_bytes": 93,
                "format": "png",
                "actual_size": "32x24",
                "actual_megapixels": 0.0,
            },
            "notes": [
                "URL 落盘失败（保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json",
                "已通过 response_format=b64_json 成功落盘",
                SIZE_NOTE_32X24_FOR_1024,
            ],
        }),
    );
    assert_eq!(
        models_and_formats(&fixture.provider.calls()),
        [
            ("gpt-image-2-openai", Some("url")),
            ("gpt-image-2-openai", Some("b64_json")),
        ]
    );
}

#[tokio::test]
async fn multi_reference_legacy_route_reports_last_save_error_when_every_format_fails() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
    let result = multi_reference(
        &fixture,
        json!({
            "prompt": "融合成海报", "image_paths": references(&fixture),
            "model": "gpt-image-2", "basename": "blend",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": false,
            "model": "gpt-image-2",
            "n_references": 2,
            "used_fallback": false,
            "error": "保存失败: 响应中未识别到图片",
            "notes": [
                "URL 落盘失败（保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json",
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
