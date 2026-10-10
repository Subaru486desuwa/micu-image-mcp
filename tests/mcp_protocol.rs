use micu_devtools::{
    Result,
    stdio::{Session, latest_meta},
};
use serde_json::json;
use std::path::Path;

#[test]
fn legacy_initialize_exposes_the_five_current_tools_through_stdio() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[],
    )?;
    let init = server.initialize()?;
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    let response = server.request("tools/list", json!({}))?;
    let names: Vec<_> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "image_generate",
            "image_edit",
            "image_batch_edit",
            "image_multi_reference",
            "server_info"
        ]
    );
    Ok(())
}

#[test]
fn modern_stateless_catalog_has_private_cache_fields_while_legacy_does_not() -> Result<()> {
    let root = tempfile::tempdir()?;
    let binary = Path::new(env!("CARGO_BIN_EXE_micu-image-mcp"));
    let mut server = Session::new(binary, root.path(), &[])?;
    let discovered = server.request("server/discover", json!({"_meta": latest_meta()}))?;
    assert!(
        discovered["result"]["supportedVersions"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28"))
    );
    for method in [
        "tools/list",
        "prompts/list",
        "resources/list",
        "resources/templates/list",
    ] {
        let response = server.request(method, json!({"_meta": latest_meta()}))?;
        assert_eq!(response["result"]["resultType"], "complete");
        assert_eq!(response["result"]["ttlMs"], 0);
        assert_eq!(response["result"]["cacheScope"], "private");
    }
    let info = server.request(
        "tools/call",
        json!({"_meta": latest_meta(), "name":"server_info", "arguments":{}}),
    )?;
    assert_eq!(info["result"]["resultType"], "complete");
    drop(server);
    let mut legacy = Session::new(binary, root.path(), &[])?;
    legacy.initialize()?;
    for method in [
        "tools/list",
        "prompts/list",
        "resources/list",
        "resources/templates/list",
    ] {
        let response = legacy.request(method, json!({}))?;
        assert!(response["result"].get("ttlMs").is_none());
        assert!(response["result"].get("cacheScope").is_none());
    }
    Ok(())
}

#[test]
fn tool_schemas_are_owned_by_rust_and_unknown_arguments_do_not_change_validation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut server = Session::new(
        Path::new(env!("CARGO_BIN_EXE_micu-image-mcp")),
        root.path(),
        &[],
    )?;
    server.initialize()?;
    let actual = server.request("tools/list", json!({}))?;
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../src/contracts/tool_contract.json"))?;
    let expected = expected["tools"]
        .as_array()
        .or_else(|| expected["result"]["tools"].as_array())
        .unwrap();
    for tool in actual["result"]["tools"].as_array().unwrap() {
        let source = expected
            .iter()
            .find(|item| item["name"] == tool["name"])
            .unwrap();
        assert_eq!(tool["inputSchema"], source["inputSchema"]);
        let tool = tool["name"].as_str().unwrap();
        let base = server.request("tools/call", json!({"name":tool,"arguments":{}}))?;
        let extra = server.request(
            "tools/call",
            json!({"name":tool,"arguments":{"unknown_parameter":{"nested":true}}}),
        )?;
        assert_eq!(base["result"], extra["result"]);
    }
    Ok(())
}
