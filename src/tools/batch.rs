use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::validation::{
    routing::{is_quality_model, model_error, resolve_model},
    size::validate_size,
};

use super::{
    EditParams, SecretArg, ToolEngine, ToolFailure,
    common::{default_basename, validation_error},
};

fn default_size() -> String {
    "1024x1024".into()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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

impl ToolEngine {
    pub async fn image_batch_edit(&self, params: BatchParams) -> Result<Value, ToolFailure> {
        let total = params.image_paths.len();
        if params.prompt.trim().is_empty() {
            return Ok(batch_validation_error("prompt 不能为空", 0));
        }
        if total == 0 {
            return Ok(batch_validation_error("image_paths 必须是非空 list", 0));
        }
        if total > 20 {
            return Ok(batch_validation_error(
                format!("image_paths 最多 20 张，收到 {total} 张（防止意外 burn quota）"),
                total,
            ));
        }
        if let Some(error) = model_error(params.model.as_deref(), &self.config.default_model) {
            return Ok(batch_validation_error(error, total));
        }
        let (cleaned_size, size_error) = validate_size(Some(&params.size), false);
        let Some(size) = cleaned_size else {
            return Ok(batch_validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
                total,
            ));
        };
        let location = match self.storage.resolve_save_dir(params.save_dir.as_deref()) {
            Ok(location) => location,
            Err(error) => return Ok(batch_validation_error(error, total)),
        };
        let (effective_model, notes) =
            resolve_model(params.model.as_deref(), &self.config.default_model, &size);
        let concurrency = if is_quality_model(&effective_model) {
            1
        } else {
            5
        };
        let output_dir = location.absolute.to_string_lossy().into_owned();
        let prompt = params.prompt;
        let image_paths = params.image_paths;
        let key = params.api_key;

        let mut results = if concurrency == 1 {
            let mut sequential = Vec::with_capacity(total);
            for (index, input) in image_paths.iter().enumerate() {
                if index > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(1.5)).await;
                }
                sequential.push(
                    self.batch_one(
                        index,
                        input.clone(),
                        prompt.clone(),
                        size.clone(),
                        effective_model.clone(),
                        output_dir.clone(),
                        key.clone(),
                    )
                    .await,
                );
            }
            sequential
        } else {
            stream::iter(image_paths.into_iter().enumerate())
                .map(|(index, input)| {
                    let engine = self.clone();
                    let prompt = prompt.clone();
                    let size = size.clone();
                    let model = effective_model.clone();
                    let output_dir = output_dir.clone();
                    let key = key.clone();
                    async move {
                        engine
                            .batch_one(index, input, prompt, size, model, output_dir, key)
                            .await
                    }
                })
                .buffer_unordered(concurrency)
                .collect::<Vec<_>>()
                .await
        };
        results.sort_by_key(|(index, _)| *index);
        let succeeded = results
            .iter()
            .filter(|(_, result)| result.get("ok").and_then(Value::as_bool) == Some(true))
            .count();
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(succeeded > 0));
        output.insert("model".into(), Value::String(effective_model));
        output.insert("size".into(), Value::String(size));
        output.insert("concurrency".into(), Value::from(concurrency));
        output.insert("total".into(), Value::from(total));
        output.insert("succeeded".into(), Value::from(succeeded));
        output.insert("failed".into(), Value::from(total - succeeded));
        output.insert(
            "results".into(),
            Value::Array(results.into_iter().map(|(_, result)| result).collect()),
        );
        output.insert(
            "notes".into(),
            Value::Array(notes.into_iter().map(Value::String).collect()),
        );
        Ok(Value::Object(output))
    }

    #[allow(clippy::too_many_arguments)]
    async fn batch_one(
        &self,
        index: usize,
        input: String,
        prompt: String,
        size: String,
        model: String,
        output_dir: String,
        api_key: Option<SecretArg>,
    ) -> (usize, Value) {
        let result = self
            .image_edit(EditParams {
                prompt,
                image_path: input.clone(),
                mask_path: None,
                size,
                model: Some(model),
                save_dir: Some(output_dir),
                basename: Some(format!("{}_{}", default_basename("batch"), index + 1)),
                api_key,
            })
            .await;
        let mut value = match result {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(other) => other,
            Err(error) => {
                let mut object = Map::new();
                object.insert("ok".into(), Value::Bool(false));
                object.insert("index".into(), Value::from(index + 1));
                object.insert("input".into(), Value::String(input));
                object.insert("error".into(), Value::String(error.to_string()));
                return (index, Value::Object(object));
            }
        };
        if let Value::Object(object) = &mut value {
            object.insert("index".into(), Value::from(index + 1));
            object.insert("input".into(), Value::String(input));
        }
        (index, value)
    }
}

