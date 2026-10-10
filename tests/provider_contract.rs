use micu_devtools::{
    Result,
    mock::{MockMicu, png},
    stdio::Session,
};
use serde_json::json;
use std::path::Path;

#[test]
fn generation_sends_utf8_and_current_model_without_an_omitted_quality_field() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"中文小猫 café", "basename":"first"}),
    )?;
    assert_eq!(result["ok"], true);
    let requests = api.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/images/generations");
    assert!(requests[0].authorization_valid);
    assert_eq!(requests[0].fields["prompt"], "中文小猫 café");
    assert_eq!(requests[0].fields["model"], "gpt-image-2.5-flare");
    assert!(requests[0].fields.get("quality").is_none());
    assert!(Path::new(result["saved"][0]["path"].as_str().unwrap()).is_file());
    Ok(())
}

#[test]
fn current_models_qualities_sizes_and_large_routing_are_verified_on_the_wire() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    for model in [
        "gpt-image-2",
        "gpt-image-2-openai",
        "gpt-image-2.5-flare",
        "gpt-image-2.5-sunburst",
    ] {
        let qualities: &[Option<&str>] = if model.contains("2.5") {
            &[
                None,
                Some("auto"),
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("xhigh"),
                Some("max"),
            ]
        } else {
            &[
                None,
                Some("auto"),
                Some("low"),
                Some("medium"),
                Some("high"),
            ]
        };
        for quality in qualities {
            let mut args = json!({"prompt":"parameter matrix","model":model,"size":"1024x1024"});
            if let Some(quality) = quality {
                args["quality"] = json!(quality);
            }
            let result = server.call("image_generate", args)?;
            assert_eq!(result["ok"], true);
            let requests = api.requests();
            let sent = requests.last().unwrap();
            assert_eq!(sent.fields["model"], model);
            assert_eq!(sent.fields["size"], "1024x1024");
            assert_eq!(
                sent.fields.get("quality").and_then(|value| value.as_str()),
                *quality
            );
        }
    }
    for size in [
        "1024x1024",
        "1280x720",
        "1536x1024",
        "2048x1152",
        "2048x2048",
        "3840x2160",
    ] {
        for model in ["gpt-image-2", "gpt-image-2.5-flare"] {
            let result = server.call(
                "image_generate",
                json!({"prompt":"route","model":model,"size":size,"n":3}),
            )?;
            assert_eq!(result["ok"], true);
            let large = size.starts_with("2048") || size.starts_with("3840");
            let expected = if large && model == "gpt-image-2" {
                "gpt-image-2-openai"
            } else {
                model
            };
            assert_eq!(result["model"], expected);
            assert_eq!(
                result["saved"].as_array().unwrap().len(),
                if large { 1 } else { 3 }
            );
            assert_eq!(api.requests().last().unwrap().fields["model"], expected);
        }
    }
    Ok(())
}

#[test]
fn unsupported_models_and_invalid_parameters_never_reach_http() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    let mut cases = vec![
        json!({"prompt":""}),
        json!({"n":0}),
        json!({"n":11}),
        json!({"basename":"../escape"}),
        json!({"quality":"invalid"}),
    ];
    for model in [
        "gpt-image-2-pro",
        "gpt-image-2-key",
        " gpt-image-2 ",
        "dall-e-3",
        "grok-imagine-image",
    ] {
        cases.push(json!({"model":model}));
    }
    for size in [
        "1920x1080",
        "256x256",
        "4096x2160",
        "3840x1024",
        "3840x3840",
    ] {
        cases.push(json!({"size":size}));
    }
    for extra in cases {
        let mut args = json!({"prompt":"invalid"});
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let result = server.call("image_generate", args)?;
        assert_eq!(result["ok"], false);
        assert!(api.requests().is_empty());
    }
    for args in [
        json!({"prompt":4}),
        json!({"prompt":"valid","n":1.5}),
        json!({"prompt":"valid","size":false}),
    ] {
        let response = server.request(
            "tools/call",
            json!({"name":"image_generate","arguments":args}),
        )?;
        assert_eq!(response["result"]["isError"], true);
    }
    let result = server.call("image_generate", json!({"prompt":"bool coercion","n":true}))?;
    assert_eq!(result["requested_n"], 1);
    Ok(())
}

