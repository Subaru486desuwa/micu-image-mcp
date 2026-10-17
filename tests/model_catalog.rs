use micu_devtools::{
    Result,
    mock::{HttpFixture, respond},
    stdio::Session,
};
use serde_json::json;
use std::path::Path;

#[test]
fn online_directory_discovers_new_image_ids_and_removes_grok_fields() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = HttpFixture::start(|request| {
        respond(
            request,
            200,
            serde_json::to_vec(
                &json!({"data":[{"id":"gpt-image-2.5-new"},{"id":"grok-imagine-image"},{"id":"text-model"},{"id":"gpt-image-2.5-chat","supported_endpoint_types":["chat"]}]}),
            )?,
            "application/json",
            &[],
        )
    })?;
    let base = api.url.clone();
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_BASEURL", base.as_str()), ("MICU_API_KEY", "fixture")],
    )?;
    server.initialize()?;
    let info = server.call("server_info", json!({}))?;
    assert_eq!(
        info["available_models"],
        json!(["gpt-image-2.5-new"]),
        "{info}"
    );
    assert_eq!(info["model_catalog"]["source"], "online");
    assert!(!info.to_string().to_lowercase().contains("grok"));
    Ok(())
}

use base64::Engine;
use micu_devtools::mock::{png, respond_json};
use serde_json::Value;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

struct CatalogFixture {
    api: HttpFixture,
    response: Arc<Mutex<(u16, Value)>>,
    gets: Arc<AtomicUsize>,
    posts: Arc<Mutex<Vec<String>>>,
}
impl CatalogFixture {
    fn new(value: Value) -> Result<Self> {
        let response = Arc::new(Mutex::new((200, value)));
        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(Mutex::new(Vec::new()));
        let (reply, count, sent) = (response.clone(), gets.clone(), posts.clone());
        let image = base64::engine::general_purpose::STANDARD.encode(png(32, 24, 1)?);
        let api = HttpFixture::start(move |mut request| {
            if request.method().as_str() == "GET" {
                count.fetch_add(1, Ordering::SeqCst);
                let (status, value) = reply.lock().unwrap().clone();
                return respond_json(request, status, value, &[]);
            }
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body)?;
            sent.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            respond_json(request, 200, json!({"data":[{"b64_json":image}]}), &[])
        })?;
        Ok(Self {
            api,
            response,
            gets,
            posts,
        })
    }
    fn server(&self, root: &Path, ttl: &str) -> Result<Session> {
        let mut server = Session::new(
            Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
            root,
            &[
                ("MICU_BASEURL", &self.api.url),
                ("MICU_API_KEY", "fixture"),
                ("MICU_TEST_CATALOG_TTL_MS", ttl),
            ],
        )?;
        server.initialize()?;
        Ok(server)
    }
}

#[test]
fn newly_discovered_model_is_sent_without_repeated_image_submission() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(
        json!({"data":[{"id":"gpt-image-2.7-new","supported_endpoint_types":["openai"]}]}),
    )?;
    let mut server = api.server(root.path(), "3600000")?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"red cube","model":"gpt-image-2.7-new","session_id":"new"}),
    )?;
    assert_eq!(result["ok"], true, "{result}");
    let posts = api.posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    let body: Value = serde_json::from_str(&posts[0])?;
    assert_eq!(body["model"], "gpt-image-2.7-new");
    assert!(body.get("response_format").is_none());
    drop(posts);
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["available_models"], json!(["gpt-image-2.7-new"]));
    assert_eq!(api.gets.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn successful_empty_directory_rejects_old_models_before_image_io() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[]}))?;
    let mut server = api.server(root.path(), "3600000")?;
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["available_models"], json!([]));
    assert_eq!(info["model_catalog"]["source"], "online");
    let result = server.call(
        "image_edit",
        json!({"prompt":"edit","image_path":"missing.png"}),
    )?;
    assert_eq!(result["ok"], false);
    assert!(result["error"].as_str().unwrap().contains("不支持 model="));
    assert!(api.posts.lock().unwrap().is_empty());
    Ok(())
}

#[test]
fn refresh_replaces_directory_then_failure_preserves_stale_success() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":"gpt-image-2.5-one"}]}))?;
    let mut server = api.server(root.path(), "30")?;
    let first = server.call("server_info", json!({}))?;
    assert_eq!(first["model_catalog"]["source"], "online");
    *api.response.lock().unwrap() = (200, json!({"data":[{"id":"gpt-image-2.5-two"}]}));
    std::thread::sleep(Duration::from_millis(50));
    let second = server.call("server_info", json!({}))?;
    assert_eq!(second["available_models"], json!(["gpt-image-2.5-two"]));
    *api.response.lock().unwrap() = (401, json!({"error":"do-not-expose-upstream-error"}));
    std::thread::sleep(Duration::from_millis(50));
    let stale = server.call("server_info", json!({}))?;
    assert_eq!(stale["model_catalog"]["source"], "stale_cache");
    assert_eq!(stale["available_models"], second["available_models"]);
    assert_eq!(
        stale["model_catalog"]["last_success_unix"],
        second["model_catalog"]["last_success_unix"]
    );
    assert!(!stale.to_string().contains("do-not-expose"));
    Ok(())
}

