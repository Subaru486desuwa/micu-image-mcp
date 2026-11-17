use secrecy::ExposeSecret;
use serde_json::{Map, Value};

use crate::{
    domain::{basename::safe_basename, routing::ModelRoute, size::validate_size},
    fs::{input::validate_mask, output_store::SavedImage},
    http::response::sanitize_sensitive,
};

use super::{
    EditParams, ToolEngine, ToolFailure,
    common::{default_basename, python_string_repr, resolve_key, saved_value, validation_error},
    edit_session::{EditSession, MAX_PROMPT_CHARS, Revision, sanitize_context},
    image_job::{ImageJob, ImageJobInputs},
};

const GIF_SESSION_INPUT_ERROR: &str = "所选会话版本是 GIF，不能作为编辑输入；用 parent_revision 选择非 GIF 版本，或转换为 PNG、JPEG、WebP 后用 session_id=new 开始新会话";

fn is_gif_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
}

/// How one single-image edit ended. `image_edit`, `image_batch_edit` and a
/// continued `image_generate` each render their own public shape from it.
pub(super) enum EditOutcome {
    /// The request was rejected before any upstream call.
    Invalid(String),
    /// The upstream request or the save failed; an open session did not advance.
    Failed {
        model: String,
        used_fallback: bool,
        size: String,
        notes: Vec<String>,
        error: String,
        /// Unchanged state of the session, when the edit ran in one.
        session: Option<Value>,
    },
    Saved {
        saved: SavedImage,
        model: String,
        used_fallback: bool,
        size: String,
        notes: Vec<String>,
        /// New state of the session, when the edit ran in one.
        session: Option<Value>,
    },
    /// The image is on disk, but the session could not record the revision.
    SessionCommitFailed {
        saved: SavedImage,
        error: String,
        /// Unchanged state of the session.
        session: Value,
    },
}

impl EditOutcome {
    fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    /// Renders the public `image_edit` shape. Key order is part of that shape.
    pub(super) fn into_edit_value(self) -> Result<Value, ToolFailure> {
        match self {
            Self::Invalid(message) => Ok(validation_error(message)),
            Self::Failed {
                model,
                used_fallback,
                size,
                notes,
                error,
                session,
            } => {
                let mut output = Map::new();
                output.insert("ok".into(), Value::Bool(false));
                output.insert("model".into(), Value::String(model));
                output.insert("used_fallback".into(), Value::Bool(used_fallback));
                output.insert("size".into(), Value::String(size));
                if let Some(session) = session {
                    output.insert("session".into(), session);
                }
                output.insert("error".into(), Value::String(error));
                output.insert(
                    "notes".into(),
                    Value::Array(notes.into_iter().map(Value::String).collect()),
                );
                Ok(Value::Object(output))
            }
            Self::Saved {
                saved,
                model,
                used_fallback,
                size,
                notes,
                session,
            } => {
                let mut output = Map::new();
                if let Some(session) = session {
                    output.insert("session".into(), session);
                }
                output.insert("ok".into(), Value::Bool(true));
                output.insert("model".into(), Value::String(model));
                output.insert("size".into(), Value::String(size));
                output.insert("used_fallback".into(), Value::Bool(used_fallback));
                output.insert("saved".into(), saved_value(&saved, None)?);
                output.insert(
                    "notes".into(),
                    Value::Array(notes.into_iter().map(Value::String).collect()),
                );
                Ok(Value::Object(output))
            }
            Self::SessionCommitFailed {
                saved,
                error,
                session,
            } => {
                let mut failure = validation_error(error);
                failure["saved"] = saved_value(&saved, None)?;
                failure["session"] = session;
                Ok(failure)
            }
        }
    }
}

impl ToolEngine {
    pub async fn image_edit(&self, params: EditParams) -> Result<Value, ToolFailure> {
        self.edit_one(params).await?.into_edit_value()
    }

