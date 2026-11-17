use futures_util::{StreamExt, stream};
use serde_json::{Map, Value};

use crate::domain::{routing::ModelRoute, size::validate_size};

use super::{
    BatchParams, EditParams, SecretArg, ToolEngine, ToolFailure,
    common::{default_basename, validation_error},
    edit::EditOutcome,
};

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
        if let Some(error) = self.edit_model_error(params.model.as_deref(), params.api_key.as_ref())
        {
            return Ok(batch_validation_error(error, total));
        }
        let (cleaned_size, size_error) = validate_size(Some(&params.size), false);
        let Some(size) = cleaned_size else {
            return Ok(batch_validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
                total,
            ));
        };
        let (route, mut notes) = ModelRoute::resolve(
            params.model.as_deref(),
            &self.config.default_edit_model,
            &size,
        );
        let quality = match route.validate_quality(params.quality.as_deref()) {
            Ok(quality) => quality,
            Err(error) => return Ok(batch_validation_error(error, total)),
        };
        let location = match self
            .output_store
            .resolve_save_dir(params.save_dir.as_deref())
        {
            Ok(location) => location,
            Err(error) => return Ok(batch_validation_error(error, total)),
        };
        // Serial routes keep a 1.5s gap between images, as server_info documents.
        let concurrency = route.parallel_limit();
        let effective_model = route.model().to_owned();
        let output_dir = location
            .absolute
            .to_str()
            .ok_or_else(|| ToolFailure("输出目录不是合法 Unicode，无法无损写入 MCP JSON".into()))?
            .to_owned();
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
                        quality.clone(),
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
                    let quality = quality.clone();
                    let output_dir = output_dir.clone();
                    let key = key.clone();
                    async move {
                        engine
                            .batch_one(index, input, prompt, size, model, quality, output_dir, key)
                            .await
                    }
                })
                .buffer_unordered(concurrency)
                .collect::<Vec<_>>()
                .await
        };
        results.sort_by_key(|item| item.index);
        let succeeded = results.iter().filter(|item| item.succeeded).count();
        let actual_model = results
            .iter()
            .find(|item| item.succeeded)
            .or_else(|| results.first())
            .and_then(|item| item.model.as_deref())
            .unwrap_or(&effective_model)
            .to_owned();
        let used_fallback = results.iter().any(|item| item.used_fallback);
        if results.iter().any(|item| {
            item.model
                .as_deref()
                .is_some_and(|model| model != actual_model)
        }) {
            notes.push("批量结果使用了不同兼容 ID；各图片的实际模型见 results[].model".into());
        }
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(succeeded > 0));
        output.insert("model".into(), Value::String(actual_model));
        output.insert("used_fallback".into(), Value::Bool(used_fallback));
        output.insert("size".into(), Value::String(size));
        output.insert("concurrency".into(), Value::from(concurrency));
        output.insert("total".into(), Value::from(total));
        output.insert("succeeded".into(), Value::from(succeeded));
        output.insert("failed".into(), Value::from(total - succeeded));
        output.insert(
            "results".into(),
            Value::Array(results.into_iter().map(|item| item.value).collect()),
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
        quality: Option<String>,
        output_dir: String,
        api_key: Option<SecretArg>,
    ) -> BatchItem {
        let outcome = self
            .edit_one(EditParams {
                prompt,
                image_path: Some(input.clone()),
                mask_path: None,
                size: Some(size),
                model: Some(model),
                quality,
                output_format: None,
                save_dir: Some(output_dir),
                basename: Some(format!("{}_{}", default_basename("batch"), index + 1)),
                api_key,
                session_id: None,
                parent_revision: None,
                context: Default::default(),
            })
            .await;
        let rendered = outcome.and_then(|outcome| {
            let (succeeded, model, used_fallback) = match &outcome {
                EditOutcome::Saved {
                    model,
                    used_fallback,
                    ..
                } => (true, Some(model.clone()), *used_fallback),
                EditOutcome::Failed {
                    model,
                    used_fallback,
                    ..
                } => (false, Some(model.clone()), *used_fallback),
                EditOutcome::Invalid(_) | EditOutcome::SessionCommitFailed { .. } => {
                    (false, None, false)
                }
            };
            let value = outcome.into_edit_value()?;
            Ok((succeeded, model, used_fallback, value))
        });
        match rendered {
            Ok((succeeded, model, used_fallback, mut value)) => {
                // Each entry is the single-image edit shape followed by `index` and `input`.
                if let Value::Object(object) = &mut value {
                    object.insert("index".into(), Value::from(index + 1));
                    object.insert("input".into(), Value::String(input));
                }
                BatchItem {
                    index,
                    succeeded,
                    model,
                    used_fallback,
                    value,
                }
            }
            Err(error) => {
                let mut object = Map::new();
                object.insert("ok".into(), Value::Bool(false));
                object.insert("index".into(), Value::from(index + 1));
                object.insert("input".into(), Value::String(input));
                object.insert("error".into(), Value::String(error.to_string()));
                BatchItem {
                    index,
                    succeeded: false,
                    model: None,
                    used_fallback: false,
                    value: Value::Object(object),
                }
            }
        }
    }
}

