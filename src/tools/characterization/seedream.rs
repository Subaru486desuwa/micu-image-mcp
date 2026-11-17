//! Seedream is text-to-image only: the upstream edits endpoint answers 200
//! but ignores the input image, so edit tools must never send it there.

use serde_json::{Value, json};

use crate::{
    domain::routing::RetryOptions,
    tools::test_support::{Endpoint, Reply, ScriptedProvider, rgb_32x24_png},
};

use super::{Fixture, SIZE_NOTE_32X24_FOR_1024, params, scripted, session_id};

const SEEDREAM: &str = "doubao-seedream-5-5-261028";
const EDIT_REJECTION: &str = "模型 doubao-seedream-5-5-261028 只支持 image_generate 文生图，不能用于编辑、批量编辑或多图参考；请显式传入 gpt-image 系列的 model";

async fn generate(fixture: &Fixture, arguments: Value) -> Value {
    fixture
        .engine
        .image_generate(params(arguments))
        .await
        .unwrap()
}

#[tokio::test]
async fn seedream_generation_turns_the_watermark_off_and_omits_response_format() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({
            "prompt": "一只猫", "size": "1024x1024", "model": SEEDREAM,
            "output_format": "png", "basename": "cat",
        }),
    )
    .await;
    fixture.assert_public_json(
        &result,
        json!({
            "ok": true,
            "model": SEEDREAM,
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
                "model": SEEDREAM,
            }],
            "errors": [],
            "notes": [SIZE_NOTE_32X24_FOR_1024],
        }),
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].endpoint, Endpoint::Generate);
    assert_eq!(calls[0].model, SEEDREAM);
    assert_eq!(calls[0].watermark, Some(false));
    assert_eq!(calls[0].quality, None);
    assert_eq!(calls[0].response_format, None);
    assert_eq!(calls[0].output_format.as_deref(), Some("png"));
}

#[tokio::test]
async fn other_models_never_send_a_watermark_field() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(&fixture, json!({"prompt": "一只猫", "size": "1024x1024"})).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(fixture.provider.calls()[0].watermark, None);
}

#[tokio::test]
async fn seedream_drops_quality_with_a_note() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({"prompt": "一只猫", "size": "1024x1024", "model": SEEDREAM, "quality": "high"}),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["notes"][0],
        "模型 doubao-seedream-5-5-261028 没有 quality 参数，已忽略 quality=high"
    );
    assert_eq!(fixture.provider.calls()[0].quality, None);
}

#[tokio::test]
async fn seedream_rejects_unsupported_formats_and_sizes_before_any_request() {
    let fixture = Fixture::new(scripted());
    let format = generate(
        &fixture,
        json!({"prompt": "一只猫", "model": SEEDREAM, "output_format": "webp"}),
    )
    .await;
    fixture.assert_public_json(
        &format,
        json!({
            "ok": false,
            "error": "output_format 不支持 'webp'；可选 jpeg / png",
            "errors": ["output_format 不支持 'webp'；可选 jpeg / png"],
        }),
    );
    let size = generate(
        &fixture,
        json!({"prompt": "雪山", "model": SEEDREAM, "size": "3840x2160"}),
    )
    .await;
    let message = "模型 doubao-seedream-5-5-261028 的总像素必须在 [921,600, 4,624,220] 范围内（如 1280x720、2048x2048、2560x1440）；3840x2160 超出范围";
    fixture.assert_public_json(
        &size,
        json!({"ok": false, "error": message, "errors": [message]}),
    );
    assert!(fixture.provider.calls().is_empty());
}

#[tokio::test]
async fn seedream_2k_keeps_n_and_runs_in_parallel_without_the_large_size_lock() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let result = generate(
        &fixture,
        json!({
            "prompt": "雪山", "size": "2048x2048", "model": SEEDREAM, "n": 3, "basename": "peak",
        }),
    )
    .await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["requested_n"], 3);
    assert_eq!(result["saved"].as_array().map(Vec::len), Some(3));
    assert!(
        result["notes"]
            .as_array()
            .is_some_and(|notes| notes.contains(&json!("Seedream + N=3 已 5 并发"))),
        "{result}"
    );
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(
            call.retry,
            RetryOptions {
                enabled: false,
                big_size: false
            }
        );
    }
}

#[tokio::test]
async fn edit_tools_reject_seedream_before_any_request() {
    let fixture = Fixture::new(scripted());
    let first = fixture.input("a.png", &rgb_32x24_png());
    let second = fixture.input("b.png", &rgb_32x24_png());

    let edit = fixture
        .engine
        .image_edit(params(
            json!({"prompt": "改成红色", "image_path": first, "model": SEEDREAM}),
        ))
        .await
        .unwrap();
    fixture.assert_public_json(
        &edit,
        json!({"ok": false, "error": EDIT_REJECTION, "errors": [EDIT_REJECTION]}),
    );

    let batch = fixture
        .engine
        .image_batch_edit(params(json!({
            "prompt": "素描风", "image_paths": [first, second], "model": SEEDREAM,
        })))
        .await
        .unwrap();
    fixture.assert_public_json(
        &batch,
        json!({"ok": false, "error": EDIT_REJECTION, "errors": [EDIT_REJECTION], "total": 2}),
    );

    let multi = fixture
        .engine
        .image_multi_reference(params(json!({
            "prompt": "融合成海报", "image_paths": [first, second], "model": SEEDREAM,
        })))
        .await
        .unwrap();
    fixture.assert_public_json(
        &multi,
        json!({"ok": false, "error": EDIT_REJECTION, "errors": [EDIT_REJECTION]}),
    );

    assert!(fixture.provider.calls().is_empty());
}

#[tokio::test]
async fn seedream_session_is_edited_only_with_an_explicit_edit_model() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let first = generate(
        &fixture,
        json!({
            "prompt": "画一只猫", "size": "1024x1024", "model": SEEDREAM,
            "basename": "v1", "session_id": "new",
        }),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    let session = session_id(&first);

    // The session's model is inherited, and it cannot edit: no silent switch to another model.
    let inherited = fixture
        .engine
        .image_edit(params(
            json!({"prompt": "加一顶帽子", "session_id": session, "basename": "v2"}),
        ))
        .await
        .unwrap();
    assert_eq!(inherited["ok"], false, "{inherited}");
    assert_eq!(inherited["error"], EDIT_REJECTION);
    assert_eq!(fixture.provider.calls().len(), 1);

    let explicit = fixture
        .engine
        .image_edit(params(json!({
            "prompt": "加一顶帽子", "session_id": session, "basename": "v2",
            "model": "gpt-image-2.6-sunburst",
        })))
        .await
        .unwrap();
    assert_eq!(explicit["ok"], true, "{explicit}");
    assert_eq!(explicit["session"]["revision"], 2);
    let calls = fixture.provider.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].endpoint, Endpoint::Edit);
    assert_eq!(calls[1].model, "gpt-image-2.6-sunburst");
}

#[tokio::test]
async fn continued_generation_with_explicit_seedream_is_rejected() {
    let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&rgb_32x24_png())));
    let first = generate(
        &fixture,
        json!({"prompt": "画一只猫", "size": "1024x1024", "session_id": "new"}),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");

    let continued = generate(
        &fixture,
        json!({"prompt": "换成夜景", "session_id": session_id(&first), "model": SEEDREAM}),
    )
    .await;

    assert_eq!(continued["ok"], false, "{continued}");
    assert_eq!(continued["error"], EDIT_REJECTION);
    assert_eq!(fixture.provider.calls().len(), 1);
}
