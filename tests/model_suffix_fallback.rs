use base64::{Engine as _, engine::general_purpose::STANDARD};
use micu_devtools::{
    Result,
    mock::{Capture, HttpFixture, body, header, multipart, png, respond_json},
    stdio::Session,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Fixture {
    http: HttpFixture,
    requests: Arc<Mutex<Vec<Capture>>>,
    gets: Arc<AtomicUsize>,
}
impl Fixture {
    fn new(
        first_status: u16,
        first_error: Value,
        fallback_status: u16,
        fallback_body: Option<Value>,
    ) -> Result<Self> {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let gets = Arc::new(AtomicUsize::new(0));
        let got = gets.clone();
        let image = STANDARD.encode(png(32, 24, 1)?);
        let http = HttpFixture::start(move |mut request| {
            if request.method().as_str() == "GET" {
                got.fetch_add(1, Ordering::SeqCst);
                return respond_json(request, 401, json!({}), &[]);
            }
            let content_type = header(&request, "Content-Type");
            let bytes = body(&mut request)?;
            let (fields, files) = if content_type.starts_with("multipart/") {
                multipart(&content_type, &bytes)?
            } else {
                (serde_json::from_slice(&bytes)?, Vec::new())
            };
            let fallback = fields["model"].as_str().unwrap().ends_with("-openai");
            captured.lock().unwrap().push(Capture {
                method: "POST".into(),
                path: request.url().into(),
                authorization_valid: header(&request, "Authorization") == "Bearer fixture-key",
                fields,
                files,
                scenario: "suffix".into(),
                attempt: 1,
            });
            if !fallback && (first_status != 200 || first_error.get("error").is_some()) {
                return respond_json(request, first_status, first_error.clone(), &[]);
            }
            if fallback {
                return respond_json(
                    request,
                    fallback_status,
                    fallback_body
                        .clone()
                        .unwrap_or_else(|| json!({"data":[{"b64_json":image}]})),
                    &[],
                );
            }
            respond_json(request, 200, json!({"data":[{"b64_json":image}]}), &[])
        })?;
        Ok(Self {
            http,
            requests,
            gets,
        })
    }
    fn server(&self, root: &Path, model: Option<&str>) -> Result<Session> {
        let mut env = vec![
            ("MICU_API_KEY", "fixture-key"),
            ("MICU_BASEURL", self.http.url.as_str()),
            ("MICU_CONTRACT_TESTING", "1"),
        ];
        if let Some(model) = model {
            env.push(("MICU_MODEL", model));
        }
        let mut server = Session::new(Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")), root, &env)?;
        server.initialize()?;
        Ok(server)
    }
}
fn missing_model() -> Value {
    json!({"error":{"code":"model_not_found", "param":"model", "message":"Model is not found"}})
}

#[test]
fn primary_success_does_not_probe_catalog_or_submit_fallback() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = Fixture::new(200, json!({}), 200, None)?;
    let mut server = api.server(root.path(), None)?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"red cube","session_id":"new"}),
    )?;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["model"], "gpt-image-2.5-flare");
    assert_eq!(result["used_fallback"], false);
    assert_eq!(api.requests.lock().unwrap().len(), 1);
    assert_eq!(api.gets.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn all_image_tools_fallback_once_preserving_fields_uploads_and_actual_model() -> Result<()> {
    for tool in [
        "image_generate",
        "image_edit",
        "image_batch_edit",
        "image_multi_reference",
    ] {
        let root = tempfile::tempdir()?;
        let a = root.path().join("a.png");
        let b = root.path().join("b.png");
        fs::write(&a, png(32, 24, 1)?)?;
        fs::write(&b, png(32, 24, 2)?)?;
        let api = Fixture::new(404, missing_model(), 200, None)?;
        // Exercise old configuration too: it must try the current unsuffixed ID first.
        let mut server = api.server(root.path(), Some("gpt-image-2.5-flare-openai"))?;
        let mut args = json!({"prompt":"red cube","quality":"low","image_path":a,"mask_path":b,"image_paths":[a,b]});
        if tool == "image_generate" || tool == "image_edit" {
            args["session_id"] = json!("new");
        }
        let result = server.call(tool, args)?;
        assert_eq!(result["ok"], true, "{tool}: {result}");
        let requests = api.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            if tool == "image_batch_edit" { 4 } else { 2 },
            "{tool}: {result}"
        );
        let primaries: Vec<_> = requests
            .iter()
            .filter(|r| r.fields["model"] == "gpt-image-2.5-flare")
            .collect();
        assert_eq!(primaries.len() * 2, requests.len());
        for before_request in primaries {
            let mut expected = before_request.fields.clone();
            expected["model"] = json!("gpt-image-2.5-flare-openai");
            let after_request = requests
                .iter()
                .find(|r| {
                    r.fields == expected
                        && r.files
                            .iter()
                            .map(|f| &f.bytes)
                            .eq(before_request.files.iter().map(|f| &f.bytes))
                })
                .unwrap();
            assert!(before_request.fields.get("response_format").is_none());
            assert_eq!(before_request.path, after_request.path);
            assert!(before_request.authorization_valid && after_request.authorization_valid);
            assert_eq!(before_request.files.len(), after_request.files.len());
            for (before, after) in before_request.files.iter().zip(&after_request.files) {
                assert_eq!(
                    (&before.name, &before.filename, &before.mime, &before.bytes),
                    (&after.name, &after.filename, &after.mime, &after.bytes)
                );
            }
        }
        if tool == "image_batch_edit" {
            for item in result["results"].as_array().unwrap() {
                assert_eq!(item["model"], "gpt-image-2.5-flare-openai");
                assert_eq!(item["used_fallback"], true);
            }
        } else {
            assert_eq!(result["model"], "gpt-image-2.5-flare-openai");
            assert_eq!(result["used_fallback"], true);
            assert!(
                result["notes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|note| note.as_str().unwrap().contains("-openai"))
            );
        }
        if let Some(id) = result["session"]["id"].as_str() {
            let record = fs::read_dir(root.path().join(".micu-sessions").join(id))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .find(|p| p.extension().is_some_and(|ext| ext == "json"))
                .unwrap();
            let revision: Value = serde_json::from_slice(&fs::read(record)?)?;
            assert_eq!(revision["model"], "gpt-image-2.5-flare-openai");
        }
        assert_eq!(api.gets.load(Ordering::SeqCst), 0);
        assert!(api.http.errors().is_empty());
    }
    Ok(())
}

#[test]
fn explicit_model_channel_rejection_skips_generic_503_retries() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = Fixture::new(
        503,
        json!({"error":{"message":"当前分组下对于模型 gpt-image-2.5-sunburst 无可用渠道"}}),
        200,
        None,
    )?;
    let mut server = api.server(root.path(), None)?;
    let a = root.path().join("a.png");
    fs::write(&a, png(32, 24, 1)?)?;
    let result = server.call("image_edit", json!({"prompt":"edit","image_path":a}))?;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["model"], "gpt-image-2.5-sunburst-openai");
    assert_eq!(api.requests.lock().unwrap().len(), 2);
    Ok(())
}