/// One image of a batch: its rendered `results[]` entry and the facts the summary needs.
struct BatchItem {
    index: usize,
    succeeded: bool,
    /// The actual model, when the edit got as far as its image job.
    model: Option<String>,
    used_fallback: bool,
    value: Value,
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
        sync::Arc,
        time::{Duration, Instant},
    };

    use image::{ImageFormat, Rgb, RgbImage};

    use crate::tools::test_support::{Endpoint, Reply, ScriptedProvider, tool_engine};

    use super::*;

    /// Start time of each edit request, in arrival order.
    fn edit_starts(provider: &ScriptedProvider) -> Vec<Instant> {
        provider
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == Endpoint::Edit)
            .map(|call| call.started_at)
            .collect()
    }

    fn fixture() -> (
        tempfile::TempDir,
        ToolEngine,
        Arc<ScriptedProvider>,
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
        let environment = BTreeMap::from([
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
            ("MICU_BASEURL".into(), "http://127.0.0.1:9".into()),
        ]);
        // Edits are held open so overlap is observable; generation is refused.
        let provider = Arc::new(
            ScriptedProvider::answering(Reply::image(&output_image))
                .with_delay(Duration::from_millis(40))
                .with_endpoint_reply(Endpoint::Generate, Reply::raw(500, Vec::new())),
        );
        let engine = tool_engine(temp.path(), environment, provider.clone());
        (temp, engine, provider, paths)
    }

    #[tokio::test]
    async fn standard_batch_caps_concurrency_at_five_and_sorts_results() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_batch_edit(BatchParams {
                prompt: "sketch".into(),
                image_paths: paths,
                size: "1024x1024".into(),
                model: Some("gpt-image-2".into()),
                quality: None,
                save_dir: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["succeeded"], 6, "{result}");
        assert_eq!(result["concurrency"], 5);
        assert_eq!(provider.peak_in_flight(), 5);
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
                quality: None,
                save_dir: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["concurrency"], 1, "{result}");
        let starts = edit_starts(&provider);
        assert_eq!(starts.len(), 2);
        assert!(starts[1].duration_since(starts[0]) >= std::time::Duration::from_secs_f64(1.45));
    }

    #[tokio::test]
    async fn large_standard_batch_is_serial_with_inter_request_gap() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_batch_edit(BatchParams {
                prompt: "upscale".into(),
                image_paths: paths[..2].to_vec(),
                size: "2048x1152".into(),
                model: Some("gpt-image-2.5-flare".into()),
                quality: None,
                save_dir: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["succeeded"], 2, "{result}");
        assert_eq!(result["model"], "gpt-image-2.5-flare");
        assert_eq!(result["concurrency"], 1);
        assert_eq!(provider.peak_in_flight(), 1);
        let starts = edit_starts(&provider);
        assert!(starts[1].duration_since(starts[0]) >= std::time::Duration::from_secs_f64(1.45));
    }
}
