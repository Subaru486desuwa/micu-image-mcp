use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{config::Config, storage::SavedImage};

#[derive(Clone)]
pub struct SecretArg(SecretString);

impl SecretArg {
    pub fn into_inner(self) -> SecretString {
        self.0
    }
}

impl fmt::Debug for SecretArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretArg([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for SecretArg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self(value.into()))
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
pub struct ToolFailure(pub String);

pub fn resolve_key(
    config: &Config,
    override_key: Option<SecretArg>,
) -> Result<SecretString, ToolFailure> {
    if let Some(key) = override_key {
        let key = key.into_inner();
        if !key.expose_secret().trim().is_empty() {
            return Ok(key.expose_secret().trim().to_owned().into());
        }
    }
    if config.api_key.expose_secret().trim().is_empty() {
        return Err(ToolFailure(
            "未配置 API key。请设置 MICU_API_KEY 环境变量，或在调用时传 api_key 参数。".into(),
        ));
    }
    Ok(config.api_key.clone())
}

pub fn validation_error(message: impl Into<String>) -> Value {
    let message = message.into();
    json!({"ok": false, "error": message, "errors": [message]})
}

pub fn saved_value(saved: &SavedImage, index: Option<usize>) -> Value {
    let mut value = serde_json::Map::new();
    if let Some(index) = index {
        value.insert("index".into(), Value::from(index));
    }
    value.insert(
        "path".into(),
        Value::String(saved.path.to_string_lossy().into_owned()),
    );
    value.insert("size_bytes".into(), Value::from(saved.size_bytes));
    value.insert(
        "actual_size".into(),
        Value::String(format!("{}x{}", saved.actual_size.0, saved.actual_size.1)),
    );
    value.insert(
        "actual_megapixels".into(),
        Value::from(saved.actual_megapixels),
    );
    Value::Object(value)
}

pub fn default_basename(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{nanos}")
}

pub fn push_note_once(notes: &mut Vec<String>, note: String) {
    if !notes.contains(&note) {
        notes.push(note);
    }
}

pub fn python_string_repr(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{escaped}'")
}