#[test]
fn fallback_failure_does_not_switch_back_or_reinterpret_postprocessing_errors() -> Result<()> {
    for response in [
        missing_model(),
        json!({"data":[]}),
        json!({"data":[{"b64_json":"invalid"}]}),
        json!({"data":[{"url":"http://127.0.0.1:9/private.png"}]}),
    ] {
        let root = tempfile::tempdir()?;
        let status = if response.get("error").is_some() {
            404
        } else {
            200
        };
        let api = Fixture::new(404, missing_model(), status, Some(response))?;
        let mut server = api.server(root.path(), None)?;
        let result = server.call("image_generate", json!({"prompt":"x"}))?;
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(api.requests.lock().unwrap().len(), 2, "{result}");
        assert_eq!(result["model"], "gpt-image-2.5-flare-openai");
    }
    Ok(())
}

#[test]
fn unrelated_errors_never_switch_models() -> Result<()> {
    for (status, error) in [
        (
            400,
            json!({"error":{"code":"invalid_parameter","param":"size","message":"Unsupported size for model gpt-image-2.5-flare"}}),
        ),
        (
            400,
            json!({"error":{"code":"insufficient_quota","message":"model gpt-image-2.5-flare unavailable: quota"}}),
        ),
        (
            400,
            json!({"error":{"code":"content_policy_violation","message":"unsupported model gpt-image-2.5-flare content"}}),
        ),
        (401, missing_model()),
        (403, missing_model()),
        (429, missing_model()),
        (404, json!({"error":{"message":"unknown endpoint"}})),
        (
            500,
            json!({"error":{"message":"model gpt-image-2.5-flare not found"}}),
        ),
        (503, json!({"error":{"message":"Service unavailable"}})),
    ] {
        let root = tempfile::tempdir()?;
        let api = Fixture::new(status, error, 200, None)?;
        let mut server = api.server(root.path(), None)?;
        let result = server.call("image_generate", json!({"prompt":"x"}))?;
        assert_eq!(result["ok"], false, "{status}: {result}");
        let requests = api.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "{status}: {result}");
        assert_eq!(requests[0].fields["model"], "gpt-image-2.5-flare");
    }
    Ok(())
}

