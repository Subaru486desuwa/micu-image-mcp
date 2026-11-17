//! One image job: turn a resolved request into one saved image or one error.
//!
//! The job negotiates `response_format`, calls the provider, saves the first
//! returned image and produces every note that belongs to a single image.
//! Fan-out, pacing between images and basename computation stay with the tools.

use secrecy::{ExposeSecret, SecretString};

use crate::{
    domain::routing::{ModelRoute, output_format_note, size_note},
    fs::{
        input::ValidatedImage,
        output_store::{OutputDirectory, SavedImage},
    },
    http::response::error_detail,
    providers::{EditRequest, GenerateRequest},
};

use super::{ToolEngine, common::push_note_once};

/// A resolved request for one image. Validation and routing are already done.
pub(super) struct ImageJob<'a> {
    pub route: &'a ModelRoute,
    pub prompt: &'a str,
    pub size: &'a str,
    pub quality: Option<&'a str>,
    pub output_format: Option<&'a str>,
    /// `None` generates from text; `Some` edits the given input images.
    pub inputs: Option<ImageJobInputs<'a>>,
    pub location: &'a OutputDirectory,
    /// Final file stem; the caller has already applied its naming rule.
    pub basename: &'a str,
    pub key: &'a SecretString,
    /// One-based image number. When set, every error text starts with `#N `,
    /// including the previous error quoted inside a retry note.
    pub image_number: Option<usize>,
}

/// Input images with their multipart field names, plus an optional mask.
pub(super) struct ImageJobInputs<'a> {
    pub images: &'a [(&'a str, &'a ValidatedImage)],
    pub mask: Option<&'a ValidatedImage>,
}

pub(super) struct ImageJobResult {
    /// The saved image, or the error text of the last failed attempt.
    pub outcome: Result<SavedImage, String>,
    /// The model that answered the last attempt.
    pub model: String,
    /// Whether any attempt was answered by a compatible model ID.
    pub used_fallback: bool,
    /// Notes for this image: retry and save notes, then size, then format.
    pub notes: Vec<String>,
}

