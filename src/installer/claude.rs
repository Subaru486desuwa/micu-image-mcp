use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};

use super::{
    ClientLaunchSpec, InstallError,
    atomic::{WriteReport, replace_verified},
    client_config::{keep_existing_environment_name, verify_launch_round_trip},
};

pub fn write_config_file(
    path: &Path,
    launch: &ClientLaunchSpec,
) -> Result<WriteReport, InstallError> {
    let existing = if path.exists() {
        fs::read_to_string(path).map_err(|error| InstallError::ConfigIo {
            action: "读取 Claude JSON",
            detail: error.to_string(),
        })?
    } else {
        String::new()
    };
    let rendered = merge_config(&existing, launch)?;
    replace_verified(path, rendered.as_bytes(), |written| {
        verify_round_trip(written, launch)
    })
}

pub fn reset_config_file(path: &Path) -> Result<Option<WriteReport>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let existing = fs::read_to_string(path).map_err(|error| InstallError::ConfigIo {
        action: "读取 Claude JSON",
        detail: error.to_string(),
    })?;
    let Some(rendered) = reset_config(&existing)? else {
        return Ok(None);
    };
    replace_verified(path, rendered.as_bytes(), |written| {
        if parse_config_launch(written)?.is_some() {
            return Err(InstallError::ClaudeRoundTrip(
                "reset 后仍存在 micu-image".into(),
            ));
        }
        Ok(())
    })
    .map(Some)
}

pub fn merge_config(existing: &str, launch: &ClientLaunchSpec) -> Result<String, InstallError> {
    let mut document = parse_document(existing)?;
    let server = micu_server(&mut document)?;
    server.insert(
        "command".into(),
        Value::String(launch.command_text()?.to_owned()),
    );
    server.insert(
        "args".into(),
        Value::Array(
            launch
                .argument_texts()?
                .into_iter()
                .map(|argument| Value::String(argument.to_owned()))
                .collect(),
        ),
    );

    let environment = environment_object(server)?;
    environment.retain(|name, _| keep_existing_environment_name(name, launch));
    for (name, value_text) in launch.env() {
        environment.insert(name.clone(), Value::String(value_text.clone()));
    }

    let rendered = serde_json::to_string_pretty(&document)
        .map_err(|error| InstallError::InvalidClaude(error.to_string()))?;
    verify_round_trip(&rendered, launch)?;
    Ok(rendered)
}

pub fn parse_config_launch(input: &str) -> Result<Option<ClientLaunchSpec>, InstallError> {
    let document = parse_document(input)?;
    let Some(server) = document
        .as_object()
        .and_then(|root| root.get("mcpServers"))
        .and_then(Value::as_object)
        .and_then(|servers| servers.get("micu-image"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let command = server
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| InstallError::InvalidClaude("micu-image.command 必须是 string".into()))?;
    let args = server
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| InstallError::InvalidClaude("micu-image.args 必须是 array".into()))?
        .iter()
        .map(|value| {
            value.as_str().map(OsString::from).ok_or_else(|| {
                InstallError::InvalidClaude("micu-image.args 只能包含 string".into())
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let env = server
        .get("env")
        .and_then(Value::as_object)
        .map(parse_environment)
        .transpose()?
        .unwrap_or_default();
    Ok(Some(ClientLaunchSpec::from_parsed(
        PathBuf::from(command),
        args,
        env,
    )))
}

pub fn reset_config(existing: &str) -> Result<Option<String>, InstallError> {
    let mut document = parse_document(existing)?;
    let changed = document
        .as_object_mut()
        .and_then(|root| root.get_mut("mcpServers"))
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.remove("micu-image"))
        .is_some();
    if !changed {
        return Ok(None);
    }
    serde_json::to_string_pretty(&document)
        .map(Some)
        .map_err(|error| InstallError::InvalidClaude(error.to_string()))
}

fn parse_document(input: &str) -> Result<Value, InstallError> {
    let source = if input.trim().is_empty() { "{}" } else { input };
    let document: Value =
        serde_json::from_str(source).map_err(|error| InstallError::JsonParse(error.to_string()))?;
    if !document.is_object() {
        return Err(InstallError::InvalidClaude(
            "~/.claude.json 顶层必须是 object".into(),
        ));
    }
    Ok(document)
}

fn micu_server(document: &mut Value) -> Result<&mut Map<String, Value>, InstallError> {
    let root = document
        .as_object_mut()
        .ok_or_else(|| InstallError::InvalidClaude("顶层必须是 object".into()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| InstallError::InvalidClaude("mcpServers 必须是 object".into()))?;
    servers
        .entry("micu-image")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| InstallError::InvalidClaude("mcpServers.micu-image 必须是 object".into()))
}

fn environment_object(
    server: &mut Map<String, Value>,
) -> Result<&mut Map<String, Value>, InstallError> {
    server
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            InstallError::InvalidClaude("mcpServers.micu-image.env 必须是 object".into())
        })
}

fn parse_environment(
    environment: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, InstallError> {
    environment
        .iter()
        .map(|(name, value)| {
            let value_text = value.as_str().ok_or_else(|| {
                InstallError::InvalidClaude(format!(
                    "mcpServers.micu-image.env.{name} 必须是 string"
                ))
            })?;
            Ok((name.clone(), value_text.to_owned()))
        })
        .collect()
}

pub(crate) fn verify_round_trip(
    rendered: &str,
    expected: &ClientLaunchSpec,
) -> Result<(), InstallError> {
    let actual = parse_config_launch(rendered)?
        .ok_or_else(|| InstallError::ClaudeRoundTrip("缺少 mcpServers.micu-image".into()))?;
    verify_launch_round_trip(&actual, expected).map_err(InstallError::ClaudeRoundTrip)
}
