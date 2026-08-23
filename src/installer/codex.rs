use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use toml_edit::{Array, DocumentMut, Item, Table, Value, value};

use super::{
    ClientLaunchSpec, InstallError,
    atomic::{WriteReport, replace_verified},
    client_config::is_secret_environment_name,
};

pub fn write_config_file(
    path: &Path,
    launch: &ClientLaunchSpec,
) -> Result<WriteReport, InstallError> {
    let existing = if path.exists() {
        fs::read_to_string(path).map_err(|error| InstallError::ConfigIo {
            action: "读取 Codex TOML",
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
        action: "读取 Codex TOML",
        detail: error.to_string(),
    })?;
    let Some(rendered) = reset_config(&existing)? else {
        return Ok(None);
    };
    replace_verified(path, rendered.as_bytes(), |written| {
        if parse_config_launch(written)?.is_some() {
            return Err(InstallError::CodexRoundTrip(
                "reset 后仍存在 micu-image".into(),
            ));
        }
        Ok(())
    })
    .map(Some)
}

pub fn merge_config(existing: &str, launch: &ClientLaunchSpec) -> Result<String, InstallError> {
    let mut document = parse_document(existing)?;
    let server = micu_server_table(&mut document)?;
    server["command"] = value(launch.command_text()?.to_owned());

    let mut args = Array::new();
    for argument in launch.argument_texts()? {
        args.push(argument);
    }
    server["args"] = value(args);

    let environment = environment_table(server)?;
    environment.retain(|name, _| !is_secret_environment_name(name));
    for (name, value_text) in launch.env() {
        environment[name] = value(value_text.clone());
    }

    let rendered = document.to_string();
    verify_round_trip(&rendered, launch)?;
    Ok(rendered)
}

pub fn parse_config_launch(input: &str) -> Result<Option<ClientLaunchSpec>, InstallError> {
    let document = parse_document(input)?;
    let Some(server) = document
        .get("mcp_servers")
        .and_then(Item::as_table)
        .and_then(|servers| servers.get("micu-image"))
        .and_then(Item::as_table)
    else {
        return Ok(None);
    };
    let command = server
        .get("command")
        .and_then(Item::as_value)
        .and_then(Value::as_str)
        .ok_or_else(|| InstallError::InvalidCodex("micu-image.command 必须是 string".into()))?;
    let args = server
        .get("args")
        .and_then(Item::as_value)
        .and_then(Value::as_array)
        .ok_or_else(|| InstallError::InvalidCodex("micu-image.args 必须是 array".into()))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(OsString::from)
                .ok_or_else(|| InstallError::InvalidCodex("micu-image.args 只能包含 string".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let env = server
        .get("env")
        .and_then(Item::as_table)
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
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .and_then(|servers| servers.remove("micu-image"))
        .is_some();
    Ok(changed.then(|| document.to_string()))
}

fn parse_document(input: &str) -> Result<DocumentMut, InstallError> {
    input
        .parse::<DocumentMut>()
        .map_err(|error| InstallError::TomlParse(error.to_string()))
}

fn micu_server_table(document: &mut DocumentMut) -> Result<&mut Table, InstallError> {
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document
        .get_mut("mcp_servers")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| InstallError::InvalidCodex("mcp_servers 必须是 table".into()))?;
    if servers.get("micu-image").is_none() {
        servers.insert("micu-image", Item::Table(Table::new()));
    }
    servers
        .get_mut("micu-image")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| InstallError::InvalidCodex("mcp_servers.micu-image 必须是 table".into()))
}

fn environment_table(server: &mut Table) -> Result<&mut Table, InstallError> {
    if server.get("env").is_none() {
        server.insert("env", Item::Table(Table::new()));
    }
    server
        .get_mut("env")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| InstallError::InvalidCodex("mcp_servers.micu-image.env 必须是 table".into()))
}

fn parse_environment(table: &Table) -> Result<BTreeMap<String, String>, InstallError> {
    table
        .iter()
        .map(|(name, item)| {
            let value_text = item.as_value().and_then(Value::as_str).ok_or_else(|| {
                InstallError::InvalidCodex(format!(
                    "mcp_servers.micu-image.env.{name} 必须是 string"
                ))
            })?;
            Ok((name.to_owned(), value_text.to_owned()))
        })
        .collect()
}

pub(crate) fn verify_round_trip(
    rendered: &str,
    expected: &ClientLaunchSpec,
) -> Result<(), InstallError> {
    let actual = parse_config_launch(rendered)?
        .ok_or_else(|| InstallError::CodexRoundTrip("缺少 mcp_servers.micu-image".into()))?;
    if actual.command() != expected.command() {
        return Err(InstallError::CodexRoundTrip(
            "command 与原 PathBuf 不一致".into(),
        ));
    }
    if actual.args() != expected.args() {
        return Err(InstallError::CodexRoundTrip(
            "args 与原 OsString 不一致".into(),
        ));
    }
    for (name, expected_value) in expected.env() {
        if actual.env().get(name) != Some(expected_value) {
            return Err(InstallError::CodexRoundTrip(format!(
                "env.{name} 与原值不一致"
            )));
        }
    }
    if actual
        .env()
        .keys()
        .any(|name| is_secret_environment_name(name))
    {
        return Err(InstallError::CodexRoundTrip(
            "客户端配置不得持久化 API key".into(),
        ));
    }
    Ok(())
}
