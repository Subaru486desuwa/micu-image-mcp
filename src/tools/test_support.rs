//! Shared support for in-process tool tests: a scriptable `ImageProvider`
//! double, fixed-byte image fixtures and `ToolEngine` wiring.

use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret, SecretString};

use crate::{
    config::{Config, test_paths},
    fs::{output_store::OutputStore, response_output::OutputSaver},
    http::{
        client::{ApiResponse, HttpExecutor, RetryOptions},
        download::SystemResolver,
    },
    providers::{EditRequest, GenerateRequest, ImageProvider, ImageResponse},
};

use super::ToolEngine;

// Fixed bytes keep `size_bytes` in expected results a literal that does not
// depend on the PNG encoder version.
const RGB_32X24_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAYCAIAAAAUMWhjAAAAJElEQVR42mPQMLKhKWIYtWDUglELRi0YtWDUglELRi0YGhYAAFz6whBQMwL/AAAAAElFTkSuQmCC";
const RGBA_32X24_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAYCAYAAACbU/80AAAAGUlEQVR42u3BAQEAAACCIP+vbkhAAQAA7wYMGAAB93LuRQAAAABJRU5ErkJggg==";
const GRAY_1024X1024_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAABAAAAAQAAQAAAABXZhYuAAAAlklEQVR42u3BAQEAAACCIP+vbkhAAQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAADvBgQeAAEN3jhkAAAAAElFTkSuQmCC";

fn decode_fixture(encoded: &str) -> Vec<u8> {
    STANDARD
        .decode(encoded)
        .unwrap_or_else(|error| panic!("{error}"))
}

/// 93-byte opaque RGB PNG, 32x24.
pub(crate) fn rgb_32x24_png() -> Vec<u8> {
    decode_fixture(RGB_32X24_PNG)
}

/// 82-byte fully transparent RGBA PNG, 32x24; a valid mask for `rgb_32x24_png`.
pub(crate) fn rgba_32x24_png() -> Vec<u8> {
    decode_fixture(RGBA_32X24_PNG)
}

/// 207-byte grayscale PNG, 1024x1024; matches the default requested size.
pub(crate) fn gray_1024x1024_png() -> Vec<u8> {
    decode_fixture(GRAY_1024X1024_PNG)
}

/// Wires a `ToolEngine` around `provider`, with the real output saver and
/// stores configured from `environment`. `root` holds the stand-in home,
/// working directory and executable.
pub(crate) fn tool_engine(
    root: &Path,
    environment: BTreeMap<String, String>,
    provider: Arc<dyn ImageProvider>,
) -> ToolEngine {
    let config = Arc::new(Config::from_map(&environment).unwrap_or_else(|error| panic!("{error}")));
    let paths = Arc::new(test_paths(root, environment));
    let storage = OutputStore::new(paths.as_ref()).unwrap_or_else(|error| panic!("{error}"));
    let http = HttpExecutor::new(config.as_ref(), paths.as_ref())
        .unwrap_or_else(|error| panic!("{error}"));
    let output = OutputSaver::new(
        config.clone(),
        storage.clone(),
        http,
        Arc::new(SystemResolver),
    );
    ToolEngine::new(config, paths, storage, output, provider)
}

/// A provider whose every reply must be queued; an unscripted call fails loudly.
pub(crate) fn scripted() -> ScriptedProvider {
    ScriptedProvider::answering(Reply::raw(599, "unscripted provider call"))
}

/// Model and `response_format` of each call, in arrival order.
pub(crate) fn models_and_formats(calls: &[CapturedCall]) -> Vec<(&str, Option<&str>)> {
    calls
        .iter()
        .map(|call| (call.model.as_str(), call.response_format.as_deref()))
        .collect()
}

/// One scripted upstream answer.
#[derive(Clone)]
pub(crate) struct Reply {
    status: u16,
    body: Vec<u8>,
    model: Option<String>,
    used_fallback: bool,
    notes: Vec<String>,
    effect: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Reply {
    pub(crate) fn raw(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            body: body.into(),
            model: None,
            used_fallback: false,
            notes: Vec::new(),
            effect: None,
        }
    }

    pub(crate) fn json(status: u16, body: serde_json::Value) -> Self {
        Self::raw(
            status,
            serde_json::to_vec(&body).unwrap_or_else(|error| panic!("{error}")),
        )
    }

    /// HTTP 200 carrying `image` as `b64_json`.
    pub(crate) fn image(image: &[u8]) -> Self {
        Self::json(
            200,
            serde_json::json!({"data": [{"b64_json": STANDARD.encode(image)}]}),
        )
    }

    /// HTTP 200 without any image, so saving fails.
    pub(crate) fn no_image() -> Self {
        Self::json(200, serde_json::json!({"data": []}))
    }

    /// Reports `model` as the one that answered, through a compatible ID.
    /// Without this the reply echoes the requested model.
    pub(crate) fn answered_by_fallback(mut self, model: &str) -> Self {
        self.model = Some(model.to_owned());
        self.used_fallback = true;
        self
    }

