use futures_util::{StreamExt, stream};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::{Map, Value};

use crate::{
    http_client::RetryOptions,
    providers::GenerateRequest,
    response::error_detail,
    storage::{SaveLocation, SavedImage},
    validation::{
        path::safe_basename,
        routing::{
            infer_size_from_prompt, is_large_tier, is_quality_model, model_error, resolve_model,
            size_note,
        },
        size::{SizeTier, size_tier, validate_n, validate_quality, validate_size},
    },
};

use super::{
    SecretArg, ToolEngine, ToolFailure,
    common::{
        default_basename, push_note_once, python_string_repr, resolve_key, saved_value,
        validation_error,
    },
};

fn default_n() -> i64 {
    1
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
#[serde(deny_unknown_fields)]
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

impl ToolEngine {
    pub async fn image_generate(&self, params: GenerateParams) -> Result<Value, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(validation_error("prompt 不能为空"));
        }
        if let Some(error) = validate_n(&Value::from(params.n)) {
            return Ok(validation_error(error));
        }
        if let Some(error) = model_error(params.model.as_deref(), &self.config.default_model) {
            return Ok(validation_error(error));
        }
        let safe_stem = params
            .basename
            .as_deref()
            .and_then(|name| safe_basename(Some(name)));
        if params.basename.is_some() && safe_stem.is_none() {
            let message = format!(
                "basename {} 含非法字符或路径分量；仅允许 [A-Za-z0-9_-.]，禁含 / 与 ..",
                python_string_repr(params.basename.as_deref().unwrap_or_default())
            );
            return Ok(validation_error(message));
        }
        let quality_value = params
            .quality
            .as_ref()
            .map(|value| Value::String(value.clone()));
        let (quality, quality_error) = validate_quality(quality_value.as_ref());
        if let Some(error) = quality_error {
            return Ok(validation_error(error));
        }
        let location = match self.storage.resolve_save_dir(params.save_dir.as_deref()) {
            Ok(location) => location,
            Err(error) => return Ok(validation_error(error)),
        };

        let (requested_size, inferred_note) = match params.size {
            Some(size) => (size, None),
            None => match infer_size_from_prompt(&params.prompt) {
                Some((size, reason)) => {
                    let note = format!("size=None → 推断 {size}（{reason}）");
                    (size, Some(note))
                }
                None => (
                    "1024x1024".into(),
                    Some("size=None → 无关键字命中，用默认 1024x1024".into()),
                ),
            },
        };
        let (cleaned_size, size_error) = validate_size(Some(&requested_size), false);
        let Some(size) = cleaned_size else {
            return Ok(validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
            ));
        };
        let (effective_model, mut notes) =
            resolve_model(params.model.as_deref(), &self.config.default_model, &size);
        if let Some(note) = inferred_note {
            notes.insert(0, note);
        }
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        let tier = size_tier(&size);
        let mut requested_n = params.n as usize;
        if is_large_tier(tier) && requested_n > 1 {
            notes.push(format!(
                "{} 强制 N=1，已忽略请求的 n={requested_n}",
                tier_label(tier).to_ascii_uppercase()
            ));
            requested_n = 1;
        }
        let quality_route = is_quality_model(&effective_model);
        let concurrency = if requested_n > 1
            && matches!(tier, SizeTier::Small | SizeTier::OneK)
            && !quality_route
        {
            5
        } else {
            1
        };
        let stem = safe_stem.unwrap_or_else(|| default_basename("gen"));
        let retry = RetryOptions {
            enabled: quality_route || is_large_tier(tier),
            big_size: is_large_tier(tier),
        };

        let mut results = if concurrency > 1 {
            stream::iter(0..requested_n)
                .map(|index| {
                    let engine = self.clone();
                    let prompt = params.prompt.clone();
                    let size = size.clone();
                    let model = effective_model.clone();
                    let quality = quality.clone();
                    let location = location.clone();
                    let stem = stem.clone();
                    let key = key.clone();
                    async move {
                        engine
                            .generate_one(
                                index,
                                &prompt,
                                &size,
                                &model,
                                quality.as_deref(),
                                &location,
                                &stem,
                                &key,
                                retry,
                            )
                            .await
                    }
                })
                .buffer_unordered(concurrency)
                .collect::<Vec<_>>()
                .await
        } else {
            let mut sequential = Vec::with_capacity(requested_n);
            for index in 0..requested_n {
                sequential.push(
                    self.generate_one(
                        index,
                        &params.prompt,
                        &size,
                        &effective_model,
                        quality.as_deref(),
                        &location,
                        &stem,
                        &key,
                        retry,
                    )
                    .await,
                );
            }
            sequential
        };
        results.sort_by_key(|result| result.index);
        if concurrency > 1 {
            notes.push(format!(
                "1K + 标准线路 + N={requested_n} 已 {concurrency} 并发"
            ));
        }

        let mut saved = Vec::new();
        let mut errors = Vec::new();
        for result in results {
            for note in result.notes {
                push_note_once(&mut notes, note);
            }
            if let Some(image) = result.saved {
                if let Some(note) = size_note(&size, Some(image.actual_size)) {
                    push_note_once(&mut notes, note);
                }
                saved.push(saved_value(&image, Some(result.index + 1)));
            }
            if let Some(error) = result.error {
                errors.push(Value::String(error));
            }
        }
        let requested_dimensions = crate::validation::size::parse_size(&size);
        let size_honored = !saved.is_empty()
            && saved.iter().all(|entry| {
                entry
                    .get("actual_size")
                    .and_then(Value::as_str)
                    .and_then(crate::validation::size::parse_size)
                    == requested_dimensions
            });
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(!saved.is_empty()));
        output.insert("model".into(), Value::String(effective_model));
        output.insert("size".into(), Value::String(size));
        output.insert("requested_n".into(), Value::from(requested_n));
        output.insert("used_fallback".into(), Value::Bool(false));
        output.insert("size_honored".into(), Value::Bool(size_honored));
        output.insert("saved".into(), Value::Array(saved));
        output.insert("errors".into(), Value::Array(errors));
        output.insert(
            "notes".into(),
            Value::Array(notes.into_iter().map(Value::String).collect()),
        );
        Ok(Value::Object(output))
    }

    #[allow(clippy::too_many_arguments)]
    async fn generate_one(
        &self,
        index: usize,
        prompt: &str,
        size: &str,
        model: &str,
        quality: Option<&str>,
        location: &SaveLocation,
        stem: &str,
        key: &SecretString,
        retry: RetryOptions,
    ) -> GenerateOneResult {
        let mut notes = Vec::new();
        let mut last_error: Option<String> = None;
        for (format_index, response_format) in
            self.config.response_formats_to_try.iter().enumerate()
        {
            if format_index > 0 {
                notes.push(format!(
                    "URL 落盘失败（{}）→ 重试 API response_format={response_format}",
                    last_error.as_deref().unwrap_or("保存失败")
                ));
            }
            let response = self
                .provider
                .generate(
                    GenerateRequest {
                        model,
                        prompt,
                        size,
                        quality,
                        response_format,
                    },
                    key,
                    retry,
                    &mut notes,
                )
                .await;
            if !response.is_success() {
                last_error = Some(format!(
                    "#{} HTTP {}: {}",
                    index + 1,
                    response.status,
                    error_detail(&response.body, &[key.expose_secret()])
                ));
                break;
            }
            match self
                .output
                .save_first(&response.body, location, &format!("{stem}_{}", index + 1))
                .await
            {
                Ok(saved) => {
                    if format_index > 0 {
                        notes.push(format!("已通过 response_format={response_format} 成功落盘"));
                    }
                    return GenerateOneResult {
                        index,
                        saved: Some(saved),
                        error: None,
                        notes,
                    };
                }
                Err(error) => {
                    last_error = Some(format!("#{} 保存失败: {error}", index + 1));
                }
            }
        }
        GenerateOneResult {
            index,
            saved: None,
            error: Some(last_error.unwrap_or_else(|| format!("#{} 保存失败", index + 1))),
            notes,
        }
    }
}