impl ToolEngine {
    pub(super) async fn image_job(&self, job: ImageJob<'_>) -> ImageJobResult {
        let error_prefix = job
            .image_number
            .map(|number| format!("#{number} "))
            .unwrap_or_default();
        let mut notes = Vec::new();
        let mut last_error: Option<String> = None;
        let mut saved = None;
        let mut actual_model = job.route.model().to_owned();
        let mut used_fallback = false;
        for (format_index, response_format) in job
            .route
            .response_formats(&self.config.response_formats_to_try)
            .enumerate()
        {
            if format_index > 0
                && let Some(response_format) = response_format
            {
                notes.push(format!(
                    "URL 落盘失败（{}）→ 重试 API response_format={response_format}",
                    last_error.as_deref().unwrap_or("保存失败")
                ));
            }
            let image_response = match &job.inputs {
                Some(inputs) => {
                    self.provider
                        .edit(
                            EditRequest {
                                model: &actual_model,
                                prompt: job.prompt,
                                size: job.size,
                                quality: job.quality,
                                response_format,
                                output_format: job.output_format,
                                images: inputs.images,
                                mask: inputs.mask,
                            },
                            job.key,
                            job.route.retry(),
                            &mut notes,
                        )
                        .await
                }
                None => {
                    self.provider
                        .generate(
                            GenerateRequest {
                                model: &actual_model,
                                prompt: job.prompt,
                                size: job.size,
                                quality: job.quality,
                                response_format,
                                output_format: job.output_format,
                                watermark: job.route.watermark(),
                            },
                            job.key,
                            job.route.retry(),
                            &mut notes,
                        )
                        .await
                }
            };
            // Each answer names the model for the next attempt.
            actual_model = image_response.model;
            used_fallback |= image_response.used_fallback;
            let response = image_response.response;
            if !response.is_success() {
                last_error = Some(format!(
                    "{error_prefix}HTTP {}: {}",
                    response.status,
                    error_detail(&response.body, &[job.key.expose_secret()])
                ));
                break;
            }
            match self
                .output
                .save_first(&response.body, job.location, job.basename)
                .await
            {
                Ok(image) => {
                    if format_index > 0
                        && let Some(response_format) = response_format
                    {
                        notes.push(format!("已通过 response_format={response_format} 成功落盘"));
                    }
                    saved = Some(image);
                    break;
                }
                Err(error) => last_error = Some(format!("{error_prefix}保存失败: {error}")),
            }
        }
        let outcome = match saved {
            Some(image) => {
                if let Some(note) = size_note(
                    job.size,
                    Some(image.actual_size),
                    job.output_format,
                    image.format,
                ) {
                    push_note_once(&mut notes, note);
                }
                if let Some(note) = output_format_note(job.output_format, image.format) {
                    push_note_once(&mut notes, note);
                }
                Ok(image)
            }
            None => Err(last_error.unwrap_or_else(|| format!("{error_prefix}保存失败"))),
        };
        ImageJobResult {
            outcome,
            model: actual_model,
            used_fallback,
            notes,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

    use serde_json::json;

    use crate::{
        domain::routing::RetryOptions,
        tools::test_support::{
            CapturedImage, Endpoint, Reply, ScriptedProvider, gray_1024x1024_png,
            models_and_formats, rgb_32x24_png, rgba_32x24_png, scripted, tool_engine,
        },
    };

    use super::*;

    const MODERN: &str = "gpt-image-2.6-flare";
    const LEGACY: &str = "gpt-image-2";
    const LEGACY_QUALITY: &str = "gpt-image-2-openai";
    const PROMPT: &str = "一只猫";
    const CONFIG_KEY: &str = "sk-config-key";
    const JOB_KEY: &str = "sk-job-key";
    const NO_NOTES: [&str; 0] = [];
    const NO_FILES: [&str; 0] = [];

    /// Save error for an answer that carries no image.
    const NO_IMAGE_ERROR: &str = "保存失败: 响应中未识别到图片";
    const RETRY_NOTE: &str =
        "URL 落盘失败（保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json";
    const SAVED_ON_RETRY_NOTE: &str = "已通过 response_format=b64_json 成功落盘";
    /// Size note for a 32x24 answer to a 1024x1024 request that asked for no GIF.
    const SIZE_NOTE: &str = "⚠ 实际 32×24 (0.00MP) ≠ 请求 1024×1024 (1.05MP)；米醋后端可能重映射自定义尺寸。需要精确像素时请使用 gpt-image-2-openai，并核对 saved.actual_size。";

    /// What a test varies about a job; the rest is fixed by `Fixture::run`.
    #[derive(Clone, Copy)]
    struct JobOptions<'a> {
        model: &'a str,
        output_format: Option<&'a str>,
        image_number: Option<usize>,
    }

    /// A modern-route job that asks for no format and carries no image number.
    const DEFAULT_JOB: JobOptions<'static> = JobOptions {
        model: MODERN,
        output_format: None,
        image_number: None,
    };

    fn upstream_error(status: u16, message: &str) -> Reply {
        Reply::json(status, json!({"error": {"message": message}}))
    }

    /// An image URL whose download is refused before any network access.
    fn refused_url() -> Reply {
        Reply::json(200, json!({"data": [{"url": "ftp://images.test/a.png"}]}))
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        engine: ToolEngine,
        provider: Arc<ScriptedProvider>,
    }

    impl Fixture {
        fn new(provider: ScriptedProvider) -> Self {
            Self::with_environment(provider, &[])
        }

        /// Outputs go to `<root>/out`, inputs come from `<root>/input`.
        fn with_environment(provider: ScriptedProvider, extra: &[(&str, &str)]) -> Self {
            let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
            let root = temp
                .path()
                .canonicalize()
                .unwrap_or_else(|error| panic!("{error}"));
            let input = root.join("input");
            std::fs::create_dir_all(&input).unwrap_or_else(|error| panic!("{error}"));
            let out = root.join("out").to_string_lossy().into_owned();
            let mut environment = BTreeMap::from([
                ("MICU_SAVE_DIR_ROOT".to_owned(), out.clone()),
                ("MICU_SAVE_DIR".to_owned(), out),
                (
                    "MICU_INPUT_ROOT".to_owned(),
                    input.to_string_lossy().into_owned(),
                ),
                ("MICU_API_KEY".to_owned(), CONFIG_KEY.to_owned()),
                ("MICU_BASEURL".to_owned(), "http://127.0.0.1:9".to_owned()),
            ]);
            environment.extend(
                extra
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
            );
            let provider = Arc::new(provider);
            let engine = tool_engine(&root, environment, provider.clone());
            Self {
                _temp: temp,
                root,
                engine,
                provider,
            }
        }