#[test]
fn data_urls_downloads_and_auto_fallback_preserve_security_and_saved_results() -> Result<()> {
    for scenario in [
        "exact_b64",
        "data_url",
        "url_success",
        "url_no_content_length",
        "url_truncated_then_b64",
        "url_redirect_private_then_b64",
    ] {
        let root = tempfile::tempdir()?;
        let api = MockMicu::new()?;
        let mut server = Session::new(
            Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root.path(),
            &api.proxy_env(),
        )?;
        server.initialize()?;
        let result = server.call(
            "image_generate",
            json!({"prompt":format!("[scenario:{scenario}] save"),"size":"1024x1024"}),
        )?;
        assert_eq!(result["ok"], true, "scenario {scenario}: {result}");
        let path = Path::new(result["saved"][0]["path"].as_str().unwrap());
        assert!(
            path.canonicalize()?
                .starts_with(root.path().canonicalize()?)
        );
        assert!(image::open(path).is_ok());
        assert_eq!(result["size_honored"], scenario == "exact_b64");
        let requests = api.requests();
        for download in requests.iter().filter(|capture| capture.method == "GET") {
            assert!(
                download.authorization_valid,
                "download forwarded API credentials"
            );
        }
        if scenario.contains("then_b64") {
            let posts: Vec<_> = requests
                .iter()
                .filter(|capture| capture.method == "POST")
                .collect();
            assert_eq!(posts.len(), 2);
            assert_eq!(posts[0].fields["response_format"], "url");
            assert_eq!(posts[1].fields["response_format"], "b64_json");
        }
    }
    for scenario in ["url_loopback", "url_mapped_loopback"] {
        let root = tempfile::tempdir()?;
        let api = MockMicu::new()?;
        let mut env = api.env();
        env.push(("MICU_RESPONSE_FORMAT", "url"));
        let mut server = Session::new(
            Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root.path(),
            &env,
        )?;
        server.initialize()?;
        let result = server.call(
            "image_generate",
            json!({"prompt":format!("[scenario:{scenario}] reject")}),
        )?;
        assert_eq!(result["ok"], false);
        assert_eq!(api.requests().len(), 1);
    }
    Ok(())
}

#[test]
fn retries_errors_and_response_limits_are_verified_at_the_http_boundary() -> Result<()> {
    for scenario in [
        "retry_400_too_many",
        "retry_408",
        "retry_429",
        "retry_500",
        "retry_after_seconds",
        "retry_after_http_date",
        "disconnect_once",
        "disconnect_body_once",
        "secret_error",
        "invalid_json",
        "no_image",
        "content_length_too_large",
        "stream_too_large",
        "http_524",
    ] {
        let root = tempfile::tempdir()?;
        let api = MockMicu::new()?;
        let mut server = Session::new(
            Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root.path(),
            &api.env(),
        )?;
        server.initialize()?;
        let success = scenario.starts_with("retry_") || scenario.starts_with("disconnect_");
        let size = if scenario == "http_524" {
            "2048x2048"
        } else {
            "1024x1024"
        };
        let model = if scenario.starts_with("retry_") {
            "gpt-image-2-openai"
        } else {
            "gpt-image-2.5-flare"
        };
        let result = server.call(
            "image_generate",
            json!({"prompt":format!("[scenario:{scenario}] boundary"),"size":size,"model":model}),
        )?;
        assert_eq!(result["ok"], success, "scenario {scenario}: {result}");
        if matches!(scenario, "content_length_too_large" | "stream_too_large") {
            assert!(
                result["errors"][0]
                    .as_str()
                    .is_some_and(|error| error.contains("HTTP 413")),
                "scenario {scenario}: response must be rejected by the size limit; errors={}, notes={}, fixture_errors={:?}",
                result["errors"]
                    .to_string()
                    .replace(micu_devtools::mock::FIXTURE_KEY, "<fixture-key>"),
                result["notes"]
                    .to_string()
                    .replace(micu_devtools::mock::FIXTURE_KEY, "<fixture-key>"),
                api.http.errors()
            );
        }
        assert_eq!(
            api.requests().len(),
            if success || matches!(scenario, "invalid_json" | "no_image") {
                2
            } else {
                1
            },
            "scenario {scenario}: errors={}, notes={}, fixture_errors={:?}",
            result["errors"]
                .to_string()
                .replace(micu_devtools::mock::FIXTURE_KEY, "<fixture-key>"),
            result["notes"]
                .to_string()
                .replace(micu_devtools::mock::FIXTURE_KEY, "<fixture-key>"),
            api.http.errors()
        );
        assert!(
            !result
                .to_string()
                .contains(micu_devtools::mock::FIXTURE_KEY)
        );
        assert!(!result.to_string().contains(&"A".repeat(200)));
    }
    Ok(())
}

