use secrecy::ExposeSecret;
use serde_json::{Map, Value};

use crate::{
    domain::{
        basename::safe_basename,
        routing::{is_large_tier, is_quality_model, model_error, resolve_model, size_note},
        size::{size_tier, validate_size},
    },
    fs::input::validate_mask,
    http::client::RetryOptions,
    http::response::error_detail,
    providers::EditRequest,
};

use super::{
    EditParams, ToolEngine, ToolFailure,
    common::{
        default_basename, push_note_once, python_string_repr, resolve_key, saved_value,
        validation_error,
    },
};

impl ToolEngine {
    pub async fn image_edit(&self, params: EditParams) -> Result<Value, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(validation_error("prompt 不能为空"));
        }
        if let Some(error) = model_error(params.model.as_deref(), &self.config.default_model) {
            return Ok(validation_error(error));
        }
        let (cleaned_size, size_error) = validate_size(Some(&params.size), false);
        let Some(size) = cleaned_size else {
            return Ok(validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
            ));
        };
        let safe_stem = params
            .basename
            .as_deref()
            .and_then(|name| safe_basename(Some(name)));
        if params.basename.is_some() && safe_stem.is_none() {
            return Ok(validation_error(format!(
                "basename {} 含非法字符或路径分量",
                python_string_repr(params.basename.as_deref().unwrap_or_default())
            )));
        }
        let location = match self
            .output_store
            .resolve_save_dir(params.save_dir.as_deref())
        {
            Ok(location) => location,
            Err(error) => return Ok(validation_error(error)),
        };
        let image = match self
            .input_store
            .validate_image(&params.image_path, "image_path")
        {
            Ok(image) => image,
            Err(error) => return Ok(validation_error(error)),
        };
        let (effective_model, mut notes) =
            resolve_model(params.model.as_deref(), &self.config.default_model, &size);
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        let mask = if let Some(mask_path) = params.mask_path {
            let mask = match self.input_store.validate_image(&mask_path, "mask_path") {
                Ok(mask) => mask,
                Err(error) => return Ok(validation_error(error)),
            };
            if let Err(error) = validate_mask(&mask, &image) {
                return Ok(validation_error(error));
            }
            Some(mask)
        } else {
            None
        };
        let stem = safe_stem.unwrap_or_else(|| default_basename("edit"));
        let tier = size_tier(&size);
        let retry = RetryOptions {
            enabled: is_quality_model(&effective_model) || is_large_tier(tier),
            big_size: is_large_tier(tier),
        };
        let images = [("image", &image)];
        let mut last_error: Option<String> = None;
        let mut saved = None;
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
                .edit(
                    EditRequest {
                        model: &effective_model,
                        prompt: &params.prompt,
                        size: &size,
                        response_format,
                        images: &images,
                        mask: mask.as_ref(),
                    },
                    &key,
                    retry,
                    &mut notes,
                )
                .await;
            if !response.is_success() {
                last_error = Some(format!(
                    "HTTP {}: {}",
                    response.status,
                    error_detail(&response.body, &[key.expose_secret()])
                ));
                break;
            }
            match self
                .output
                .save_first(&response.body, &location, &stem)
                .await
            {
                Ok(image) => {
                    if format_index > 0 {
                        notes.push(format!("已通过 response_format={response_format} 成功落盘"));
                    }
                    saved = Some(image);
                    break;
                }
                Err(error) => last_error = Some(format!("保存失败: {error}")),
            }
        }
        let Some(saved) = saved else {
            let mut output = Map::new();
            output.insert("ok".into(), Value::Bool(false));
            output.insert("model".into(), Value::String(effective_model));
            output.insert("size".into(), Value::String(size));
            output.insert(
                "error".into(),
                Value::String(last_error.unwrap_or_else(|| "保存失败".into())),
            );
            output.insert(
                "notes".into(),
                Value::Array(notes.into_iter().map(Value::String).collect()),
            );
            return Ok(Value::Object(output));
        };
        if let Some(note) = size_note(&size, Some(saved.actual_size)) {
            push_note_once(&mut notes, note);
        }
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(true));
        output.insert("model".into(), Value::String(effective_model));
        output.insert("size".into(), Value::String(size));
        output.insert("used_fallback".into(), Value::Bool(false));
        output.insert("saved".into(), saved_value(&saved, None)?);
        output.insert(
            "notes".into(),
            Value::Array(notes.into_iter().map(Value::String).collect()),
        );
        Ok(Value::Object(output))
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
    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
    use secrecy::SecretString;
    use tokio::sync::Mutex;

    use crate::{
        config::{Config, test_paths},
        fs::output_store::OutputStore,
        fs::response_output::OutputSaver,
        http::client::{ApiResponse, HttpExecutor, RetryOptions},
        http::download::SystemResolver,
        providers::{EditRequest, GenerateRequest, ImageProvider},
    };

    use super::*;

    type CapturedEdit = (String, String, Vec<String>, bool);

    struct FakeProvider {
        body: Vec<u8>,
        edits: Mutex<Vec<CapturedEdit>>,
        calls: AtomicUsize,
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
                body: Vec::new(),
            }
        }

        async fn edit(
            &self,
            request: EditRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.edits.lock().await.push((
                request.model.into(),
                request.size.into(),
                request
                    .images
                    .iter()
                    .map(|(field, _)| (*field).into())
                    .collect(),
                request.mask.is_some(),
            ));
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
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        let out = temp.path().join("out");
        std::fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let source = input.join("source.png");
        let mask = input.join("mask.png");
        RgbImage::from_pixel(32, 24, Rgb([1, 2, 3]))
            .save_with_format(&source, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        RgbaImage::from_pixel(32, 24, Rgba([0, 0, 0, 0]))
            .save_with_format(&mask, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": STANDARD.encode(std::fs::read(&source).unwrap_or_else(|error| panic!("{error}")))}]
        })).unwrap_or_else(|error| panic!("{error}"));
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
        ]);
        let config =
            Arc::new(Config::from_map(&environment).unwrap_or_else(|error| panic!("{error}")));
        let paths = Arc::new(test_paths(temp.path(), environment));
        let provider = Arc::new(FakeProvider {
            body,
            edits: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        });
        let storage = OutputStore::new(paths.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let http = HttpExecutor::new(config.as_ref(), paths.as_ref())
            .unwrap_or_else(|error| panic!("{error}"));
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        let engine = ToolEngine::new(config, paths, storage, output, provider.clone());
        (temp, engine, provider, source, mask)
    }

    #[tokio::test]
    async fn edit_uses_edits_semantics_with_mask_and_saved_shape() {
        let (_temp, engine, provider, source, mask) = fixture();
        let result = engine
            .image_edit(EditParams {
                prompt: "换背景".into(),
                image_path: source.to_string_lossy().into_owned(),
                mask_path: Some(mask.to_string_lossy().into_owned()),
                size: "1024x1024".into(),
                model: None,
                save_dir: None,
                basename: Some("edited".into()),
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["saved"]["actual_size"], "32x24");
        assert_eq!(
            provider.edits.lock().await.as_slice(),
            &[(
                "gpt-image-2".into(),
                "1024x1024".into(),
                vec!["image".into()],
                true
            )]
        );
    }

    #[tokio::test]
    async fn edit_routes_high_resolution_and_rejects_missing_input_before_provider() {
        let (_temp, engine, provider, source, _mask) = fixture();
        let high = engine
            .image_edit(EditParams {
                prompt: "enhance".into(),
                image_path: source.to_string_lossy().into_owned(),
                mask_path: None,
                size: "3840x2160".into(),
                model: None,
                save_dir: None,
                basename: Some("high".into()),
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(high["model"], "gpt-image-2-openai", "{high}");
        let missing = engine
            .image_edit(EditParams {
                prompt: "x".into(),
                image_path: "/definitely/missing.png".into(),
                mask_path: None,
                size: "1024x1024".into(),
                model: None,
                save_dir: None,
                basename: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(missing["ok"], false);
        assert!(
            missing["error"]
                .as_str()
                .is_some_and(|text| text.contains("不存在"))
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