fn batch_validation_error(message: impl Into<String>, total: usize) -> Value {
    let mut value = validation_error(message);
    if let Value::Object(object) = &mut value {
        object.insert("total".into(), Value::from(total));
    }
    value
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Instant,
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
        active: AtomicUsize,
        max_active: AtomicUsize,
        starts: Mutex<Vec<Instant>>,
    }

    #[async_trait]
    impl ImageProvider for FakeProvider {
        async fn generate(
            &self,
            _request: GenerateRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            ApiResponse {
                status: 500,
                headers: reqwest::header::HeaderMap::new(),
                body: vec![],
            }
        }

        async fn edit(
            &self,
            _request: EditRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            self.starts.lock().await.push(Instant::now());
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            ApiResponse {
                status: 200,
                headers: reqwest::header::HeaderMap::new(),
                body: self.body.clone(),
            }
        }
    }

    fn fixture() -> (
        tempfile::TempDir,
        ToolEngine,
        Arc<FakeProvider>,
        Vec<String>,
    ) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        let out = temp.path().join("out");
        std::fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let mut paths = Vec::new();
        for index in 0..6 {
            let path = input.join(format!("{index}.png"));
            RgbImage::from_pixel(32 + index, 24 + index, Rgb([1, 2, 3]))
                .save_with_format(&path, ImageFormat::Png)
                .unwrap_or_else(|error| panic!("{error}"));
            paths.push(path.to_string_lossy().into_owned());
        }
        let output_image = std::fs::read(&paths[0]).unwrap_or_else(|error| panic!("{error}"));
        let body = serde_json::to_vec(
            &serde_json::json!({"data":[{"b64_json":STANDARD.encode(output_image)}]}),
        )
        .unwrap_or_else(|error| panic!("{error}"));
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
                (
                    "MICU_INPUT_ROOT".into(),
                    input.to_string_lossy().into_owned(),
                ),
                ("MICU_API_KEY".into(), "sk-test".into()),
            ]))
            .unwrap_or_else(|error| panic!("{error}")),
        );
        let provider = Arc::new(FakeProvider {
            body,
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            starts: Mutex::new(Vec::new()),
        });
        let storage = Storage::new(config.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let http = HttpExecutor::new(config.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        (
            temp,
            ToolEngine::new(config, storage, output, provider.clone()),
            provider,
            paths,
        )
    }

    #[tokio::test]
    async fn standard_batch_caps_concurrency_at_five_and_sorts_results() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_batch_edit(BatchParams {
                prompt: "sketch".into(),
                image_paths: paths,
                size: "1024x1024".into(),
                model: None,
                save_dir: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["succeeded"], 6, "{result}");
        assert_eq!(result["concurrency"], 5);
        assert_eq!(provider.max_active.load(Ordering::SeqCst), 5);
        assert_eq!(result["results"][0]["index"], 1);
        assert_eq!(result["results"][5]["index"], 6);
    }

    #[tokio::test]
    async fn quality_batch_is_serial_with_inter_request_gap() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_batch_edit(BatchParams {
                prompt: "enhance".into(),
                image_paths: paths[..2].to_vec(),
                size: "1024x1024".into(),
                model: Some("gpt-image-2-openai".into()),
                save_dir: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["concurrency"], 1, "{result}");
        let starts = provider.starts.lock().await;
        assert_eq!(starts.len(), 2);
        assert!(starts[1].duration_since(starts[0]) >= std::time::Duration::from_secs_f64(1.45));
    }
}
