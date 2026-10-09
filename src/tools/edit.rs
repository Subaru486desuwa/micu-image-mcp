use secrecy::ExposeSecret;
use serde_json::{Map, Value};

use crate::{
    domain::{
        basename::safe_basename,
        routing::{is_large_tier, is_quality_model, model_error, resolve_model, size_note},
        size::{size_tier, validate_quality, validate_size},
    },
    fs::input::validate_mask,
    http::client::RetryOptions,
    http::response::{error_detail, sanitize_sensitive},
    providers::EditRequest,
};

use super::{
    EditParams, ToolEngine, ToolFailure,
    common::{
        default_basename, push_note_once, python_string_repr, resolve_key, saved_value,
        validation_error,
    },
    edit_session::{EditSession, MAX_PROMPT_CHARS, Revision, sanitize_context},
};

impl ToolEngine {
    pub async fn image_edit(&self, params: EditParams) -> Result<Value, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(validation_error("prompt 不能为空"));
        }
        if params.prompt.chars().count() > MAX_PROMPT_CHARS {
            return Ok(validation_error("prompt 最多 32000 个字符"));
        }
        if params.session_id.is_none()
            && (params.parent_revision.is_some() || !params.context.is_empty())
        {
            return Ok(validation_error(
                "parent_revision 和 context 需要 session_id；用 new 开始会话",
            ));
        }
        let session = match params
            .session_id
            .as_deref()
            .map(|id| EditSession::open(&self.paths.save_root, id, params.parent_revision))
            .transpose()
        {
            Ok(session) => session,
            Err(error) => return Ok(validation_error(error)),
        };
        let parent = session.as_ref().and_then(|session| session.parent.as_ref());
        let requested_model = params
            .model
            .as_deref()
            .or(parent.map(|parent| parent.model.as_str()));
        if let Some(error) = model_error(requested_model, &self.config.default_edit_model) {
            return Ok(validation_error(error));
        }
        let requested_size = params
            .size
            .as_deref()
            .or(parent.map(|parent| parent.size.as_str()))
            .unwrap_or("1024x1024");
        let (cleaned_size, size_error) = validate_size(Some(requested_size), false);
        let Some(size) = cleaned_size else {
            return Ok(validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
            ));
        };
        let (effective_model, mut notes) =
            resolve_model(requested_model, &self.config.default_edit_model, &size);
        let requested_quality = params
            .quality
            .as_deref()
            .or(parent.and_then(|parent| parent.quality.as_deref()));
        let quality_value = requested_quality.map(|value| Value::String(value.to_owned()));
        let (quality, quality_error) = validate_quality(quality_value.as_ref(), &effective_model);
        if let Some(error) = quality_error {
            return Ok(validation_error(error));
        }
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
        let Some(image_path) = params
            .image_path
            .as_deref()
            .or(parent.map(|parent| parent.output_path.as_str()))
        else {
            return Ok(validation_error(
                "单次编辑或新会话需要 image_path；已有会话可自动使用上一版本",
            ));
        };
        let image = match self.input_store.validate_image(image_path, "image_path") {
            Ok(image) => image,
            Err(error) => return Ok(validation_error(error)),
        };
        if parent.is_some_and(|parent| image.path != std::path::Path::new(&parent.output_path)) {
            return Ok(validation_error(
                "image_path 必须与所选会话版本的输出一致；更换图片请用 new 开始新会话",
            ));
        }
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        // Persist only explicit editing state. Never serialize credentials or the tool arguments.
        // Validate caller limits before redaction can shorten base64-like values.
        if let Some(session) = &session
            && let Err(error) = session.context(&params.context)
        {
            return Ok(validation_error(error));
        }
        let instruction = sanitize_sensitive(
            &params.prompt,
            &[key.expose_secret(), self.config.api_key.expose_secret()],
        );
        let patch = sanitize_context(
            params.context,
            &[key.expose_secret(), self.config.api_key.expose_secret()],
        );
        let context = match session
            .as_ref()
            .map(|session| session.context(&patch))
            .transpose()
        {
            Ok(context) => context.unwrap_or_default(),
            Err(error) => return Ok(validation_error(error)),
        };
        let effective_prompt = match session
            .as_ref()
            .map(|session| session.prompt(&instruction, &context))
            .transpose()
        {
            Ok(prompt) => prompt.unwrap_or_else(|| params.prompt.clone()),
            Err(error) => return Ok(validation_error(error)),
        };
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
                        prompt: &effective_prompt,
                        size: &size,
                        quality: quality.as_deref(),
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
            if let Some(session) = &session {
                output.insert("session".into(), session.unchanged_value());
            }
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
        if let Some(session) = &session {
            let session_value = session.commit(Revision {
                revision: 0,
                parent_revision: None,
                input_path: image
                    .path
                    .to_str()
                    .ok_or_else(|| ToolFailure("输入路径不是合法 Unicode".into()))?
                    .to_owned(),
                output_path: saved
                    .path
                    .to_str()
                    .ok_or_else(|| ToolFailure("输出路径不是合法 Unicode".into()))?
                    .to_owned(),
                instruction,
                context,
                model: effective_model.clone(),
                size: size.clone(),
                quality: quality.clone(),
            });
            match session_value {
                Ok(value) => {
                    output.insert("session".into(), value);
                }
                Err(error) => {
                    let mut failure =
                        validation_error(format!("图片已保存，但会话提交失败：{error}"));
                    failure["saved"] = saved_value(&saved, None)?;
                    failure["session"] = session.unchanged_value();
                    return Ok(failure);
                }
            }
        }
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
            atomic::{AtomicU16, AtomicUsize, Ordering},
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

    type CapturedEdit = (String, String, Option<String>, Vec<String>, bool);

    struct FakeProvider {
        body: Vec<u8>,
        edits: Mutex<Vec<CapturedEdit>>,
        prompts_and_inputs: Mutex<Vec<(String, Vec<u8>)>>,
        calls: AtomicUsize,
        status: AtomicU16,
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
                request.quality.map(str::to_owned),
                request
                    .images
                    .iter()
                    .map(|(field, _)| (*field).into())
                    .collect(),
                request.mask.is_some(),
            ));
            self.prompts_and_inputs.lock().await.push((
                request.prompt.into(),
                std::fs::read(request.images[0].1.file.path()).unwrap(),
            ));
            ApiResponse {
                status: self.status.load(Ordering::SeqCst),
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
        let fake_output = temp.path().join("fake-output.png");
        RgbImage::from_pixel(32, 24, Rgb([40, 50, 60]))
            .save(&fake_output)
            .unwrap();
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": STANDARD.encode(std::fs::read(&fake_output).unwrap_or_else(|error| panic!("{error}")))}]
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
            prompts_and_inputs: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            status: AtomicU16::new(200),
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
                image_path: Some(source.to_string_lossy().into_owned()),
                mask_path: Some(mask.to_string_lossy().into_owned()),
                size: Some("1024x1024".into()),
                model: None,
                quality: Some("max".into()),
                save_dir: None,
                basename: Some("edited".into()),
                api_key: None,
                session_id: None,
                parent_revision: None,
                context: Default::default(),
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["saved"]["actual_size"], "32x24");
        assert_eq!(
            provider.edits.lock().await.as_slice(),
            &[(
                "gpt-image-2.5-sunburst".into(),
                "1024x1024".into(),
                Some("max".into()),
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
                image_path: Some(source.to_string_lossy().into_owned()),
                mask_path: None,
                size: Some("3840x2160".into()),
                model: Some("gpt-image-2".into()),
                quality: None,
                save_dir: None,
                basename: Some("high".into()),
                api_key: None,
                session_id: None,
                parent_revision: None,
                context: Default::default(),
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(high["model"], "gpt-image-2-openai", "{high}");
        let missing = engine
            .image_edit(EditParams {
                prompt: "x".into(),
                image_path: Some("/definitely/missing.png".into()),
                mask_path: None,
                size: Some("1024x1024".into()),
                model: None,
                quality: None,
                save_dir: None,
                basename: None,
                api_key: None,
                session_id: None,
                parent_revision: None,
                context: Default::default(),
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

    fn session_params(value: Value) -> EditParams {
        serde_json::from_value(value).unwrap()
    }

    fn allow_session_outputs(engine: &mut ToolEngine, root: &std::path::Path) {
        let mut paths = (*engine.paths).clone();
        paths.input_root = Some(root.to_owned());
        engine.input_store = crate::fs::input::InputStore::new(&paths);
        engine.paths = Arc::new(paths);
    }

    #[tokio::test]
    async fn session_continues_previous_output_inherits_settings_and_merges_context() {
        let (temp, mut engine, provider, source, _mask) = fixture();
        allow_session_outputs(&mut engine, temp.path());
        let first = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "make clothing red", "image_path": source,
                "session_id": "new", "context": {"identity": "same face", "clothing": "red shirt"},
                "model": "gpt-image-2.5-flare", "size": "1536x1024", "quality": "medium",
            })))
            .await
            .unwrap();
        assert_eq!(first["ok"], true, "{first}");
        let second = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "change background to seaside", "session_id": first["session"]["id"],
                "context": {"background": "seaside"},
            })))
            .await
            .unwrap();
        assert_eq!(second["ok"], true, "{second}");
        assert_eq!(second["session"]["revision"], 2);
        assert_eq!(second["session"]["parent_revision"], 1);
        assert_eq!(second["session"]["context"]["identity"], "same face");
        assert_eq!(second["session"]["context"]["clothing"], "red shirt");
        assert_eq!(second["session"]["input_path"], first["saved"]["path"]);
        assert_eq!(second["model"], "gpt-image-2.5-flare");
        assert_eq!(second["size"], "1536x1024");
        let captured = provider.prompts_and_inputs.lock().await;
        assert_eq!(
            captured[1].1,
            std::fs::read(first["saved"]["path"].as_str().unwrap()).unwrap()
        );
        assert_ne!(captured[1].1, std::fs::read(source).unwrap());
        assert!(captured[1].0.contains("make clothing red"));
        assert!(
            captured[1]
                .0
                .contains("CURRENT EDIT INSTRUCTION:\nchange background to seaside")
        );
        assert_eq!(provider.edits.lock().await[1].2.as_deref(), Some("medium"));
    }

    #[tokio::test]
    async fn failed_round_does_not_persist_its_instruction_or_context() {
        let (temp, mut engine, provider, source, _mask) = fixture();
        allow_session_outputs(&mut engine, temp.path());
        let first = engine.image_edit(session_params(serde_json::json!({
            "prompt": "first", "image_path": source, "session_id": "new", "context": {"clothing": "red"},
        }))).await.unwrap();
        let id = &first["session"]["id"];
        provider.status.store(400, Ordering::SeqCst);
        let failed = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "failed instruction", "session_id": id, "context": {"clothing": "green"},
            })))
            .await
            .unwrap();
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["session"]["revision"], 1);
        assert_eq!(failed["session"]["context"]["clothing"], "red");
        provider.status.store(200, Ordering::SeqCst);
        let success = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "make clothing blue", "session_id": id, "context": {"clothing": "blue"},
            })))
            .await
            .unwrap();
        assert_eq!(success["session"]["revision"], 2);
        assert_eq!(success["session"]["context"]["clothing"], "blue");
        let captured = provider.prompts_and_inputs.lock().await;
        assert!(!captured[2].0.contains("failed instruction"));
        assert!(!captured[2].0.contains("green"));
    }

    #[tokio::test]
    async fn session_preserves_input_root_and_rejects_mismatched_image_or_missing_input() {
        let (_temp, engine, provider, source, _mask) = fixture();
        let first = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "first", "image_path": source, "session_id": "new",
            })))
            .await
            .unwrap();
        let second = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "second", "session_id": first["session"]["id"],
            })))
            .await
            .unwrap();
        assert_eq!(second["ok"], false);
        assert!(
            second["error"]
                .as_str()
                .unwrap()
                .contains("MICU_INPUT_ROOT")
        );
        let mismatched = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "mismatch", "image_path": source, "session_id": first["session"]["id"],
            })))
            .await
            .unwrap();
        assert_eq!(mismatched["ok"], false);
        let missing = engine
            .image_edit(session_params(
                serde_json::json!({"prompt": "missing input"}),
            ))
            .await
            .unwrap();
        assert_eq!(missing["ok"], false);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn session_metadata_never_serializes_override_or_environment_credentials() {
        let (temp, engine, _provider, source, _mask) = fixture();
        let first = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "keep face, accidental sk-override-secret", "image_path": source,
                "session_id": "new", "api_key": "sk-override-secret",
                "context": {"identity": "same face sk-test"},
            })))
            .await
            .unwrap();
        assert_eq!(first["ok"], true, "{first}");
        let metadata = std::fs::read_to_string(
            temp.path()
                .join("out/.micu-sessions")
                .join(first["session"]["id"].as_str().unwrap())
                .join("revision-1.json"),
        )
        .unwrap();
        assert!(!metadata.contains("sk-override-secret"));
        assert!(!metadata.contains("sk-test"));
        assert!(!metadata.contains("api_key"));
        assert!(metadata.contains("[REDACTED]"));
    }
}