        /// Writes `<root>/input/<name>` and validates it as a tool would.
        fn input(&self, name: &str, bytes: &[u8]) -> ValidatedImage {
            let path = self.root.join("input").join(name);
            std::fs::write(&path, bytes).unwrap_or_else(|error| panic!("{error}"));
            self.engine
                .input_store
                .validate_image(&path.to_string_lossy(), "image_path")
                .unwrap_or_else(|error| panic!("{error}"))
        }

        /// Runs a text-to-image job.
        async fn run(&self, options: JobOptions<'_>) -> ImageJobResult {
            self.run_with(options, None).await
        }

        /// Runs a 1024x1024 job with quality `high`, basename `job` and `JOB_KEY`.
        async fn run_with(
            &self,
            options: JobOptions<'_>,
            inputs: Option<ImageJobInputs<'_>>,
        ) -> ImageJobResult {
            let (route, _) = ModelRoute::resolve(Some(options.model), MODERN, "1024x1024");
            let location = self
                .engine
                .output_store
                .resolve_save_dir(None)
                .unwrap_or_else(|error| panic!("{error}"));
            let key = SecretString::from(JOB_KEY.to_owned());
            self.engine
                .image_job(ImageJob {
                    route: &route,
                    prompt: PROMPT,
                    size: "1024x1024",
                    quality: Some("high"),
                    output_format: options.output_format,
                    inputs,
                    location: &location,
                    basename: "job",
                    key: &key,
                    image_number: options.image_number,
                })
                .await
        }

