use serde_json::{Map, Value};

use crate::{
    domain::{
        basename::safe_basename,
        routing::ModelRoute,
        size::{parse_size, validate_size},
    },
    fs::input::MAX_TOTAL_INPUT_BYTES,
};

use super::{
    MultiReferenceParams, ToolEngine, ToolFailure,
    common::{default_basename, python_string_repr, resolve_key, saved_value, validation_error},
    image_job::{ImageJob, ImageJobInputs},
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
        if let Some(error) = self.model_error(
            params.model.as_deref(),
            &self.config.default_edit_model,
            params.api_key.as_ref(),
        ) {
            return Ok(validation_error(error));
        }
        let (cleaned_size, size_error) = validate_size(Some(&params.size), false);
        let Some(size) = cleaned_size else {
            return Ok(validation_error(
                size_error.unwrap_or_else(|| "size 校验失败".into()),
            ));
        };
        let (route, mut notes) = ModelRoute::resolve(
            params.model.as_deref(),
            &self.config.default_edit_model,
            &size,
        );
        let quality = match route.validate_quality(params.quality.as_deref()) {
            Ok(quality) => quality,
            Err(error) => return Ok(validation_error(error)),
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
        // No output_format is sent, so the job adds no format note.
        let job = self
            .image_job(ImageJob {
                route: &route,
                prompt: &full_prompt,
                size: &size,
                quality: quality.as_deref(),
                output_format: None,
                inputs: Some(ImageJobInputs {
                    images: &image_fields,
                    mask: None,
                }),
                location: &location,
                basename: &stem,
                key: &key,
                image_number: None,
            })
            .await;
        notes.extend(job.notes);
        let saved = match job.outcome {
            Ok(saved) => saved,
            Err(error) => {
                let mut output = Map::new();
                output.insert("ok".into(), Value::Bool(false));
                output.insert("model".into(), Value::String(job.model));
                output.insert("n_references".into(), Value::from(reference_count));
                output.insert("used_fallback".into(), Value::Bool(job.used_fallback));
                output.insert("error".into(), Value::String(error));
                output.insert(
                    "notes".into(),
                    Value::Array(notes.into_iter().map(Value::String).collect()),
                );
                return Ok(Value::Object(output));
            }
        };
        let size_honored = parse_size(&size) == Some(saved.actual_size);
        let mut output = Map::new();
        output.insert("ok".into(), Value::Bool(true));
        output.insert("model".into(), Value::String(job.model));
        output.insert("size".into(), Value::String(size));
        output.insert("used_fallback".into(), Value::Bool(job.used_fallback));
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
    use std::{collections::BTreeMap, sync::Arc};

    use image::{ImageFormat, Rgb, RgbImage};

    use crate::tools::test_support::{Endpoint, Reply, ScriptedProvider, tool_engine};

    use super::*;

    type CapturedReference = (String, String, String);
    type CapturedEdit = (String, String, Option<String>, Vec<CapturedReference>);

    /// Model, prompt, quality and each reference's field name, filename and
    /// MIME type of each edit request, in arrival order.
    fn captured_edits(provider: &ScriptedProvider) -> Vec<CapturedEdit> {
        provider
            .calls()
            .into_iter()
            .filter(|call| call.endpoint == Endpoint::Edit)
            .map(|call| {
                (
                    call.model,
                    call.prompt,
                    call.quality,
                    call.images
                        .into_iter()
                        .map(|image| (image.field, image.filename, image.mime))
                        .collect(),
                )
            })
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
        for (index, suffix) in ["png", "webp"].iter().enumerate() {
            let path = input.join(format!("ref-{index}.{suffix}"));
            RgbImage::from_pixel(32 + index as u32, 24 + index as u32, Rgb([1, 2, 3]))
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
        // Edits succeed; generation is refused.
        let provider = Arc::new(
            ScriptedProvider::answering(Reply::image(&output_image))
                .with_endpoint_reply(Endpoint::Generate, Reply::raw(500, Vec::new())),
        );
        let engine = tool_engine(temp.path(), environment, provider.clone());
        (temp, engine, provider, paths)
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
                quality: Some("max".into()),
                save_dir: None,
                basename: Some("multi".into()),
                api_key: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["n_references"], 2);
        let captured = captured_edits(&provider);
        assert!(captured[0].1.contains("Instruction:\n融合成海报"));
        assert_eq!(captured[0].2.as_deref(), Some("max"));
        assert_eq!(
            captured[0]
                .3
                .iter()
                .map(|item| item.0.as_str())
                .collect::<Vec<_>>(),
            ["image[]", "image[]"]
        );
        assert_eq!(captured[0].3[1].1, "ref-1.webp");
        assert_eq!(captured[0].3[1].2, "image/png");
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
                quality: None,
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
        assert_eq!(captured_edits(&provider).len(), 0);
    }
}
