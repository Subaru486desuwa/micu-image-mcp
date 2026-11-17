use futures_util::{StreamExt, stream};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value};

use crate::{
    domain::{
        basename::safe_basename,
        routing::{ModelRoute, infer_size_from_prompt},
        size::{validate_n, validate_size},
    },
    fs::output_store::{OutputDirectory, SavedImage},
    http::response::sanitize_sensitive,
};

use super::{
    EditParams, GenerateParams, ToolEngine, ToolFailure,
    common::{
        default_basename, push_note_once, python_string_repr, resolve_key, saved_value,
        validation_error,
    },
    edit::EditOutcome,
    edit_session::{EditSession, MAX_PROMPT_CHARS, Revision, sanitize_context},
    image_job::{ImageJob, ImageJobResult},
};

impl ToolEngine {
    pub async fn image_generate(&self, params: GenerateParams) -> Result<Value, ToolFailure> {
        if params.prompt.trim().is_empty() {
            return Ok(validation_error("prompt 不能为空"));
        }
        if let Some(error) = validate_n(&Value::from(params.n)) {
            return Ok(validation_error(error));
        }
        if params.session_id.is_none()
            && (params.parent_revision.is_some() || !params.context.is_empty())
        {
            return Ok(validation_error(
                "parent_revision 和 context 需要 session_id；用 new 开始会话",
            ));
        }
        if params.session_id.is_some() && params.n != 1 {
            return Ok(validation_error(
                "会话生成仅支持 n=1；多张候选请省略 session_id，再用所选图片开始编辑会话",
            ));
        }
        if params.session_id.as_deref().is_some_and(|id| id != "new") {
            return self.continue_generation(params).await;
        }
        if params.session_id.is_some() && params.prompt.chars().count() > MAX_PROMPT_CHARS {
            return Ok(validation_error("prompt 最多 32000 个字符"));
        }
        if let Some(error) = self.model_error(
            params.model.as_deref(),
            &self.config.default_model,
            params.api_key.as_ref(),
        ) {
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
        let (route, mut notes) =
            ModelRoute::resolve(params.model.as_deref(), &self.config.default_model, &size);
        if let Some(error) = route.size_error(&size) {
            return Ok(validation_error(error));
        }
        let quality = match route.validate_quality(params.quality.as_deref()) {
            Ok(quality) => quality,
            Err(error) => return Ok(validation_error(error)),
        };
        let output_format = match route.validate_output_format(params.output_format.as_deref()) {
            Ok(output_format) => output_format,
            Err(error) => return Ok(validation_error(error)),
        };
        let location = match self
            .output_store
            .resolve_save_dir(params.save_dir.as_deref())
        {
            Ok(location) => location,
            Err(error) => return Ok(validation_error(error)),
        };
        if let Some(note) = inferred_note {
            notes.insert(0, note);
        }
        notes.extend(route.dropped_quality_note(params.quality.as_deref()));
        let key = resolve_key(self.config.as_ref(), params.api_key)?;
        let session = match params
            .session_id
            .as_deref()
            .map(|id| EditSession::open(&self.paths.save_root, id, params.parent_revision))
            .transpose()
        {
            Ok(session) => session,
            Err(error) => return Ok(validation_error(error)),
        };
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
            .map(|session| session.generation_prompt(&instruction, &context))
            .transpose()
        {
            Ok(prompt) => prompt.unwrap_or_else(|| params.prompt.clone()),
            Err(error) => return Ok(validation_error(error)),
        };
        let (requested_n, n_note) = route.cap_n(params.n as usize);
        notes.extend(n_note);
        let concurrency = if requested_n > 1 {
            route.parallel_limit()
        } else {
            1
        };
        let stem = safe_stem.unwrap_or_else(|| default_basename("gen"));

        let mut results = if concurrency > 1 {
            stream::iter(0..requested_n)
                .map(|index| {
                    let engine = self.clone();
                    let prompt = effective_prompt.clone();
                    let size = size.clone();
                    let route = route.clone();
                    let quality = quality.clone();
                    let output_format = output_format.clone();
                    let location = location.clone();
                    let stem = stem.clone();
                    let key = key.clone();
                    async move {
                        engine
                            .generate_one(
                                index,
                                &prompt,
                                &size,
                                &route,
                                quality.as_deref(),
                                output_format.as_deref(),
                                &location,
                                &stem,
                                &key,
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
                        &effective_prompt,
                        &size,
                        &route,
                        quality.as_deref(),
                        output_format.as_deref(),
                        &location,
                        &stem,
                        &key,
                    )
                    .await,
                );
            }
            sequential
        };
        results.sort_by_key(|(index, _)| *index);
        if concurrency > 1 {
            notes.push(format!(
                "{} + N={requested_n} 已 {concurrency} 并发",
                route.parallel_label()
            ));
        }

        let mut saved = Vec::new();
        let mut errors = Vec::new();
        let mut used_fallback = false;
        let mut actual_model = route.model().to_owned();
        for (index, job) in results {
            used_fallback |= job.used_fallback;
            if saved.is_empty() {
                actual_model = job.model.clone();
            }
            for note in job.notes {
                push_note_once(&mut notes, note);
            }
            match job.outcome {
                Ok(image) => {
                    let mut value = saved_value(&image, Some(index + 1))?;
                    value["model"] = Value::String(job.model);
                    saved.push(value);
                }
                Err(error) => errors.push(Value::String(error)),
            }
        }
        let requested_dimensions = crate::domain::size::parse_size(&size);
        let size_honored = !saved.is_empty()
            && saved.iter().all(|entry| {
                entry
                    .get("actual_size")
                    .and_then(Value::as_str)
                    .and_then(crate::domain::size::parse_size)
                    == requested_dimensions
            });
        let mut output = Map::new();
        let mut committed = true;
        if let Some(session) = &session
            && let Some(image) = saved.first()
        {
            let session_value = session.commit(Revision {
                revision: 0,
                parent_revision: None,
                // The first revision has no input image; its generated output starts history.
                input_path: String::new(),
                output_path: image["path"]
                    .as_str()
                    .ok_or_else(|| ToolFailure("输出路径不是合法 Unicode".into()))?
                    .to_owned(),
                instruction,
                context,
                model: actual_model.clone(),
                size: size.clone(),
                quality: quality.clone(),
            });
            match session_value {
                Ok(value) => {
                    output.insert("session".into(), value);
                }
                Err(error) => {
                    committed = false;
                    errors.push(Value::String(format!(
                        "图片已保存，但会话提交失败：{error}"
                    )));
                    output.insert("session".into(), session.unchanged_value());
                }
            }
        }
        output.insert("ok".into(), Value::Bool(!saved.is_empty() && committed));
        output.insert("model".into(), Value::String(actual_model));
        output.insert("size".into(), Value::String(size));
        output.insert("requested_n".into(), Value::from(requested_n));
        output.insert("used_fallback".into(), Value::Bool(used_fallback));
        output.insert("size_honored".into(), Value::Bool(size_honored));
        output.insert("saved".into(), Value::Array(saved));
        output.insert("errors".into(), Value::Array(errors));
        output.insert(
            "notes".into(),
            Value::Array(notes.into_iter().map(Value::String).collect()),
        );
        Ok(Value::Object(output))
    }

    async fn continue_generation(&self, params: GenerateParams) -> Result<Value, ToolFailure> {
        // A continued generation uses the selected image and the same session machinery as edits.
        // Keep image_generate's list-shaped result so callers can use either tool in one session.
        let outcome = self
            .edit_one(EditParams {
                prompt: params.prompt,
                image_path: None,
                mask_path: None,
                size: params.size,
                model: params.model,
                quality: params.quality,
                output_format: params.output_format,
                save_dir: params.save_dir,
                basename: params.basename,
                api_key: params.api_key,
                session_id: params.session_id,
                parent_revision: params.parent_revision,
                context: params.context,
            })
            .await?;
        match outcome {
            EditOutcome::Saved {
                saved,
                model,
                used_fallback,
                size,
                mut notes,
                session,
            } => {
                let size_honored =
                    crate::domain::size::parse_size(&size) == Some(saved.actual_size);
                notes.push("已有会话通过参考编辑延续上一版本图片与上下文".into());
                // Existing shape: the list fields follow `notes`.
                let mut output = Map::new();
                if let Some(session) = session {
                    output.insert("session".into(), session);
                }
                output.insert("ok".into(), Value::Bool(true));
                output.insert("model".into(), Value::String(model));
                output.insert("size".into(), Value::String(size));
                output.insert("used_fallback".into(), Value::Bool(used_fallback));
                output.insert(
                    "saved".into(),
                    Value::Array(vec![continued_saved_value(&saved)?]),
                );
                output.insert(
                    "notes".into(),
                    Value::Array(notes.into_iter().map(Value::String).collect()),
                );
                output.insert("requested_n".into(), Value::from(1));
                output.insert("size_honored".into(), Value::Bool(size_honored));
                output.insert("errors".into(), Value::Array(Vec::new()));
                Ok(Value::Object(output))
            }
            EditOutcome::SessionCommitFailed {
                saved,
                error,
                session,
            } => {
                // Existing shape: no `size`, and `size_honored` is always false.
                let mut output = Map::new();
                output.insert("ok".into(), Value::Bool(false));
                output.insert("error".into(), Value::String(error.clone()));
                output.insert("errors".into(), Value::Array(vec![Value::String(error)]));
                output.insert(
                    "saved".into(),
                    Value::Array(vec![continued_saved_value(&saved)?]),
                );
                output.insert("session".into(), session);
                output.insert("requested_n".into(), Value::from(1));
                output.insert("size_honored".into(), Value::Bool(false));
                Ok(Value::Object(output))
            }
            EditOutcome::Failed {
                model,
                used_fallback,
                size,
                notes,
                error,
                session,
            } => {
                // The fields and key order of the edit failure shape come first; the list
                // fields follow, so one caller can handle both kinds of failed generation.
                // The error carries no `#N ` prefix and the continuation note is not added.
                let mut output = Map::new();
                output.insert("ok".into(), Value::Bool(false));
                output.insert("model".into(), Value::String(model));
                output.insert("used_fallback".into(), Value::Bool(used_fallback));
                output.insert("size".into(), Value::String(size));
                if let Some(session) = session {
                    output.insert("session".into(), session);
                }
                output.insert("error".into(), Value::String(error.clone()));
                output.insert(
                    "notes".into(),
                    Value::Array(notes.into_iter().map(Value::String).collect()),
                );
                output.insert("saved".into(), Value::Array(Vec::new()));
                output.insert("requested_n".into(), Value::from(1));
                output.insert("size_honored".into(), Value::Bool(false));
                output.insert("errors".into(), Value::Array(vec![Value::String(error)]));
                Ok(Value::Object(output))
            }
            // Existing shape: a rejected continuation keeps the validation-error shape.
            outcome @ EditOutcome::Invalid(_) => outcome.into_edit_value(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn generate_one(
        &self,
        index: usize,
        prompt: &str,
        size: &str,
        route: &ModelRoute,
        quality: Option<&str>,
        output_format: Option<&str>,
        location: &OutputDirectory,
        stem: &str,
        key: &SecretString,
    ) -> (usize, ImageJobResult) {
        let basename = format!("{stem}_{}", index + 1);
        let job = self
            .image_job(ImageJob {
                route,
                prompt,
                size,
                quality,
                output_format,
                inputs: None,
                location,
                basename: &basename,
                key,
                image_number: Some(index + 1),
            })
            .await;
        (index, job)
    }
}

/// A continued generation lists its one image with `index` as the last key.
fn continued_saved_value(saved: &SavedImage) -> Result<Value, ToolFailure> {
    let mut value = saved_value(saved, None)?;
    value["index"] = Value::from(1);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use image::{ImageFormat, Rgb, RgbImage};

    use crate::tools::test_support::{Endpoint, Reply, ScriptedProvider, tool_engine};

    use super::*;

    /// Model, size and `response_format` of each generation request, in arrival order.
    fn generate_calls(provider: &ScriptedProvider) -> Vec<(String, String, Option<String>)> {
        provider
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == Endpoint::Generate)
            .map(|call| (call.model, call.size, call.response_format))
            .collect()
    }

    fn fixture() -> (tempfile::TempDir, ToolEngine, Arc<ScriptedProvider>) {
        let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let out = temp.path().join("out");
        let environment = BTreeMap::from([
            (
                "MICU_SAVE_DIR_ROOT".into(),
                out.to_string_lossy().into_owned(),
            ),
            ("MICU_SAVE_DIR".into(), out.to_string_lossy().into_owned()),
            ("MICU_API_KEY".into(), "sk-test".into()),
            ("MICU_BASEURL".into(), "http://127.0.0.1:9".into()),
        ]);
        let image_path = temp.path().join("fixture.png");
        RgbImage::from_pixel(32, 24, Rgb([1, 2, 3]))
            .save_with_format(&image_path, ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let image = std::fs::read(image_path).unwrap_or_else(|error| panic!("{error}"));
        // Generation is held open so overlap is observable; edits are refused.
        let provider = Arc::new(
            ScriptedProvider::answering(Reply::image(&image))
                .with_delay(Duration::from_millis(30))
                .with_endpoint_reply(Endpoint::Edit, Reply::raw(500, Vec::new())),
        );
        let engine = tool_engine(temp.path(), environment, provider.clone());
        (temp, engine, provider)
    }

    fn params() -> GenerateParams {
        GenerateParams {
            prompt: "一只猫".into(),
            size: Some("1024x1024".into()),
            n: 1,
            model: None,
            quality: None,
            output_format: None,
            save_dir: None,
            basename: Some("test".into()),
            api_key: None,
            session_id: None,
            parent_revision: None,
            context: BTreeMap::new(),
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
        assert_eq!(result["model"], "gpt-image-2.6-flare");
        assert_eq!(result["requested_n"], 1);
        assert_eq!(result["saved"][0]["actual_size"], "32x24");
        assert_eq!(
            generate_calls(&provider).as_slice(),
            &[("gpt-image-2.6-flare".into(), "1024x1024".into(), None)]
        );
    }

    #[tokio::test]
    async fn generate_forces_high_resolution_to_one_and_routes_quality() {
        let (_temp, engine, provider) = fixture();
        let mut request = params();
        request.size = Some("2048x2048".into());
        request.model = Some("gpt-image-2".into());
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
        assert_eq!(generate_calls(&provider).len(), 1);
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
        assert_eq!(provider.peak_in_flight(), 5);
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
