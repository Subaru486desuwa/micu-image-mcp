use micu_devtools::{
    Result,
    mock::{MockMicu, png},
    stdio::Session,
};
use serde_json::json;
use std::{fs, path::Path};

#[test]
fn default_flare_uses_images_api_without_response_format() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    let result = server.call("image_generate", json!({"prompt":"a red cube"}))?;
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/images/generations");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(requests[0].fields["model"], "gpt-image-2.5-flare-openai");
    assert!(requests[0].fields.get("response_format").is_none());
    assert!(Path::new(result["saved"][0]["path"].as_str().unwrap()).is_file());
    Ok(())
}

#[test]
fn every_flare_tool_omits_response_format_even_with_explicit_configuration() -> Result<()> {
    for format in ["auto", "url", "b64_json"] {
        for model in [None, Some("gpt-image-2.5-flare")] {
            let root = tempfile::tempdir()?;
            let api = MockMicu::new()?;
            let a = root.path().join("a.png");
            let b = root.path().join("b.png");
            fs::write(&a, png(32, 24, 1)?)?;
            fs::write(&b, png(32, 24, 2)?)?;
            let mut env = api.env();
            env.extend([
                ("MICU_RESPONSE_FORMAT", format),
                ("MICU_MODEL", "gpt-image-2.5-flare-openai"),
            ]);
            let mut server = Session::new(
                Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
                root.path(),
                &env,
            )?;
            server.initialize()?;
            for tool in [
                "image_generate",
                "image_edit",
                "image_batch_edit",
                "image_multi_reference",
            ] {
                let before = api.requests().len();
                let result = server.call(tool, json!({"prompt":"preserve the red cube", "model":model, "image_path":a, "mask_path":b, "image_paths":[a,b]}))?;
                assert_eq!(result["ok"], true, "{format} {model:?} {tool}: {result}");
                let requests = api.requests();
                let sent = &requests[before..];
                assert_eq!(sent.len(), if tool == "image_batch_edit" { 2 } else { 1 });
                for request in sent {
                    assert_eq!(
                        request.path,
                        if tool == "image_generate" {
                            "/v1/images/generations"
                        } else {
                            "/v1/images/edits"
                        }
                    );
                    assert_eq!(request.fields["model"], "gpt-image-2.5-flare-openai");
                    assert!(request.fields.get("response_format").is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn flare_output_failures_do_not_regenerate_identical_paid_requests() -> Result<()> {
    for scenario in [
        "invalid_json",
        "no_image",
        "url_truncated_then_b64",
        "url_redirect_private_then_b64",
    ] {
        let root = tempfile::tempdir()?;
        let api = MockMicu::new()?;
        let a = root.path().join("a.png");
        let b = root.path().join("b.png");
        fs::write(&a, png(32, 24, 1)?)?;
        fs::write(&b, png(32, 24, 2)?)?;
        let mut env = api.proxy_env();
        env.push(("MICU_MODEL", "gpt-image-2.5-flare-openai"));
        let mut server = Session::new(
            Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root.path(),
            &env,
        )?;
        server.initialize()?;
        for tool in [
            "image_generate",
            "image_edit",
            "image_batch_edit",
            "image_multi_reference",
        ] {
            let before = api.requests().len();
            let result = server.call(tool, json!({"prompt":format!("[scenario:{scenario}] failure"), "image_path":a, "image_paths":[a,b]}))?;
            assert_eq!(result["ok"], false, "{scenario} {tool}: {result}");
            let requests = api.requests();
            let posts: Vec<_> = requests[before..]
                .iter()
                .filter(|request| request.method == "POST")
                .collect();
            assert_eq!(
                posts.len(),
                if tool == "image_batch_edit" { 2 } else { 1 },
                "{scenario} {tool}: {result}"
            );
            for request in posts {
                assert_eq!(
                    request.path,
                    if tool == "image_generate" {
                        "/v1/images/generations"
                    } else {
                        "/v1/images/edits"
                    }
                );
                assert!(request.fields.get("response_format").is_none());
            }
        }
    }
    Ok(())
}

#[test]
fn legacy_models_keep_their_configured_response_format() -> Result<()> {
    for format in ["auto", "url", "b64_json"] {
        for model in [
            "gpt-image-2",
            "gpt-image-2-openai",
            "gpt-image-2.5-sunburst",
        ] {
            let root = tempfile::tempdir()?;
            let api = MockMicu::new()?;
            let image = root.path().join("source.png");
            fs::write(&image, png(32, 24, 1)?)?;
            let mut env = api.env();
            env.push(("MICU_RESPONSE_FORMAT", format));
            let mut server = Session::new(
                Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
                root.path(),
                &env,
            )?;
            server.initialize()?;
            for tool in ["image_generate", "image_edit"] {
                let result = server.call(
                    tool,
                    json!({"prompt":"legacy format", "model":model, "image_path":image}),
                )?;
                assert_eq!(result["ok"], true, "{result}");
                let requests = api.requests();
                let sent = requests.last().unwrap();
                assert_eq!(sent.fields["model"], model);
                assert_eq!(
                    sent.fields["response_format"],
                    if format == "auto" { "url" } else { format }
                );
            }
        }
    }
    Ok(())
}