struct GenerateOneResult {
    index: usize,
    saved: Option<SavedImage>,
    error: Option<String>,
    notes: Vec<String>,
}

fn tier_label(tier: SizeTier) -> &'static str {
    match tier {
        SizeTier::Unknown => "unknown",
        SizeTier::Small => "small",
        SizeTier::OneK => "1k",
        SizeTier::TwoK => "2k",
        SizeTier::FourK => "4k",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::{ImageFormat, Rgb, RgbImage};
    use secrecy::SecretString;
    use tokio::sync::Mutex;

    use crate::{
        config::Config,
        download::SystemResolver,
        http_client::{ApiResponse, HttpExecutor, RetryOptions},
        output::OutputSaver,
        providers::{EditRequest, GenerateRequest, ImageProvider},
        storage::Storage,
    };

    use super::*;

    struct FakeProvider {
        body: Vec<u8>,
        calls: Mutex<Vec<(String, String, String)>>,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    #[async_trait]
    impl ImageProvider for FakeProvider {
        async fn generate(
            &self,
            request: GenerateRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            self.calls.lock().await.push((
                request.model.into(),
                request.size.into(),
                request.response_format.into(),
            ));
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ApiResponse {
                status: 200,
                headers: reqwest::header::HeaderMap::new(),
                body: self.body.clone(),
            }
        }

        async fn edit(
            &self,
            _request: EditRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            ApiResponse {
                status: 500,
                headers: reqwest::header::HeaderMap::new(),
                body: Vec::new(),
            }
        }
    }

    fn fixture() -> (tempfile::TempDir, ToolEngine, Arc<FakeProvider>) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let out = temp.path().join("out");
        let config = Arc::new(
            Config::from_map(&BTreeMap::from([
                (
                    "HOME".into(),
                    temp.path().join("home").to_string_lossy().into_owned(),
                ),
                (
                    "MICU_SAVE_DIR_ROOT".into(),
                    out.to_string_lossy().into_owned(),
                ),
                ("MICU_SAVE_DIR".into(), out.to_string_lossy().into_owned()),
                ("MICU_API_KEY".into(), "sk-test".into()),
            ]))
            .unwrap_or_else(|error| panic!("{error}")),
        );
        let image_path = temp.path().join("fixture.png");
        RgbImage::from_pixel(32, 24, Rgb([1, 2, 3]))
            .save_with_format(&image_path, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let image = std::fs::read(image_path).unwrap_or_else(|error| panic!("{error}"));
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": STANDARD.encode(image)}]
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        let provider = Arc::new(FakeProvider {
            body,
            calls: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let storage = Storage::new(config.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let http = HttpExecutor::new(config.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        let engine = ToolEngine::new(config, storage, output, provider.clone());
        (temp, engine, provider)
    }

    fn params() -> GenerateParams {
        GenerateParams {
            prompt: "一只猫".into(),
            size: Some("1024x1024".into()),
            n: 1,
            model: None,
            quality: None,
            save_dir: None,
            basename: Some("test".into()),
            api_key: None,
        }
    }

    #[tokio::test]
    async fn generate_validates_then_saves_the_public_shape() {
        let (_temp, engine, provider) = fixture();
        let result = engine
            .image_generate(params())
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["model"], "gpt-image-2");
        assert_eq!(result["requested_n"], 1);
        assert_eq!(result["saved"][0]["actual_size"], "32x24");
        assert_eq!(
            provider.calls.lock().await.as_slice(),
            &[("gpt-image-2".into(), "1024x1024".into(), "url".into())]
        );
    }

    #[tokio::test]
    async fn generate_forces_high_resolution_to_one_and_routes_quality() {
        let (_temp, engine, provider) = fixture();
        let mut request = params();
        request.size = Some("2048x2048".into());
        request.n = 4;
        let result = engine
            .image_generate(request)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["model"], "gpt-image-2-openai", "{result}");
        assert_eq!(result["requested_n"], 1);
        assert!(result["notes"].as_array().is_some_and(|notes| {
            notes
                .iter()
                .any(|note| note.as_str().is_some_and(|text| text.contains("强制 N=1")))
        }));
        assert_eq!(provider.calls.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn generate_standard_n_six_has_max_five_in_flight_and_stable_order() {
        let (_temp, engine, provider) = fixture();
        let mut request = params();
        request.n = 6;
        let result = engine
            .image_generate(request)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            result["saved"].as_array().map(Vec::len),
            Some(6),
            "{result}"
        );
        assert_eq!(provider.max_active.load(Ordering::SeqCst), 5);
        let indices = result["saved"]
            .as_array()
            .unwrap_or_else(|| panic!("saved array"))
            .iter()
            .map(|entry| entry["index"].as_u64())
            .collect::<Vec<_>>();
        assert_eq!(
            indices,
            [Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)]
        );
    }
}