#[test]
fn multi_reference_mask_and_batch_upload_real_files_with_current_edits_endpoint() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let a = root.path().join("a.png");
    let b = root.path().join("b.png");
    let mask = root.path().join("mask.png");
    for (path, seed) in [(&a, 1), (&b, 2), (&mask, 3)] {
        std::fs::write(path, png(32, 24, seed)?)?;
    }
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    let edited = server.call(
        "image_edit",
        json!({"prompt":"mask edit","image_path":a,"mask_path":mask}),
    )?;
    assert_eq!(edited["ok"], true);
    let requests = api.requests();
    assert_eq!(
        requests[0]
            .files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>(),
        ["image", "mask"]
    );
    let merged = server.call(
        "image_multi_reference",
        json!({"prompt":"merge","image_paths":[a,b]}),
    )?;
    assert_eq!(merged["ok"], true);
    let requests = api.requests();
    let last = requests.last().unwrap();
    assert_eq!(last.path, "/v1/images/edits");
    assert_eq!(
        last.files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>(),
        ["image[]", "image[]"]
    );
    assert_eq!(last.files[0].mime, "image/png");
    assert_eq!(last.files[1].bytes, std::fs::read(&b)?);
    let batch = server.call(
        "image_batch_edit",
        json!({"prompt":"batch","image_paths":[a,b]}),
    )?;
    assert_eq!(batch["ok"], true);
    assert_eq!(batch["failed"], 0);
    assert_eq!(batch["results"].as_array().unwrap().len(), 2);
    for result in batch["results"].as_array().unwrap() {
        assert_eq!(result["ok"], true);
    }
    Ok(())
}

#[test]
fn reference_edits_send_the_selected_image_as_multipart() -> Result<()> {
    let root = tempfile::tempdir()?;
    let image = root.path().join("reference.png");
    let bytes = png(32, 24, 45)?;
    std::fs::write(&image, &bytes)?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    let result = server.call(
        "image_edit",
        json!({"prompt":"保留人物", "image_path":image, "quality":"low"}),
    )?;
    assert_eq!(result["ok"], true);
    let requests = api.requests();
    assert_eq!(requests[0].path, "/v1/images/edits");
    assert_eq!(requests[0].fields["model"], "gpt-image-2.5-sunburst");
    assert_eq!(requests[0].fields["quality"], "low");
    assert_eq!(requests[0].files[0].name, "image");
    assert_eq!(requests[0].files[0].bytes, bytes);
    Ok(())
}

#[test]
fn timeout_uses_one_free_network_retry_and_quality_batch_has_a_real_start_gap() -> Result<()> {
    let binary = Path::new(env!("CARGO_BIN_EXE_micu-image-mcp"));
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut env = api.env();
    env.push(("MICU_TEST_API_TIMEOUT_MS", "50"));
    let mut server = Session::new(binary, root.path(), &env)?;
    server.initialize()?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"[scenario:api_timeout] timeout"}),
    )?;
    assert_eq!(result["ok"], false);
    assert_eq!(api.requests().len(), 2);
    drop(server);
    drop(api);
    let api = MockMicu::new()?;
    let image = root.path().join("source.png");
    std::fs::write(&image, png(32, 24, 1)?)?;
    let mut server = Session::new(binary, root.path(), &api.env())?;
    server.initialize()?;
    let result=server.call("image_batch_edit",json!({"prompt":"[scenario:concurrency_probe] batch","image_paths":[image,image],"model":"gpt-image-2-openai"}))?;
    assert_eq!(result["ok"], true);
    assert_eq!(api.requests().len(), 2);
    assert_eq!(api.metrics().max_active, 1);
    assert!(api.metrics().gaps_seconds[0] >= 1.3);
    Ok(())
}