#[test]
fn failed_or_malformed_catalog_uses_explicit_offline_baseline() -> Result<()> {
    for (status, value) in [
        (401, json!({"error":"secret-auth"})),
        (200, json!({"data":"bad"})),
        (200, json!({"data":[{}]})),
    ] {
        let root = tempfile::tempdir()?;
        let api = CatalogFixture::new(value)?;
        api.response.lock().unwrap().0 = status;
        let mut server = api.server(root.path(), "3600000")?;
        let info = server.call("server_info", json!({}))?;
        assert_eq!(info["model_catalog"]["source"], "offline_baseline");
        assert!(
            info["available_models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| id == "gpt-image-2.5-flare-openai")
        );
        assert!(!info.to_string().contains("secret-auth"));
    }
    Ok(())
}

#[test]
fn override_credentials_do_not_reuse_default_directory() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":"gpt-image-2.5-a"}]}))?;
    let mut server = api.server(root.path(), "3600000")?;
    server.call("server_info", json!({}))?;
    *api.response.lock().unwrap() = (200, json!({"data":[{"id":"gpt-image-2.5-b"}]}));
    let result = server.call(
        "image_generate",
        json!({"prompt":"x","model":"gpt-image-2.5-b","api_key":"other-fixture"}),
    )?;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(api.gets.load(Ordering::SeqCst), 2);
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["available_models"], json!(["gpt-image-2.5-a"]));
    Ok(())
}

#[test]
fn routing_cannot_submit_a_model_absent_from_online_directory() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":"gpt-image-2"}]}))?;
    let mut server = api.server(root.path(), "3600000")?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"x","model":"gpt-image-2","size":"2048x1152"}),
    )?;
    assert_eq!(result["ok"], false, "{result}");
    assert!(api.posts.lock().unwrap().is_empty());
    Ok(())
}

#[test]
fn simultaneous_first_calls_share_one_directory_query() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":"gpt-image-2.5-new"}]}))?;
    let mut server = api.server(root.path(), "3600000")?;
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(server.send("tools/call", json!({"name":"server_info","arguments":{}}))?);
    }
    for id in ids {
        let response = server.wait(id)?;
        let text: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())?;
        assert_eq!(text, response["result"]["structuredContent"]);
        assert_eq!(text["model_catalog"]["source"], "online");
    }
    assert_eq!(api.gets.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn sunburst_legacy_configuration_and_session_use_authenticated_current_id() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":"gpt-image-2.5-sunburst-openai"}]}))?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[
            ("MICU_BASEURL", &api.api.url),
            ("MICU_API_KEY", "fixture"),
            ("MICU_MODEL", "gpt-image-2.5-sunburst"),
        ],
    )?;
    server.initialize()?;
    let first = server.call("image_generate", json!({"prompt":"x","session_id":"new"}))?;
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["model"], "gpt-image-2.5-sunburst-openai");
    let id = first["session"]["id"].as_str().unwrap();
    let second = server.call(
        "image_generate",
        json!({"prompt":"blue","session_id":id,"model":"gpt-image-2.5-sunburst"}),
    )?;
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["model"], "gpt-image-2.5-sunburst-openai");
    assert_eq!(api.posts.lock().unwrap().len(), 2);
    Ok(())
}

#[test]
fn discovery_timeout_is_bounded_and_does_not_block_offline_baseline() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = HttpFixture::start(|request| {
        std::thread::sleep(Duration::from_millis(100));
        // The client may have closed the timed-out socket already.
        let _result = respond_json(request, 200, json!({"data":[]}), &[]);
        Ok(())
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[
            ("MICU_BASEURL", &api.url),
            ("MICU_API_KEY", "fixture"),
            ("MICU_TEST_API_TIMEOUT_MS", "10"),
        ],
    )?;
    server.initialize()?;
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["model_catalog"]["source"], "offline_baseline");
    Ok(())
}

#[test]
fn modern_output_failure_does_not_submit_an_identical_paid_request() -> Result<()> {
    let root = tempfile::tempdir()?;
    let posts = Arc::new(AtomicUsize::new(0));
    let count = posts.clone();
    let api = HttpFixture::start(move |request| {
        if request.method().as_str() == "GET" {
            return respond_json(
                request,
                200,
                json!({"data":[{"id":"gpt-image-2.5-new"}]}),
                &[],
            );
        }
        count.fetch_add(1, Ordering::SeqCst);
        respond_json(request, 200, json!({"data":[]}), &[])
    })?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_BASEURL", &api.url), ("MICU_API_KEY", "fixture")],
    )?;
    server.initialize()?;
    let result = server.call(
        "image_generate",
        json!({"prompt":"x","model":"gpt-image-2.5-new"}),
    )?;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(posts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn upstream_reflected_credentials_are_not_exposed_or_accepted_as_models() -> Result<()> {
    let key = "sk-fixture-secret";
    let reflected = format!("gpt-image-2.5-{key}");
    let root = tempfile::tempdir()?;
    let api = CatalogFixture::new(json!({"data":[{"id":reflected},{"id":"gpt-image-2.5-safe"}]}))?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[("MICU_BASEURL", &api.api.url), ("MICU_API_KEY", key)],
    )?;
    server.initialize()?;
    let info = server.request("tools/call", json!({"name":"server_info","arguments":{}}))?;
    assert!(
        !info.to_string().contains(key),
        "server_info exposed reflected credential"
    );
    assert_eq!(
        info["result"]["structuredContent"]["available_models"],
        json!(["gpt-image-2.5-safe"])
    );
    for model in ["unavailable", reflected.as_str()] {
        let result = server.request(
            "tools/call",
            json!({"name":"image_generate","arguments":{"prompt":"x","model":model}}),
        )?;
        assert!(
            !result.to_string().contains(key),
            "validation error exposed credential"
        );
        assert_eq!(result["result"]["structuredContent"]["ok"], false);
    }
    assert!(api.posts.lock().unwrap().is_empty());
    Ok(())
}