#[test]
fn old_sunburst_session_records_actual_fallback_and_retries_primary_on_next_edit() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = Fixture::new(400, missing_model(), 200, None)?;
    let mut server = api.server(root.path(), Some("gpt-image-2.5-sunburst-openai"))?;
    let first = server.call(
        "image_generate",
        json!({"prompt":"subject","session_id":"new","quality":"max"}),
    )?;
    assert_eq!(first["ok"], true, "{first}");
    let id = first["session"]["id"].as_str().unwrap().to_owned();
    drop(server);
    let mut restarted = api.server(root.path(), None)?;
    let edited = restarted.call(
        "image_edit",
        json!({"prompt":"change background","session_id":id}),
    )?;
    assert_eq!(edited["ok"], true, "{edited}");
    assert_eq!(edited["model"], "gpt-image-2.5-sunburst-openai");
    assert_eq!(edited["session"]["revision"], 2);
    let requests = api.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for pair in requests.chunks(2) {
        assert_eq!(pair[0].fields["model"], "gpt-image-2.5-sunburst");
        assert_eq!(pair[1].fields["model"], "gpt-image-2.5-sunburst-openai");
        assert_eq!(pair[1].fields["quality"], "max");
    }
    Ok(())
}

#[test]
fn standard_and_historical_quality_routes_are_not_suffix_aliases() -> Result<()> {
    for model in ["gpt-image-2", "gpt-image-2-openai"] {
        let root = tempfile::tempdir()?;
        let api = Fixture::new(404, missing_model(), 200, None)?;
        let mut server = api.server(root.path(), None)?;
        let result = server.call("image_generate", json!({"prompt":"x","model":model}))?;
        assert_eq!(result["ok"], model == "gpt-image-2-openai", "{result}");
        assert_eq!(result["model"], model);
        assert_eq!(result["used_fallback"], false);
        let requests = api.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].fields["model"], model);
        assert_eq!(requests.last().unwrap().fields["response_format"], "url");
    }
    Ok(())
}

#[test]
fn network_failure_only_uses_existing_same_model_retry() -> Result<()> {
    let root = tempfile::tempdir()?;
    let models = Arc::new(Mutex::new(Vec::new()));
    let captured = models.clone();
    let http = HttpFixture::start(move |mut request| {
        let fields: Value = serde_json::from_slice(&body(&mut request)?)?;
        captured
            .lock()
            .unwrap()
            .push(fields["model"].as_str().unwrap().to_owned());
        // Closing without a response is a definite transport error, not a model rejection.
        drop(request);
        Ok(())
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_API_KEY", "fixture-key"), ("MICU_BASEURL", &http.url)],
    )?;
    server.initialize()?;
    let result = server.call("image_generate", json!({"prompt":"x"}))?;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(
        *models.lock().unwrap(),
        ["gpt-image-2.5-flare", "gpt-image-2.5-flare"]
    );
    Ok(())
}

#[test]
fn legacy_format_retry_preserves_standard_route_without_alias_fallback() -> Result<()> {
    let root = tempfile::tempdir()?;
    let models = Arc::new(Mutex::new(Vec::new()));
    let captured = models.clone();
    let http = HttpFixture::start(move |mut request| {
        let fields: Value = serde_json::from_slice(&body(&mut request)?)?;
        captured
            .lock()
            .unwrap()
            .push(fields["model"].as_str().unwrap().to_owned());
        if fields["response_format"] == "url" {
            respond_json(
                request,
                200,
                json!({"data":[{"url":"http://127.0.0.1:9/private.png"}]}),
                &[],
            )
        } else {
            respond_json(request, 404, missing_model(), &[])
        }
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_API_KEY", "fixture-key"), ("MICU_BASEURL", &http.url)],
    )?;
    server.initialize()?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"x","model":"gpt-image-2"}),
    )?;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(*models.lock().unwrap(), ["gpt-image-2", "gpt-image-2"]);
    assert_eq!(result["used_fallback"], false);
    Ok(())
}

