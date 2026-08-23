use secrecy::ExposeSecret;
use serde_json::{Map, Value};

use crate::{
    domain::{
        basename::safe_basename,
        routing::{is_large_tier, is_quality_model, model_error, resolve_model, size_note},
        size::{parse_size, size_tier, validate_size},
    },
    fs::input::MAX_TOTAL_INPUT_BYTES,
    http::client::RetryOptions,
    http::response::error_detail,
    providers::EditRequest,
};

use super::{
    MultiReferenceParams, ToolEngine, ToolFailure,
    common::{
        default_basename, push_note_once, python_string_repr, resolve_key, saved_value,
        validation_error,
    },
};

impl ToolEngine {
    pub async fn image_multi_reference(
        &self,
        params: MultiReferenceParams,
    ) -> Result<Value, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(validation_error("prompt 不能为空"));
        }
        let reference_count = params.image_paths.len();
        if reference_count < 2 {
            return Ok(validation_error(format!(
                "至少需要 2 张参考图（收到 {reference_count}）。1 张请用 image_edit；0 张请用 image_generate。"
            )));
        }
        if reference_count > 10 {
            return Ok(validation_error(format!(
                "参考图最多 10 张，当前 {reference_count} 张。请减少或分批。"
            )));
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
        let (effective_model, mut notes) =
            resolve_model(params.model.as_deref(), &self.config.default_model, &size);
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        let stem = safe_stem.unwrap_or_else(|| default_basename("multiref"));
        let mut images = Vec::with_capacity(reference_count);
        let mut total_bytes = 0_u64;
        for (index, path) in params.image_paths.iter().enumerate() {
            let image = match self
                .input_store
                .validate_image(path, &format!("image_paths[{index}]"))
            {
                Ok(image) => image,
                Err(error) => return Ok(validation_error(error)),
            };
            total_bytes = total_bytes.saturating_add(image.size_bytes);
            if total_bytes > MAX_TOTAL_INPUT_BYTES {
                return Ok(validation_error(format!(
                    "参考图累计 {:.1}MB 超过总量上限 {}MB。请压缩或减少。",
                    total_bytes as f64 / 1024.0 / 1024.0,
                    MAX_TOTAL_INPUT_BYTES / 1024 / 1024
                )));
            }
            images.push(image);
        }
        let full_prompt = format!(
            "Reference images are provided. Synthesize their visual elements (style, palette, composition, subjects) into ONE single new image per the instruction below. Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\nInstruction:\n{}",
            params.prompt
        );
        let image_fields = images
            .iter()
            .map(|image| ("image[]", image))
            .collect::<Vec<_>>();
        let tier = size_tier(&size);
        let retry = RetryOptions {
            enabled: is_quality_model(&effective_model) || is_large_tier(tier),
            big_size: is_large_tier(tier),
        };
        let mut saved = None;
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
                .edit(
                    EditRequest {
                        model: &effective_model,
                        prompt: &full_prompt,
                        size: &size,
                        response_format,
                        images: &image_fields,
                        mask: None,
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
            output.insert("n_references".into(), Value::from(reference_count));
            output.insert("used_fallback".into(), Value::Bool(false));
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
        let size_honored = parse_size(&size) == Some(saved.actual_size);
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(true));
        output.insert("model".into(), Value::String(effective_model));
        output.insert("size".into(), Value::String(size));
        output.insert("used_fallback".into(), Value::Bool(false));
        output.insert("size_honored".into(), Value::Bool(size_honored));
        output.insert("n_references".into(), Value::from(reference_count));
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
    use image::{ImageFormat, Rgb, RgbImage};
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

    type CapturedReference = (String, String, String);
    type CapturedEdit = (String, String, Vec<CapturedReference>);

    struct FakeProvider {
        body: Vec<u8>,
        calls: AtomicUsize,
        captured: Mutex<Vec<CapturedEdit>>,
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
            request: EditRequest<'_>,
            _key: &SecretString,
            _retry: RetryOptions,
            _notes: &mut Vec<String>,
        ) -> ApiResponse {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.captured.lock().await.push((
                request.model.into(),
                request.prompt.into(),
                request
                    .images
                    .iter()
                    .map(|(field, image)| {
                        ((*field).into(), image.filename.clone(), image.mime.into())
                    })
                    .collect(),
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
        Vec<String>,
    ) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let input = temp.path().join("input");
        let out = temp.path().join("out");
        std::fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
        let mut paths = Vec::new();
        for (index, suffix) in ["png", "webp"].iter().enumerate() {
            let path = input.join(format!("ref-{index}.{suffix}"));
            RgbImage::from_pixel(32 + index as u32, 24 + index as u32, Rgb([1, 2, 3]))
                .save_with_format(&path, ImageFormat::Png)
                .unwrap_or_else(|error| panic!("{error}"));
            paths.push(path.to_string_lossy().into_owned());
        }
        let body = serde_json::to_vec(&serde_json::json!({
            "data":[{"b64_json":STANDARD.encode(std::fs::read(&paths[0]).unwrap_or_else(|error| panic!("{error}")))}]
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
        let app_paths = Arc::new(test_paths(temp.path(), environment));
        let provider = Arc::new(FakeProvider {
            body,
            calls: AtomicUsize::new(0),
            captured: Mutex::new(Vec::new()),
        });
        let storage =
            OutputStore::new(app_paths.as_ref()).unwrap_or_else(|error| panic!("{error}"));
        let http = HttpExecutor::new(config.as_ref(), app_paths.as_ref())
            .unwrap_or_else(|error| panic!("{error}"));
        let output = OutputSaver::new(
            config.clone(),
            storage.clone(),
            http,
            Arc::new(SystemResolver),
        );
        (
            temp,
            ToolEngine::new(config, app_paths, storage, output, provider.clone()),
            provider,
            paths,
        )
    }

    #[tokio::test]
    async fn multi_reference_uses_repeated_image_array_and_full_prompt() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_multi_reference(MultiReferenceParams {
                prompt: "融合成海报".into(),
                image_paths: paths,
                size: "1024x1024".into(),
                model: None,
                save_dir: None,
                basename: Some("multi".into()),
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["n_references"], 2);
        let captured = provider.captured.lock().await;
        assert!(captured[0].1.contains("Instruction:\n融合成海报"));
        assert_eq!(
            captured[0]
                .2
                .iter()
                .map(|item| item.0.as_str())
                .collect::<Vec<_>>(),
            ["image[]", "image[]"]
        );
        assert_eq!(captured[0].2[1].1, "ref-1.webp");
        assert_eq!(captured[0].2[1].2, "image/png");
    }

    #[tokio::test]
    async fn multi_reference_validates_count_before_provider() {
        let (_temp, engine, provider, paths) = fixture();
        let result = engine
            .image_multi_reference(MultiReferenceParams {
                prompt: "x".into(),
                image_paths: paths[..1].to_vec(),
                size: "1024x1024".into(),
                model: None,
                save_dir: None,
                basename: None,
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], false);
        assert!(
            result["error"]
                .as_str()
                .is_some_and(|text| text.contains("至少需要 2 张"))
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    }
}