    /// Validates, routes and runs one single-image edit, including its session round.
    pub(super) async fn edit_one(&self, params: EditParams) -> Result<EditOutcome, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(EditOutcome::invalid("prompt 不能为空"));
        }
        if params.prompt.chars().count() > MAX_PROMPT_CHARS {
            return Ok(EditOutcome::invalid("prompt 最多 32000 个字符"));
        }
        if params.session_id.is_none()
            && (params.parent_revision.is_some() || !params.context.is_empty())
        {
            return Ok(EditOutcome::invalid(
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
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        let parent = session.as_ref().and_then(|session| session.parent.as_ref());
        let requested_model = params
            .model
            .as_deref()
            .or(parent.map(|parent| parent.model.as_str()));
        if let Some(error) = self.edit_model_error(requested_model, params.api_key.as_ref()) {
            return Ok(EditOutcome::invalid(error));
        }
        let requested_size = params
            .size
            .as_deref()
            .or(parent.map(|parent| parent.size.as_str()))
            .unwrap_or("1024x1024");
        let (cleaned_size, size_error) = validate_size(Some(requested_size), false);
        let Some(size) = cleaned_size else {
            return Ok(EditOutcome::invalid(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
            ));
        };
        let (route, mut notes) =
            ModelRoute::resolve(requested_model, &self.config.default_edit_model, &size);
        let requested_quality = params
            .quality
            .as_deref()
            .or(parent.and_then(|parent| parent.quality.as_deref()));
        let quality = match route.validate_quality(requested_quality) {
            Ok(quality) => quality,
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        let output_format = match route.validate_output_format(params.output_format.as_deref()) {
            Ok(output_format) => output_format,
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        let safe_stem = params
            .basename
            .as_deref()
            .and_then(|name| safe_basename(Some(name)));
        if params.basename.is_some() && safe_stem.is_none() {
            return Ok(EditOutcome::invalid(format!(
                "basename {} 含非法字符或路径分量",
                python_string_repr(params.basename.as_deref().unwrap_or_default())
            )));
        }
        let location = match self
            .output_store
            .resolve_save_dir(params.save_dir.as_deref())
        {
            Ok(location) => location,
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        if parent.is_some_and(|parent| is_gif_path(&parent.output_path)) {
            return Ok(EditOutcome::invalid(GIF_SESSION_INPUT_ERROR));
        }
        let Some(image_path) = params
            .image_path
            .as_deref()
            .or(parent.map(|parent| parent.output_path.as_str()))
        else {
            return Ok(EditOutcome::invalid(
                "单次编辑或新会话需要 image_path；已有会话可自动使用上一版本",
            ));
        };
        let image = match self.input_store.validate_image(image_path, "image_path") {
            Ok(image) => image,
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        if parent.is_some_and(|parent| image.path != std::path::Path::new(&parent.output_path)) {
            return Ok(EditOutcome::invalid(
                "image_path 必须与所选会话版本的输出一致；更换图片请用 new 开始新会话",
            ));
        }
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        // Persist only explicit editing state. Never serialize credentials or the tool arguments.
        // Validate caller limits before redaction can shorten base64-like values.
        if let Some(session) = &session
            && let Err(error) = session.context(&params.context)
        {
            return Ok(EditOutcome::invalid(error));
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
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        let effective_prompt = match session
            .as_ref()
            .map(|session| session.prompt(&instruction, &context))
            .transpose()
        {
            Ok(prompt) => prompt.unwrap_or_else(|| params.prompt.clone()),
            Err(error) => return Ok(EditOutcome::invalid(error)),
        };
        let mask = if let Some(mask_path) = params.mask_path {
            let mask = match self.input_store.validate_image(&mask_path, "mask_path") {
                Ok(mask) => mask,
                Err(error) => return Ok(EditOutcome::invalid(error)),
            };
            if let Err(error) = validate_mask(&mask, &image) {
                return Ok(EditOutcome::invalid(error));
            }
            Some(mask)
        } else {
            None
        };
        let stem = safe_stem.unwrap_or_else(|| default_basename("edit"));
        let images = [("image", &image)];
        let job = self
            .image_job(ImageJob {
                route: &route,
                prompt: &effective_prompt,
                size: &size,
                quality: quality.as_deref(),
                output_format: output_format.as_deref(),
                inputs: Some(ImageJobInputs {
                    images: &images,
                    mask: mask.as_ref(),
                }),
                location: &location,
                basename: &stem,
                key: &key,
                image_number: None,
            })
            .await;
        notes.extend(job.notes);
        let actual_model = job.model;
        let used_fallback = job.used_fallback;
        let saved = match job.outcome {
            Ok(saved) => saved,
            Err(error) => {
                return Ok(EditOutcome::Failed {
                    model: actual_model,
                    used_fallback,
                    size,
                    notes,
                    error,
                    session: session.as_ref().map(EditSession::unchanged_value),
                });
            }
        };
        let mut session_value = None;
        if let Some(session) = &session {
            let committed = session.commit(Revision {
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
                model: actual_model.clone(),
                size: size.clone(),
                quality: quality.clone(),
            });
            match committed {
                Ok(value) => session_value = Some(value),
                Err(error) => {
                    return Ok(EditOutcome::SessionCommitFailed {
                        saved,
                        error: format!("图片已保存，但会话提交失败：{error}"),
                        session: session.unchanged_value(),
                    });
                }
            }
        }
        Ok(EditOutcome::Saved {
            saved,
            model: actual_model,
            used_fallback,
            size,
            notes,
            session: session_value,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use image::{ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

    use crate::tools::test_support::{Endpoint, Reply, ScriptedProvider, tool_engine};

    use super::*;

    type CapturedEdit = (String, String, Option<String>, Vec<String>, bool);

    /// Model, size, quality, input field names and mask presence of each edit
    /// request, in arrival order.
    fn edits(provider: &ScriptedProvider) -> Vec<CapturedEdit> {
        provider
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == Endpoint::Edit)
            .map(|call| {
                (
                    call.model,
                    call.size,
                    call.quality,
                    call.images.into_iter().map(|image| image.field).collect(),
                    call.has_mask,
                )
            })
            .collect()
    }

    /// Prompt and first input image of each edit request, in arrival order.
    fn prompts_and_inputs(provider: &ScriptedProvider) -> Vec<(String, Vec<u8>)> {
        provider
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == Endpoint::Edit)
            .map(|call| {
                let input = call.images.into_iter().next().unwrap().bytes;
                (call.prompt, input)
            })
            .collect()
    }

    fn fixture() -> (
        tempfile::TempDir,
        ToolEngine,
        Arc<ScriptedProvider>,
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
        let output_image = std::fs::read(&fake_output).unwrap_or_else(|error| panic!("{error}"));
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
        // Edits succeed unless a test queues another reply; generation is refused.
        let provider = Arc::new(
            ScriptedProvider::answering(Reply::image(&output_image))
                .with_endpoint_reply(Endpoint::Generate, Reply::raw(500, Vec::new())),
        );
        let engine = tool_engine(temp.path(), environment, provider.clone());
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
                output_format: None,
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
            edits(&provider).as_slice(),
            &[(
                "gpt-image-2.6-sunburst".into(),
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
                output_format: None,
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
                output_format: None,
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
        assert_eq!(edits(&provider).len(), 1);
    }

    fn session_params(value: Value) -> EditParams {
        serde_json::from_value(value).unwrap()
    }

    fn allow_session_outputs(engine: &mut ToolEngine, root: &std::path::Path) {
        let mut paths = (*engine.paths).clone();
        // Production AppPaths canonicalizes roots; macOS /var is a symlink to /private/var.
        paths.input_root = Some(root.canonicalize().unwrap());
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
                "model": "gpt-image-2.5-flare-openai", "size": "1536x1024", "quality": "medium",
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
        let captured = prompts_and_inputs(&provider);
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
        assert_eq!(edits(&provider)[1].2.as_deref(), Some("medium"));
    }

    #[tokio::test]
    async fn failed_round_does_not_persist_its_instruction_or_context() {
        let (temp, mut engine, provider, source, _mask) = fixture();
        allow_session_outputs(&mut engine, temp.path());
        let first = engine.image_edit(session_params(serde_json::json!({
            "prompt": "first", "image_path": source, "session_id": "new", "context": {"clothing": "red"},
        }))).await.unwrap();
        assert_eq!(first["ok"], true, "{first}");
        let id = &first["session"]["id"];
        // Only the next edit is rejected; later ones get the default answer again.
        provider.enqueue(Reply::json(
            400,
            serde_json::json!({"error": {"message": "rejected"}}),
        ));
        let failed = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "failed instruction", "session_id": id, "context": {"clothing": "green"},
            })))
            .await
            .unwrap();
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["session"]["revision"], 1);
        assert_eq!(failed["session"]["context"]["clothing"], "red");
        let success = engine
            .image_edit(session_params(serde_json::json!({
                "prompt": "make clothing blue", "session_id": id, "context": {"clothing": "blue"},
            })))
            .await
            .unwrap();
        assert_eq!(success["session"]["revision"], 2);
        assert_eq!(success["session"]["context"]["clothing"], "blue");
        let captured = prompts_and_inputs(&provider);
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
        assert_eq!(edits(&provider).len(), 1);
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