    /// Appends `note` to the caller's notes, as the real provider does while retrying.
    pub(crate) fn with_note(mut self, note: &str) -> Self {
        self.notes.push(note.to_owned());
        self
    }

    /// Runs `effect` while the call is in flight, to simulate the world changing mid-request.
    pub(crate) fn with_effect(mut self, effect: impl Fn() + Send + Sync + 'static) -> Self {
        self.effect = Some(Arc::new(effect));
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Endpoint {
    Generate,
    Edit,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapturedImage {
    pub field: String,
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// What one provider call was asked to send upstream.
#[derive(Clone, Debug)]
pub(crate) struct CapturedCall {
    pub endpoint: Endpoint,
    pub model: String,
    pub prompt: String,
    pub size: String,
    pub quality: Option<String>,
    pub response_format: Option<String>,
    pub output_format: Option<String>,
    pub watermark: Option<bool>,
    pub images: Vec<CapturedImage>,
    pub has_mask: bool,
    pub key: String,
    pub retry: RetryOptions,
    pub started_at: Instant,
}

/// Answers calls from a queue of scripted replies, then from the default reply.
pub(crate) struct ScriptedProvider {
    script: Mutex<VecDeque<Reply>>,
    default_reply: Reply,
    endpoint_replies: Vec<(Endpoint, Reply)>,
    calls: Mutex<Vec<CapturedCall>>,
    delay: Duration,
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
}

impl ScriptedProvider {
    pub(crate) fn answering(default_reply: Reply) -> Self {
        Self {
            script: Mutex::new(VecDeque::new()),
            default_reply,
            endpoint_replies: Vec::new(),
            calls: Mutex::new(Vec::new()),
            delay: Duration::ZERO,
            in_flight: AtomicUsize::new(0),
            peak_in_flight: AtomicUsize::new(0),
        }
    }

    /// Answers every call to `endpoint` with `reply`. Such calls are still
    /// captured, but take nothing from the script.
    pub(crate) fn with_endpoint_reply(mut self, endpoint: Endpoint, reply: Reply) -> Self {
        self.endpoint_replies.push((endpoint, reply));
        self
    }

    /// Holds every call open for `delay`, so overlapping calls are observable.
    pub(crate) fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Queues `reply` for the next call that has no earlier queued reply.
    pub(crate) fn enqueue(&self, reply: Reply) {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(reply);
    }

    /// Calls in arrival order.
    pub(crate) fn calls(&self) -> Vec<CapturedCall> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    async fn answer(&self, call: CapturedCall, notes: &mut Vec<String>) -> ImageResponse {
        let requested_model = call.model.clone();
        let endpoint_reply = self
            .endpoint_replies
            .iter()
            .find(|(endpoint, _)| *endpoint == call.endpoint)
            .map(|(_, reply)| reply.clone());
        // Record and pick the reply before the first await, so arrival order decides both.
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(call);
        let reply = endpoint_reply.unwrap_or_else(|| {
            self.script
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
                .unwrap_or_else(|| self.default_reply.clone())
        });
        let in_flight = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(in_flight, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if let Some(effect) = &reply.effect {
            effect();
        }
        notes.extend(reply.notes);
        ImageResponse {
            response: ApiResponse {
                status: reply.status,
                headers: reqwest::header::HeaderMap::new(),
                body: reply.body,
            },
            model: reply.model.unwrap_or(requested_model),
            used_fallback: reply.used_fallback,
        }
    }
}

#[async_trait]
impl ImageProvider for ScriptedProvider {
    async fn generate(
        &self,
        request: GenerateRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ImageResponse {
        let call = CapturedCall {
            endpoint: Endpoint::Generate,
            model: request.model.to_owned(),
            prompt: request.prompt.to_owned(),
            size: request.size.to_owned(),
            quality: request.quality.map(str::to_owned),
            response_format: request.response_format.map(str::to_owned),
            output_format: request.output_format.map(str::to_owned),
            watermark: request.watermark,
            images: Vec::new(),
            has_mask: false,
            key: key.expose_secret().to_owned(),
            retry,
            started_at: Instant::now(),
        };
        self.answer(call, notes).await
    }

    async fn edit(
        &self,
        request: EditRequest<'_>,
        key: &SecretString,
        retry: RetryOptions,
        notes: &mut Vec<String>,
    ) -> ImageResponse {
        let call = CapturedCall {
            endpoint: Endpoint::Edit,
            model: request.model.to_owned(),
            prompt: request.prompt.to_owned(),
            size: request.size.to_owned(),
            quality: request.quality.map(str::to_owned),
            response_format: request.response_format.map(str::to_owned),
            output_format: request.output_format.map(str::to_owned),
            watermark: None,
            images: request
                .images
                .iter()
                .map(|(field, image)| CapturedImage {
                    field: (*field).to_owned(),
                    filename: image.filename.clone(),
                    mime: image.mime.to_owned(),
                    bytes: std::fs::read(image.file.path())
                        .unwrap_or_else(|error| panic!("{error}")),
                })
                .collect(),
            has_mask: request.mask.is_some(),
            key: key.expose_secret().to_owned(),
            retry,
            started_at: Instant::now(),
        };
        self.answer(call, notes).await
    }
}