#[test]
fn model_refusal_requires_consistent_model_and_error_context() -> Result<()> {
    let model = "gpt-image-2.5-flare";
    let cases = [
        (
            "missing_model_code_only",
            404,
            json!({"error":{"code":"model_not_found","param":"model"}}),
            true,
        ),
        (
            "generic_model_message_with_code",
            400,
            missing_model(),
            true,
        ),
        (
            "permission_text",
            400,
            json!({"error":{"message":format!("Model {model} not found: permission denied")}}),
            false,
        ),
        (
            "authentication_text",
            400,
            json!({"error":{"type":"new_api_error","message":format!("Model {model} not found: authentication required")}}),
            false,
        ),
        (
            "permission_code_conflict",
            400,
            json!({"error":{"code":"model_not_found","param":"model","message":format!("Permission denied: access to {model} is disabled")}}),
            false,
        ),
        (
            "background_condition",
            400,
            json!({"error":{"message":format!("Model {model} is not supported with background=transparent")}}),
            false,
        ),
        (
            "seed_parameter_code_conflict",
            400,
            json!({"error":{"code":"unsupported_model","message":"Unsupported seed parameter"}}),
            false,
        ),
        (
            "other_model_code_conflict",
            404,
            json!({"error":{"code":"model_not_found","param":"model","message":"Model gpt-image-2.5-sunburst not found"}}),
            false,
        ),
        (
            "other_model_without_code",
            400,
            json!({"error":{"message":"Model gpt-image-2.5-sunburst not found"}}),
            false,
        ),
        (
            "quality_named_group",
            404,
            json!({"error":{"code":"model_not_found","type":"new_api_error","message":format!("当前分组 image_quality 下对于模型 {model} 无可用渠道")}}),
            true,
        ),
        (
            "prompt_named_group",
            404,
            json!({"error":{"code":"model_not_found","message":format!("当前分组 prompt_vip 下对于模型 {model} 无可用渠道")}}),
            true,
        ),
        (
            "group_without_code",
            503,
            json!({"error":{"message":format!("当前分组 prompt_quality_vip 下对于模型 {model} 无可用渠道")}}),
            true,
        ),
        (
            "ordinary_group",
            503,
            json!({"error":{"code":"","type":"new_api_error","message":format!("当前分组 vip_2_image 下对于模型 {model} 无可用渠道")}}),
            true,
        ),
        (
            "parameter_field",
            400,
            json!({"error":{"code":"model_not_found","param":"background","message":format!("Model {model} not found")}}),
            false,
        ),
        (
            "malformed_error",
            400,
            json!({"error":[{"code":"model_not_found"}]}),
            false,
        ),
        (
            "nested_error",
            400,
            json!({"error":{"error":{"code":"model_not_found"}}}),
            false,
        ),
        (
            "success_error_shape",
            200,
            json!({"error":{"code":"model_not_found"}}),
            false,
        ),
        (
            "different_exact_model_alias",
            404,
            json!({"error":{"code":"model_not_found","message":format!("Model {model}-openai not found")}}),
            false,
        ),
        (
            "permission_type_conflict",
            400,
            json!({"error":{"code":"model_not_found","type":"permission_denied","message":format!("Model {model} not found")}}),
            false,
        ),
        (
            "parameter_parenthesis",
            400,
            json!({"error":{"code":"model_not_found","message":format!("Model {model} not found (seed parameter is unsupported)")}}),
            false,
        ),
        (
            "request_trace",
            404,
            json!({"error":{"code":"model_not_found","message":format!("Model {model} not found (request id: fixture-123)")}}),
            true,
        ),
        (
            "fake_request_trace",
            400,
            json!({"error":{"code":"model_not_found","message":format!("Model {model} not found (request id: permission denied)")}}),
            false,
        ),
        (
            "malformed_message",
            400,
            json!({"error":{"code":"model_not_found","message":{"reason":"permission denied"}}}),
            false,
        ),
    ];
    for (name, status, error, expected) in cases {
        assert_eq!(
            micu_image_mcp::http::response::model_rejected(
                status,
                &serde_json::to_vec(&error)?,
                model
            ),
            expected,
            "{name}"
        );
        let root = tempfile::tempdir()?;
        let api = Fixture::new(status, error, 200, None)?;
        let mut server = api.server(root.path(), None)?;
        let result = server.call(
            "image_generate",
            json!({"prompt":"context regression","session_id":"new"}),
        )?;
        assert_eq!(result["used_fallback"], expected, "{name}: {result}");
        let requests = api.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            if expected { 2 } else { 1 },
            "{name}: {result}"
        );
        assert_eq!(requests[0].fields["model"], model);
        if expected {
            assert_eq!(requests[1].fields["model"], format!("{model}-openai"));
        }
        assert_eq!(api.gets.load(Ordering::SeqCst), 0);
        assert!(api.http.errors().is_empty(), "{name}");
    }
    Ok(())
}
