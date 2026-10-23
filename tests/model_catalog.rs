use micu_devtools::{
    Result,
    mock::{HttpFixture, MockMicu, respond_json},
    stdio::Session,
};
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[test]
fn manual_directory_never_fetches_upstream_and_removes_grok_fields() -> Result<()> {
    let root = tempfile::tempdir()?;
    let gets = Arc::new(AtomicUsize::new(0));
    let count = gets.clone();
    let api = HttpFixture::start(move |request| {
        count.fetch_add(1, Ordering::SeqCst);
        respond_json(request, 401, json!({"error":"must not query catalog"}), &[])
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_BASEURL", &api.url), ("MICU_API_KEY", "fixture")],
    )?;
    server.initialize()?;
    for _ in 0..3 {
        let info = server.call("server_info", json!({}))?;
        assert_eq!(
            info["available_models"],
            json!([
                "gpt-image-2",
                "gpt-image-2-openai",
                "gpt-image-2.5-flare",
                "gpt-image-2.5-sunburst"
            ])
        );
        assert_eq!(info["model_catalog"]["source"], "manual");
        assert_eq!(info["model_catalog"]["refresh_status"], "not_applicable");
        assert!(info["model_catalog"]["last_success_unix"].is_null());
        assert!(!info.to_string().to_lowercase().contains("grok"));
    }
    assert_eq!(gets.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn unknown_manual_models_are_rejected_without_echoing_credentials() -> Result<()> {
    let key = "sk-fixture-secret";
    let root = tempfile::tempdir()?;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let api = HttpFixture::start(move |request| {
        count.fetch_add(1, Ordering::SeqCst);
        respond_json(request, 500, json!({}), &[])
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_BASEURL", &api.url), ("MICU_API_KEY", key)],
    )?;
    server.initialize()?;
    for model in [
        "gpt-image-2.7-new".to_owned(),
        format!("gpt-image-2.5-{key}"),
    ] {
        let result = server.call("image_generate", json!({"prompt":"x", "model":model}))?;
        assert_eq!(result["ok"], false);
        assert!(!result.to_string().contains(key));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn manual_directory_preserves_explicit_legacy_quality_route() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    for (model, size) in [
        ("gpt-image-2", "2048x1152"),
        ("gpt-image-2-openai", "1024x1024"),
    ] {
        let result = server.call(
            "image_generate",
            json!({"prompt":"x", "model":model,"size":size}),
        )?;
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["model"], "gpt-image-2-openai");
        assert_eq!(
            api.requests().last().unwrap().fields["model"],
            "gpt-image-2-openai"
        );
    }
    Ok(())
}