        /// Names of the files left in the output directory, sorted.
        fn output_files(&self) -> Vec<String> {
            let Ok(entries) = std::fs::read_dir(self.root.join("out")) else {
                return Vec::new();
            };
            let mut names = entries
                .map(|entry| {
                    entry
                        .unwrap_or_else(|error| panic!("{error}"))
                        .file_name()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
            names.sort();
            names
        }
    }

    fn expect_saved(result: ImageJobResult) -> SavedImage {
        result
            .outcome
            .unwrap_or_else(|error| panic!("expected a saved image, got error: {error}"))
    }

    fn expect_error(result: ImageJobResult) -> String {
        match result.outcome {
            Ok(image) => panic!("expected an error, got {image:?}"),
            Err(error) => error,
        }
    }

    #[tokio::test]
    async fn text_job_sends_one_generation_request_and_saves_the_answer() {
        let image = gray_1024x1024_png();
        let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&image)));
        let result = fixture
            .run(JobOptions {
                output_format: Some("png"),
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(result.model, MODERN);
        assert!(!result.used_fallback);
        assert_eq!(result.notes, NO_NOTES);
        let saved = expect_saved(result);
        assert_eq!(saved.path, fixture.root.join("out").join("job.png"));
        assert_eq!(saved.format, "png");
        assert_eq!(saved.actual_size, (1024, 1024));
        assert_eq!(std::fs::read(&saved.path).ok(), Some(image));
        assert_eq!(fixture.output_files(), ["job.png"]);

        let calls = fixture.provider.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].endpoint, Endpoint::Generate);
        assert_eq!(calls[0].model, MODERN);
        assert_eq!(calls[0].prompt, PROMPT);
        assert_eq!(calls[0].size, "1024x1024");
        assert_eq!(calls[0].quality.as_deref(), Some("high"));
        assert_eq!(calls[0].response_format, None);
        assert_eq!(calls[0].output_format.as_deref(), Some("png"));
        assert_eq!(calls[0].images, []);
        assert!(!calls[0].has_mask);
        assert_eq!(calls[0].key, JOB_KEY);
        assert_eq!(
            calls[0].retry,
            RetryOptions {
                enabled: false,
                big_size: false,
            }
        );
    }

    #[tokio::test]
    async fn edit_job_sends_the_input_image_and_mask_to_the_edit_endpoint() {
        let image = gray_1024x1024_png();
        let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(&image)));
        let source = fixture.input("source.png", &rgb_32x24_png());
        let mask = fixture.input("mask.png", &rgba_32x24_png());
        let result = fixture
            .run_with(
                JobOptions {
                    model: LEGACY_QUALITY,
                    ..DEFAULT_JOB
                },
                Some(ImageJobInputs {
                    images: &[("image", &source)],
                    mask: Some(&mask),
                }),
            )
            .await;

        let saved = expect_saved(result);
        assert_eq!(std::fs::read(&saved.path).ok(), Some(image));
        let calls = fixture.provider.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].endpoint, Endpoint::Edit);
        assert_eq!(calls[0].model, LEGACY_QUALITY);
        assert_eq!(calls[0].prompt, PROMPT);
        assert_eq!(calls[0].size, "1024x1024");
        assert_eq!(calls[0].quality.as_deref(), Some("high"));
        assert_eq!(calls[0].response_format.as_deref(), Some("url"));
        assert_eq!(calls[0].output_format, None);
        assert_eq!(
            calls[0].images,
            [CapturedImage {
                field: "image".into(),
                filename: "source.png".into(),
                mime: "image/png".into(),
                bytes: rgb_32x24_png(),
            }]
        );
        assert!(calls[0].has_mask);
        assert_eq!(calls[0].key, JOB_KEY);
        assert_eq!(
            calls[0].retry,
            RetryOptions {
                enabled: true,
                big_size: false,
            }
        );
    }

    #[tokio::test]
    async fn edit_job_keeps_the_field_name_and_order_of_each_input_image() {
        let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(
            &gray_1024x1024_png(),
        )));
        let first = fixture.input("ref-0.png", &rgb_32x24_png());
        let second = fixture.input("ref-1.png", &rgba_32x24_png());
        let result = fixture
            .run_with(
                DEFAULT_JOB,
                Some(ImageJobInputs {
                    images: &[("image[]", &first), ("image[]", &second)],
                    mask: None,
                }),
            )
            .await;

        expect_saved(result);
        let calls = fixture.provider.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].endpoint, Endpoint::Edit);
        assert_eq!(
            calls[0].images,
            [
                CapturedImage {
                    field: "image[]".into(),
                    filename: "ref-0.png".into(),
                    mime: "image/png".into(),
                    bytes: rgb_32x24_png(),
                },
                CapturedImage {
                    field: "image[]".into(),
                    filename: "ref-1.png".into(),
                    mime: "image/png".into(),
                    bytes: rgba_32x24_png(),
                },
            ]
        );
        assert!(!calls[0].has_mask);
    }

    #[tokio::test]
    async fn modern_routes_do_not_resend_the_request_after_a_save_failure() {
        for model in [
            "gpt-image-2.5-flare",
            "gpt-image-2.5-sunburst",
            "gpt-image-2.6-flare",
            "gpt-image-2.6-sunburst",
        ] {
            let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
            let result = fixture
                .run(JobOptions {
                    model,
                    ..DEFAULT_JOB
                })
                .await;

            assert_eq!(result.model, model);
            assert!(!result.used_fallback);
            assert_eq!(result.notes, NO_NOTES, "{model}");
            assert_eq!(expect_error(result), NO_IMAGE_ERROR, "{model}");
            assert_eq!(
                models_and_formats(&fixture.provider.calls()),
                [(model, None)]
            );
            assert_eq!(fixture.output_files(), NO_FILES, "{model}");
        }
    }

    #[tokio::test]
    async fn legacy_routes_try_each_configured_response_format_in_order() {
        for model in [LEGACY, LEGACY_QUALITY] {
            let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
            fixture
                .run(JobOptions {
                    model,
                    ..DEFAULT_JOB
                })
                .await;

            assert_eq!(
                models_and_formats(&fixture.provider.calls()),
                [(model, Some("url")), (model, Some("b64_json"))]
            );
        }
    }

    #[tokio::test]
    async fn legacy_route_sends_only_a_pinned_response_format() {
        for pinned in ["url", "b64_json"] {
            let fixture = Fixture::with_environment(
                ScriptedProvider::answering(Reply::no_image()),
                &[("MICU_RESPONSE_FORMAT", pinned)],
            );
            let result = fixture
                .run(JobOptions {
                    model: LEGACY,
                    ..DEFAULT_JOB
                })
                .await;

            assert_eq!(result.notes, NO_NOTES, "{pinned}");
            assert_eq!(expect_error(result), NO_IMAGE_ERROR, "{pinned}");
            assert_eq!(
                models_and_formats(&fixture.provider.calls()),
                [(LEGACY, Some(pinned))]
            );
        }
    }

    #[tokio::test]
    async fn upstream_error_ends_the_job_without_trying_the_next_format() {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(upstream_error(500, "boom"));
        fixture
            .provider
            .enqueue(Reply::image(&gray_1024x1024_png()));
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(result.model, LEGACY);
        assert_eq!(result.notes, NO_NOTES);
        assert_eq!(expect_error(result), "HTTP 500: boom");
        assert_eq!(
            models_and_formats(&fixture.provider.calls()),
            [(LEGACY, Some("url"))]
        );
        assert_eq!(fixture.output_files(), NO_FILES);
    }

    #[tokio::test]
    async fn save_failure_retries_with_the_next_format_and_notes_each_step() {
        let image = gray_1024x1024_png();
        let fixture = Fixture::new(scripted());
        fixture
            .provider
            .enqueue(Reply::no_image().with_note("第 1 次请求的提示"));
        fixture
            .provider
            .enqueue(Reply::image(&image).with_note("第 2 次请求的提示"));
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(
            result.notes,
            [
                "第 1 次请求的提示",
                RETRY_NOTE,
                "第 2 次请求的提示",
                SAVED_ON_RETRY_NOTE,
            ]
        );
        let saved = expect_saved(result);
        assert_eq!(saved.path, fixture.root.join("out").join("job.png"));
        assert_eq!(std::fs::read(&saved.path).ok(), Some(image));
        assert_eq!(
            models_and_formats(&fixture.provider.calls()),
            [(LEGACY, Some("url")), (LEGACY, Some("b64_json"))]
        );
    }

    #[tokio::test]
    async fn refused_image_url_is_retried_as_b64_json() {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(refused_url());
        fixture
            .provider
            .enqueue(Reply::image(&gray_1024x1024_png()));
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(
            result.notes,
            [
                "URL 落盘失败（保存失败: 下载 URL scheme 非法（仅允许 http/https）: 'ftp'）→ 重试 API response_format=b64_json",
                SAVED_ON_RETRY_NOTE,
            ]
        );
        expect_saved(result);
        assert_eq!(fixture.output_files(), ["job.png"]);
    }

    #[tokio::test]
    async fn every_format_failing_reports_the_last_save_error() {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(refused_url());
        fixture.provider.enqueue(Reply::no_image());
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(result.model, LEGACY);
        assert_eq!(
            result.notes,
            [
                "URL 落盘失败（保存失败: 下载 URL scheme 非法（仅允许 http/https）: 'ftp'）→ 重试 API response_format=b64_json"
            ]
        );
        assert_eq!(expect_error(result), NO_IMAGE_ERROR);
        assert_eq!(fixture.provider.calls().len(), 2);
        assert_eq!(fixture.output_files(), NO_FILES);
    }

    #[tokio::test]
    async fn no_configured_format_fails_with_the_generic_text_and_sends_nothing() {
        let mut fixture = Fixture::new(scripted());
        let mut config = (*fixture.engine.config).clone();
        config.response_formats_to_try.clear();
        fixture.engine.config = Arc::new(config);

        let unnumbered = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;
        assert_eq!(unnumbered.model, LEGACY);
        assert!(!unnumbered.used_fallback);
        assert_eq!(unnumbered.notes, NO_NOTES);
        assert_eq!(expect_error(unnumbered), "保存失败");

        let numbered = fixture
            .run(JobOptions {
                model: LEGACY,
                image_number: Some(3),
                ..DEFAULT_JOB
            })
            .await;
        assert_eq!(expect_error(numbered), "#3 保存失败");
        assert_eq!(fixture.provider.calls().len(), 0);
    }

    /// A legacy job whose first answer cannot be saved and whose second is HTTP 500.
    async fn save_failure_then_upstream_error(image_number: Option<usize>) -> ImageJobResult {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(Reply::no_image());
        fixture.provider.enqueue(upstream_error(500, "boom"));
        fixture
            .run(JobOptions {
                model: LEGACY,
                image_number,
                ..DEFAULT_JOB
            })
            .await
    }

    #[tokio::test]
    async fn image_number_prefixes_errors_and_the_error_quoted_in_the_retry_note() {
        let result = save_failure_then_upstream_error(Some(2)).await;
        assert_eq!(
            result.notes,
            ["URL 落盘失败（#2 保存失败: 响应中未识别到图片）→ 重试 API response_format=b64_json"]
        );
        assert_eq!(expect_error(result), "#2 HTTP 500: boom");

        let fixture = Fixture::new(ScriptedProvider::answering(Reply::no_image()));
        let result = fixture
            .run(JobOptions {
                image_number: Some(7),
                ..DEFAULT_JOB
            })
            .await;
        assert_eq!(expect_error(result), "#7 保存失败: 响应中未识别到图片");
    }

    #[tokio::test]
    async fn without_an_image_number_no_error_text_is_prefixed() {
        let result = save_failure_then_upstream_error(None).await;
        assert_eq!(result.notes, [RETRY_NOTE]);
        assert_eq!(expect_error(result), "HTTP 500: boom");
    }

    #[tokio::test]
    async fn each_answer_names_the_model_of_the_next_request_and_fallback_use_accumulates() {
        let fixture = Fixture::new(scripted());
        fixture
            .provider
            .enqueue(Reply::no_image().answered_by_fallback("gpt-image-2-compat"));
        // Echoes the requested model and reports no compatible ID of its own.
        fixture
            .provider
            .enqueue(Reply::image(&gray_1024x1024_png()));
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(
            models_and_formats(&fixture.provider.calls()),
            [
                (LEGACY, Some("url")),
                ("gpt-image-2-compat", Some("b64_json")),
            ]
        );
        assert_eq!(result.model, "gpt-image-2-compat");
        assert!(result.used_fallback);
        expect_saved(result);
    }

    #[tokio::test]
    async fn failed_job_reports_the_model_that_answered_last() {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(
            upstream_error(500, "boom").answered_by_fallback("gpt-image-2.5-flare-openai"),
        );
        let result = fixture
            .run(JobOptions {
                model: "gpt-image-2.5-flare",
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(result.model, "gpt-image-2.5-flare-openai");
        assert!(result.used_fallback);
        assert_eq!(expect_error(result), "HTTP 500: boom");
    }

    /// Notes of a modern-route job answered with `image` after asking for `output_format`.
    async fn notes_for(image: &[u8], output_format: Option<&str>) -> Vec<String> {
        let fixture = Fixture::new(ScriptedProvider::answering(Reply::image(image)));
        let result = fixture
            .run(JobOptions {
                output_format,
                ..DEFAULT_JOB
            })
            .await;
        if let Err(error) = &result.outcome {
            panic!("expected a saved image, got error: {error}");
        }
        result.notes
    }

    #[tokio::test]
    async fn size_mismatch_alone_adds_only_the_size_note() {
        assert_eq!(notes_for(&rgb_32x24_png(), Some("png")).await, [SIZE_NOTE]);
    }

    #[tokio::test]
    async fn format_mismatch_alone_adds_only_the_format_note() {
        assert_eq!(
            notes_for(&gray_1024x1024_png(), Some("jpeg")).await,
            ["⚠ 请求 output_format=jpeg，实际返回 png；以 saved[].format 为准"]
        );
    }

    #[tokio::test]
    async fn job_without_a_requested_format_adds_no_format_note() {
        assert_eq!(notes_for(&rgb_32x24_png(), None).await, [SIZE_NOTE]);
        assert_eq!(notes_for(&gray_1024x1024_png(), None).await, NO_NOTES);
    }

    #[tokio::test]
    async fn static_answer_to_a_gif_request_gets_gif_wording_in_both_notes() {
        assert_eq!(
            notes_for(&rgb_32x24_png(), Some("gif")).await,
            [
                "⚠ 实际 32×24 (0.00MP) ≠ 请求 1024×1024 (1.05MP)；涉及 GIF 时尺寸和宽高比不保证遵循请求，请以 saved[].actual_size 为准。",
                "⚠ 请求 output_format=gif，实际返回 png；以 saved[].format 为准；动图需要在 prompt 中明确要求动画",
            ]
        );
    }

    #[tokio::test]
    async fn size_note_then_format_note_follow_the_retry_notes() {
        let fixture = Fixture::new(scripted());
        fixture.provider.enqueue(Reply::no_image());
        fixture.provider.enqueue(Reply::image(&rgb_32x24_png()));
        let result = fixture
            .run(JobOptions {
                model: LEGACY,
                output_format: Some("webp"),
                ..DEFAULT_JOB
            })
            .await;

        assert_eq!(
            result.notes,
            [
                RETRY_NOTE,
                SAVED_ON_RETRY_NOTE,
                SIZE_NOTE,
                "⚠ 请求 output_format=webp，实际返回 png；以 saved[].format 为准",
            ]
        );
        expect_saved(result);
    }

    #[tokio::test]
    async fn error_summary_redacts_the_key_in_effect_for_the_job() {
        // The upstream echoes the key it was called with.
        let fixture = Fixture::new(ScriptedProvider::answering(upstream_error(
            401,
            &format!("invalid key {JOB_KEY}"),
        )));
        let result = fixture.run(DEFAULT_JOB).await;

        assert_eq!(expect_error(result), "HTTP 401: invalid key [REDACTED]");
        assert_eq!(fixture.provider.calls()[0].key, JOB_KEY);
    }
}
