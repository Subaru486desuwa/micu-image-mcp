use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::Value;

use super::SecretArg;

fn default_n() -> i64 {
    1
}

fn default_size() -> String {
    "1024x1024".into()
}

fn deserialize_n<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Bool(value) => Ok(i64::from(value)),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Ok(value)
            } else if let Some(value) = number.as_u64().and_then(|value| i64::try_from(value).ok())
            {
                Ok(value)
            } else if let Some(value) = number.as_f64().filter(|value| value.fract() == 0.0) {
                Ok(value as i64)
            } else {
                Err(D::Error::custom("n must be an integer"))
            }
        }
        Value::String(value) => value
            .trim()
            .parse()
            .map_err(|_| D::Error::custom("n must be an integer")),
        _ => Err(D::Error::custom("n must be an integer")),
    }
}

#[derive(Deserialize)]
pub struct GenerateParams {
    pub prompt: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default = "default_n", deserialize_with = "deserialize_n")]
    pub n: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub basename: Option<String>,
    #[serde(default)]
    pub api_key: Option<SecretArg>,
}

#[derive(Deserialize)]
pub struct EditParams {
    pub prompt: String,
    pub image_path: String,
    #[serde(default)]
    pub mask_path: Option<String>,
    #[serde(default = "default_size")]
    pub size: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub basename: Option<String>,
    #[serde(default)]
    pub api_key: Option<SecretArg>,
}

#[derive(Deserialize)]
pub struct BatchParams {
    pub prompt: String,
    pub image_paths: Vec<String>,
    #[serde(default = "default_size")]
    pub size: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub api_key: Option<SecretArg>,
}

#[derive(Deserialize)]
pub struct MultiReferenceParams {
    pub prompt: String,
    pub image_paths: Vec<String>,
    #[serde(default = "default_size")]
    pub size: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub save_dir: Option<String>,
    #[serde(default)]
    pub basename: Option<String>,
    #[serde(default)]
    pub api_key: Option<SecretArg>,
}
