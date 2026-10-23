use micu_devtools::{
    Result,
    mock::{MockMicu, png},
    stdio::Session,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

const FLARE: &str = "gpt-image-2.5-flare";
const LEGACY_FLARE: &str = "gpt-image-2.5-flare-openai";

#[test]
fn default_and_explicit_flare_ids_send_the_current_model() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    for model in [None, Some(FLARE), Some(LEGACY_FLARE)] {
        for size in ["1024x1024", "2048x1152", "3840x2160"] {
            let result = server.call(
                "image_generate",
                json!({"prompt":"current model", "model":model, "size":size, "quality":"max"}),
            )?;
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["model"], FLARE);
            let requests = api.requests();
            let request = requests.last().unwrap();
            assert_eq!(request.path, "/v1/images/generations");
            assert_eq!(request.fields["model"], FLARE);
            assert_eq!(request.fields["quality"], "max");
            assert_eq!(request.fields["size"], size);
        }
    }
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["default_model"], FLARE);
    assert_eq!(
        info["default_models"]["image_edit"],
        "gpt-image-2.5-sunburst"
    );
    assert!(
        info["available_models"]
            .as_array()
            .unwrap()
            .contains(&json!(FLARE))
    );
    assert_eq!(info["model_aliases"][LEGACY_FLARE], FLARE);
    Ok(())
}

#[test]
fn all_edit_tools_accept_both_flare_ids_without_changing_quality_policy() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let a = root.path().join("a.png");
    let b = root.path().join("b.png");
    fs::write(&a, png(32, 24, 1)?)?;
    fs::write(&b, png(32, 24, 2)?)?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &api.env(),
    )?;
    server.initialize()?;
    for model in [FLARE, LEGACY_FLARE] {
        for tool in ["image_edit", "image_batch_edit", "image_multi_reference"] {
            let before = api.requests().len();
            let result = server.call(
                tool,
                json!({"prompt":"edit", "image_path":a, "image_paths":[a,b],
                    "model":model, "size":"1024x1024", "quality":"xhigh"}),
            )?;
            assert_eq!(result["ok"], true, "{tool}: {result}");
            let requests = api.requests();
            let sent = &requests[before..];
            assert_eq!(sent.len(), if tool == "image_batch_edit" { 2 } else { 1 });
            for request in sent {
                assert_eq!(request.path, "/v1/images/edits");
                assert_eq!(request.fields["model"], FLARE);
                assert_eq!(request.fields["quality"], "xhigh");
                assert!(!request.files.is_empty());
            }
        }
    }
    Ok(())
}

#[test]
fn legacy_model_override_reports_and_sends_the_current_model() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let mut env = api.env();
    env.push(("MICU_MODEL", LEGACY_FLARE));
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &env,
    )?;
    server.initialize()?;
    let info = server.call("server_info", json!({}))?;
    assert_eq!(info["default_model"], FLARE);
    for tool in [
        "image_generate",
        "image_edit",
        "image_batch_edit",
        "image_multi_reference",
    ] {
        assert_eq!(info["default_models"][tool], FLARE);
    }
    let first = server.call("image_generate", json!({"prompt":"override"}))?;
    assert_eq!(first["ok"], true);
    let edited = server.call(
        "image_edit",
        json!({"prompt":"override edit", "image_path":first["saved"][0]["path"]}),
    )?;
    assert_eq!(edited["ok"], true);
    for request in api.requests() {
        assert_eq!(request.fields["model"], FLARE);
    }
    Ok(())
}

#[test]
fn persisted_legacy_sessions_resume_with_the_current_model() -> Result<()> {
    let root = tempfile::tempdir()?;
    let api = MockMicu::new()?;
    let binary = Path::new(env!("CARGO_BIN_EXE_micu-image-mcp"));
    let mut server = Session::new(binary, root.path(), &api.env())?;
    server.initialize()?;
    let first = server.call(
        "image_generate",
        json!({"prompt":"original subject", "session_id":"new", "quality":"low"}),
    )?;
    assert_eq!(first["ok"], true);
    let id = first["session"]["id"].as_str().unwrap();
    drop(server);
    // Reproduce a record saved before the upstream model rename.
    let directory = root.path().join(".micu-sessions").join(id);
    let record = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .ok_or("session revision missing")?;
    let mut revision: Value = serde_json::from_slice(&fs::read(&record)?)?;
    revision["model"] = json!(LEGACY_FLARE);
    fs::write(&record, serde_json::to_vec(&revision)?)?;
    for tool in ["image_edit", "image_generate"] {
        let mut restarted = Session::new(binary, root.path(), &api.env())?;
        restarted.initialize()?;
        let edited = restarted.call(
            tool,
            json!({"prompt":"change background", "session_id":id, "parent_revision":1}),
        )?;
        assert_eq!(edited["ok"], true, "{edited}");
        assert_eq!(edited["model"], FLARE);
        let requests = api.requests();
        let request = requests.last().unwrap();
        assert_eq!(request.path, "/v1/images/edits");
        assert_eq!(request.fields["model"], FLARE);
        assert_eq!(request.fields["quality"], "low");
        assert!(
            request.fields["prompt"]
                .as_str()
                .unwrap()
                .contains("original subject")
        );
    }
    assert_eq!(fs::read(&record)?, serde_json::to_vec(&revision)?);
    Ok(())
}
